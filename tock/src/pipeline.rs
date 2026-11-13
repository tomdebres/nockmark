//! Prove while grinding (M7 step 5, part 2): the run loop both tock clients
//! share.
//!
//! Until this module, `zk-bench` and `ai-bench` ground all `k` wins, then
//! proved them one at a time while the grinder sat idle. The registry credits
//! `k × expected-attempts / server window`, and with the tier sized at
//! `R = 2` proofs' worth of grinding per win that window is `k·R·p + k·p`, so
//! the board recovered at most `R/(R+1)` = 2/3 of a machine's real rate (0.61
//! to 0.68 on the GPU rows of the ZK sweep, PR #36).
//!
//! Here the grinder hands each win to a pool of provers the moment it finds
//! it, and keeps grinding. The window becomes `k·R·p + p` (the last win's
//! proof) plus the upload, so recovery is `kR/(kR + 1)` ≈ 97 % at `k = 16`,
//! `R = 2`. The proof time `p` still cancels, so recovery stays the same on
//! every host. Upstream's own miners work this way: a device keeps searching
//! while a Serf proves.
//!
//! ## The contract
//!
//! * The grinder runs on the calling thread and gets a [`Sink`]. It
//!   [`Sink::offer`]s every win it finds, checks [`Sink::need_more`] (one
//!   atomic load) between attempts, and when that is false calls
//!   [`Sink::wait_for_more`], which blocks until either a proof failed (grind
//!   on) or the run is over (return).
//! * At most `k` wins are ever in hand (queued, proving or proved). A win
//!   offered beyond that is dropped, unproved: the run needs `k` wins and any
//!   `k` distinct wins earn the same credit.
//! * A failed proof (an `Err`, or a panic inside the prover) gives its slot
//!   back: the grinder finds a replacement. The failed win is never retried
//!   and never reaches the result, so nothing is submitted twice and nothing
//!   unproved is counted. More than `max_failures` failures abandon the run.
//! * [`Proving::Serial`] is the old client, kept for comparison: one prover,
//!   which starts only once all `k` wins are in hand.
//!
//! Nothing here is trusted by the registry. It verifies every win and times
//! the run itself, from mint to commit.

use std::collections::VecDeque;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How a run's proofs were scheduled. Sent with the submission as
/// `"proving"`, a label the board shows (the registry's `Proving`); it ranks
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proving {
    /// Prove each win while the grinder keeps going (the default).
    Pipelined,
    /// Grind all `k` wins, then prove them one at a time (the client before
    /// M7 step 5, part 2).
    Serial,
}

impl Proving {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pipelined => "pipelined",
            Self::Serial => "serial",
        }
    }
}

/// Stack for each prover thread. Upstream's AI provers run on their own
/// 16 MiB-stack rayon workers, and the ZK provers on the Serf's own thread, so
/// this only has to carry the calling frames; it is virtual memory.
const PROVER_STACK_BYTES: usize = 32 * 1024 * 1024;

/// How one run schedules its proofs.
#[derive(Clone, Copy, Debug)]
pub struct PipelineOptions {
    /// Wins the run needs.
    pub k: usize,
    /// Concurrent provers. Forced to 1 for [`Proving::Serial`].
    pub provers: usize,
    pub proving: Proving,
    /// Failed proofs tolerated before the run is abandoned.
    pub max_failures: usize,
}

impl PipelineOptions {
    /// `provers` concurrent provers (at least 1; exactly 1 when serial), and
    /// up to `k` failed proofs before giving up.
    pub fn new(k: usize, provers: usize, proving: Proving) -> Self {
        let provers = match proving {
            Proving::Serial => 1,
            Proving::Pipelined => provers.max(1),
        };
        Self {
            k,
            provers,
            proving,
            max_failures: k,
        }
    }
}

/// One proved win, with when it was found and proved (from the start of the
/// grind).
#[derive(Debug)]
pub struct Proved<W, P> {
    pub win: W,
    pub proof: P,
    pub found_at: Duration,
    pub started_at: Duration,
    pub done_at: Duration,
    /// Which prover proved it (`0..provers`).
    pub worker: usize,
}

impl<W, P> Proved<W, P> {
    /// When this win was found and proved, without the win and proof.
    pub fn timing(&self) -> WinTiming {
        WinTiming {
            found_at: self.found_at,
            started_at: self.started_at,
            done_at: self.done_at,
            worker: self.worker,
        }
    }
}

/// When one win was found and proved, from the start of the grind.
#[derive(Clone, Copy, Debug, Default)]
pub struct WinTiming {
    pub found_at: Duration,
    pub started_at: Duration,
    pub done_at: Duration,
    pub worker: usize,
}

/// What a run did.
#[derive(Debug)]
pub struct PipelineReport<W, P, G> {
    /// The grinder's own return value (its attempt count, typically).
    pub grind: G,
    /// Exactly `k` proved wins, in the order their proofs finished. Callers
    /// that submit in a canonical order sort these themselves.
    pub proved: Vec<Proved<W, P>>,
    /// Wins whose proof failed, with the reason. None of them is in `proved`.
    pub failures: Vec<(W, String)>,
    /// Wins offered while `k` were already in hand (never proved).
    pub dropped: u64,
    /// Grind start to the moment the last of the `proved` wins was found:
    /// the grind phase as the old client measured it.
    pub grind_elapsed: Duration,
    /// Grind start to the `k`-th finished proof.
    pub wall: Duration,
    pub options: PipelineOptions,
}

impl<W, P, G> PipelineReport<W, P, G> {
    /// `wall − grind_elapsed`: what proving added after the last win, which is
    /// one proof when the pool keeps up.
    pub fn tail(&self) -> Duration {
        self.wall.saturating_sub(self.grind_elapsed)
    }
}

struct Job<W> {
    win: W,
    found_at: Duration,
}

struct State<W, P> {
    queue: VecDeque<Job<W>>,
    /// Accepted and not failed: queued + proving + proved.
    in_hand: usize,
    proved: Vec<Proved<W, P>>,
    failures: Vec<(W, String)>,
    dropped: u64,
    grinder_done: bool,
    abort: Option<String>,
}

impl<W, P> State<W, P> {
    fn finished(&self, k: usize) -> bool {
        self.abort.is_some() || self.proved.len() >= k
    }
}

struct Shared<W, P> {
    opts: PipelineOptions,
    t0: Instant,
    state: Mutex<State<W, P>>,
    cv: Condvar,
    /// `in_hand < k` and the run is still live: the grinder's fast check.
    need_more: AtomicBool,
}

impl<W, P> Shared<W, P> {
    fn lock(&self) -> MutexGuard<'_, State<W, P>> {
        // A panic is caught before it can poison the lock (provers run under
        // catch_unwind, the grinder's panic is handled after it unwinds), but
        // a poisoned lock would still hold consistent counters: take it.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, g: MutexGuard<'a, State<W, P>>) -> MutexGuard<'a, State<W, P>> {
        self.cv.wait(g).unwrap_or_else(|e| e.into_inner())
    }

    /// Re-derive the fast flag from the state and wake everyone.
    fn publish(&self, st: &State<W, P>) {
        let more = !st.finished(self.opts.k) && st.in_hand < self.opts.k && !st.grinder_done;
        self.need_more.store(more, Ordering::Release);
        self.cv.notify_all();
    }
}

/// The grinder's handle on a running pipeline. `Sync`: a multi-threaded
/// grinder shares one `&Sink` across its threads.
pub struct Sink<'a, W, P> {
    shared: &'a Shared<W, P>,
}

impl<W, P> Sink<'_, W, P> {
    /// Does the run still want wins? One atomic load: call it between
    /// attempts. When it turns false, call [`Sink::wait_for_more`].
    pub fn need_more(&self) -> bool {
        self.shared.need_more.load(Ordering::Acquire)
    }

    /// Hand over a win. `true` if it was taken for proving; `false` if `k`
    /// were already in hand or the run is over (the win is dropped).
    pub fn offer(&self, win: W) -> bool {
        let found_at = self.shared.t0.elapsed();
        let mut st = self.shared.lock();
        if st.finished(self.shared.opts.k) || st.in_hand >= self.shared.opts.k {
            st.dropped += 1;
            return false;
        }
        st.in_hand += 1;
        st.queue.push_back(Job { win, found_at });
        self.shared.publish(&st);
        true
    }

    /// Block until the run needs another win (`true`: grind on) or is over
    /// (`false`: return). Returns at once when a win is wanted now.
    pub fn wait_for_more(&self) -> bool {
        let mut st = self.shared.lock();
        loop {
            if st.finished(self.shared.opts.k) {
                return false;
            }
            if st.in_hand < self.shared.opts.k {
                return true;
            }
            st = self.shared.wait(st);
        }
    }

    /// Time since the grind started.
    pub fn elapsed(&self) -> Duration {
        self.shared.t0.elapsed()
    }
}

/// Run one grind-and-prove pipeline to `opts.k` proved wins.
///
/// `grind` runs on this thread and returns when [`Sink::wait_for_more`] says
/// the run is over (or when it runs out of attempts, which ends the run
/// short). `prove(worker, &win)` runs on `opts.provers` threads of its own.
///
/// `Err` if the run ended without `k` proved wins: too many failed proofs, or
/// the grinder stopped early. A panic in `grind` stops the provers and is
/// re-raised here; a panic in `prove` counts as a failed proof.
pub fn run<W, P, G, FG, FP>(
    opts: PipelineOptions,
    grind: FG,
    prove: FP,
) -> Result<PipelineReport<W, P, G>, String>
where
    W: Send,
    P: Send,
    FG: FnOnce(&Sink<'_, W, P>) -> G,
    FP: Fn(usize, &W) -> Result<P, String> + Sync,
{
    assert!(opts.k >= 1, "k must be at least 1");
    let provers = match opts.proving {
        Proving::Serial => 1,
        Proving::Pipelined => opts.provers.max(1),
    };
    let shared = Shared {
        opts,
        t0: Instant::now(),
        state: Mutex::new(State {
            queue: VecDeque::new(),
            in_hand: 0,
            proved: Vec::with_capacity(opts.k),
            failures: Vec::new(),
            dropped: 0,
            grinder_done: false,
            abort: None,
        }),
        cv: Condvar::new(),
        need_more: AtomicBool::new(true),
    };
    let serial = opts.proving == Proving::Serial;

    let grind_out = std::thread::scope(|s| {
        for worker in 0..provers {
            let (shared, prove) = (&shared, &prove);
            std::thread::Builder::new()
                .name(format!("tock-prover-{worker}"))
                .stack_size(PROVER_STACK_BYTES)
                .spawn_scoped(s, move || prover_loop(shared, worker, serial, prove))
                .expect("spawn prover thread");
        }
        let sink = Sink { shared: &shared };
        let out = catch_unwind(AssertUnwindSafe(|| grind(&sink)));
        let mut st = shared.lock();
        st.grinder_done = true;
        if out.is_err() && st.abort.is_none() {
            st.abort = Some("the grinder panicked".into());
        }
        shared.publish(&st);
        drop(st);
        out
    });
    let grind = match grind_out {
        Ok(g) => g,
        Err(panic) => resume_unwind(panic),
    };

    let st = shared.state.into_inner().unwrap_or_else(|e| e.into_inner());
    if let Some(reason) = st.abort {
        return Err(reason);
    }
    if st.proved.len() < opts.k {
        return Err(format!(
            "the grinder stopped with {} of {} wins proved ({} failed)",
            st.proved.len(),
            opts.k,
            st.failures.len()
        ));
    }
    let grind_elapsed = st.proved.iter().map(|p| p.found_at).max().unwrap_or_default();
    let wall = st.proved.iter().map(|p| p.done_at).max().unwrap_or_default();
    Ok(PipelineReport {
        grind,
        proved: st.proved,
        failures: st.failures,
        dropped: st.dropped,
        grind_elapsed,
        wall,
        options: PipelineOptions { provers, ..opts },
    })
}

fn prover_loop<W, P, FP>(shared: &Shared<W, P>, worker: usize, serial: bool, prove: &FP)
where
    FP: Fn(usize, &W) -> Result<P, String>,
{
    let k = shared.opts.k;
    loop {
        let job = {
            let mut st = shared.lock();
            loop {
                if st.finished(k) {
                    return;
                }
                // Serial: proving starts only once all k wins are in hand, or
                // once the grinder has stopped (a run ending short must drain,
                // not wait for wins that will never come).
                if !serial || st.in_hand >= k || st.grinder_done {
                    if let Some(job) = st.queue.pop_front() {
                        break job;
                    }
                }
                if st.grinder_done && st.queue.is_empty() {
                    return;
                }
                st = shared.wait(st);
            }
        };
        let started_at = shared.t0.elapsed();
        let result = catch_unwind(AssertUnwindSafe(|| prove(worker, &job.win)));
        let done_at = shared.t0.elapsed();
        let mut st = shared.lock();
        match result {
            Ok(Ok(proof)) if st.proved.len() < k => st.proved.push(Proved {
                win: job.win,
                proof,
                found_at: job.found_at,
                started_at,
                done_at,
                worker,
            }),
            // Unreachable while in_hand ≤ k; kept so a counting slip can only
            // drop a surplus proof, never submit k + 1.
            Ok(Ok(_)) => st.dropped += 1,
            Ok(Err(reason)) => fail(&mut st, shared.opts.max_failures, job.win, reason),
            Err(panic) => {
                let reason = panic_message(&panic);
                fail(&mut st, shared.opts.max_failures, job.win, reason)
            }
        }
        shared.publish(&st);
    }
}

fn fail<W, P>(st: &mut State<W, P>, max_failures: usize, win: W, reason: String) {
    st.in_hand -= 1;
    st.failures.push((win, reason));
    if st.failures.len() > max_failures && st.abort.is_none() {
        st.abort = Some(format!(
            "{} proofs failed (more than {max_failures}); last: {}",
            st.failures.len(),
            st.failures.last().map_or("", |f| f.1.as_str())
        ));
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        format!("prover panicked: {s}")
    } else if let Some(s) = panic.downcast_ref::<String>() {
        format!("prover panicked: {s}")
    } else {
        "prover panicked".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    /// Run `f` on its own thread and fail the test if it has not returned
    /// within `secs`: the bugs these tests exist for (a slot never given
    /// back, a run never abandoned) hang rather than return a wrong answer.
    fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
        });
        match rx.recv_timeout(Duration::from_secs(secs)) {
            Ok(Ok(v)) => v,
            Ok(Err(panic)) => resume_unwind(panic),
            Err(_) => panic!("the pipeline hung for {secs} s"),
        }
    }

    /// A grinder that finds win `i` every `gap`, walking 0, 1, 2, … like the
    /// real ones, until the run says stop.
    fn ticking_grinder(gap: Duration) -> impl FnOnce(&Sink<'_, u64, u64>) -> u64 {
        move |sink| {
            let mut next = 0u64;
            loop {
                if !sink.need_more() && !sink.wait_for_more() {
                    return next;
                }
                std::thread::sleep(gap);
                sink.offer(next);
                next += 1;
            }
        }
    }

    fn sleeping_prover(p: Duration) -> impl Fn(usize, &u64) -> Result<u64, String> + Sync {
        move |_, w| {
            std::thread::sleep(p);
            Ok(*w * 10)
        }
    }

    /// The point of the module: pipelined, the run is the grind plus about
    /// ONE proof; serial, the grind plus k proofs.
    #[test]
    fn pipelining_overlaps_proving_with_grinding() {
        let (k, gap, p) = (8usize, Duration::from_millis(60), Duration::from_millis(40));
        let piped = run(
            PipelineOptions::new(k, 2, Proving::Pipelined),
            ticking_grinder(gap),
            sleeping_prover(p),
        )
        .unwrap();
        let serial = run(
            PipelineOptions::new(k, 2, Proving::Serial),
            ticking_grinder(gap),
            sleeping_prover(p),
        )
        .unwrap();
        let grind = gap * k as u32;
        // Structural checks first: they hold however loaded the machine is
        // (sleeps only ever run long).
        let started_after_grind = |r: &PipelineReport<u64, u64, u64>| {
            r.proved.iter().filter(|x| x.started_at >= r.grind_elapsed).count()
        };
        // Pipelined: proofs ran during the grind, and at most one per prover
        // was left for after it. That tail is the "+ p" in k·R·p + p.
        assert!(piped.proved.iter().any(|x| x.started_at < piped.grind_elapsed / 2));
        assert!(started_after_grind(&piped) <= 2, "tail proofs: {}", started_after_grind(&piped));
        assert!(piped.wall >= grind + p, "{:?}", piped.wall);
        // Serial: no proof starts before the k-th win is found, so all k come
        // after the grind, one at a time.
        assert_eq!(started_after_grind(&serial), k);
        assert!(serial.tail() >= p * k as u32, "serial tail {:?}", serial.tail());
        assert_eq!(serial.options.provers, 1, "serial forces one prover");
        // And the point of it all: the pipelined run is shorter by most of
        // the serial tail.
        assert!(
            piped.wall + p * (k as u32 - 3) < serial.wall,
            "pipelined {:?} vs serial {:?}",
            piped.wall,
            serial.wall
        );
    }

    /// A failed proof gives its slot back: the grinder finds a replacement, the
    /// failed win is neither retried nor submitted, and no win appears twice.
    #[test]
    fn a_failed_proof_is_replaced_not_lost_or_duplicated() {
        let attempts = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&attempts);
        let r = within(20, move || {
            run(
                PipelineOptions::new(5, 2, Proving::Pipelined),
                ticking_grinder(Duration::from_millis(3)),
                move |_, w: &u64| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(4));
                    if *w == 2 || *w == 3 {
                        Err(format!("win {w} does not prove"))
                    } else {
                        Ok(*w)
                    }
                },
            )
        })
        .unwrap();
        let proved: Vec<u64> = {
            let mut v: Vec<u64> = r.proved.iter().map(|x| x.win).collect();
            v.sort();
            v
        };
        assert_eq!(proved, vec![0, 1, 4, 5, 6], "failed wins replaced by the next ones");
        let failed: Vec<u64> = r.failures.iter().map(|f| f.0).collect();
        assert_eq!(failed.len(), 2);
        assert!(failed.contains(&2) && failed.contains(&3));
        assert!(r.failures.iter().all(|f| f.1.contains("does not prove")));
        assert_eq!(attempts.load(Ordering::Relaxed), 7, "each win proved at most once");
        assert_eq!(r.grind, 7, "the grinder stopped once 5 were proved");
    }

    /// A panic inside the prover is a failed proof, not a hung or crashed run.
    #[test]
    fn a_prover_panic_counts_as_a_failure() {
        let r = within(20, || {
            run(
                PipelineOptions::new(3, 2, Proving::Pipelined),
                ticking_grinder(Duration::from_millis(2)),
                |_, w: &u64| {
                    if *w == 1 {
                        panic!("upstream expect() on win {w}");
                    }
                    Ok(*w * 10)
                },
            )
        })
        .unwrap();
        assert!(r.proved.iter().all(|x| x.proof == x.win * 10), "each proof is its win's");
        assert_eq!(r.proved.len(), 3);
        assert!(r.proved.iter().all(|x| x.win != 1));
        assert_eq!(r.failures.len(), 1);
        assert!(r.failures[0].1.contains("panicked"), "{}", r.failures[0].1);
    }

    /// Proofs that keep failing end the run with an error instead of grinding
    /// forever.
    #[test]
    fn persistent_failures_abandon_the_run() {
        let err = within(20, || {
            run(
                PipelineOptions::new(4, 2, Proving::Pipelined),
                ticking_grinder(Duration::from_millis(1)),
                |_, _: &u64| Err::<u64, _>("kernel refuses everything".to_string()),
            )
        })
        .unwrap_err();
        assert!(err.contains("5 proofs failed"), "{err}");
        assert!(err.contains("kernel refuses everything"), "{err}");
    }

    /// A grinder panic stops the provers and is re-raised, rather than leaving
    /// prover threads waiting for wins that never come.
    #[test]
    fn a_grinder_panic_is_re_raised_without_hanging() {
        let res = within(20, || {
            catch_unwind(|| {
                run(
                    PipelineOptions::new(3, 2, Proving::Pipelined),
                    |sink: &Sink<'_, u64, u64>| -> u64 {
                        sink.offer(0);
                        panic!("device disagrees with the CPU oracle");
                    },
                    sleeping_prover(Duration::from_millis(20)),
                )
                .map(|r| r.proved.len())
            })
        });
        let panic = res.unwrap_err();
        assert!(panic_message(&panic).contains("oracle"));
    }

    /// A grinder that runs out of attempts ends the run short, with an error,
    /// in either mode. (Serial used to hang here: its prover waited for k
    /// wins that would never come. Adversarial review, 2026-11-13.)
    #[test]
    fn a_grinder_that_stops_early_is_an_error() {
        for proving in [Proving::Pipelined, Proving::Serial] {
            a_grinder_that_stops_early_is_an_error_in(proving);
        }
    }

    fn a_grinder_that_stops_early_is_an_error_in(proving: Proving) {
        let err = within(20, move || {
            run(
                PipelineOptions::new(4, 1, proving),
                |sink: &Sink<'_, u64, u64>| {
                    sink.offer(0);
                    sink.offer(1);
                    2u64
                },
                sleeping_prover(Duration::from_millis(1)),
            )
            .map(|r| r.proved.len())
        })
        .unwrap_err();
        assert!(err.contains("2 of 4"), "{err}");
    }

    /// A grinder that offers more than k wins gets the first k taken and the
    /// rest refused: nothing beyond k is ever queued for proving.
    #[test]
    fn offers_beyond_k_are_refused() {
        let r = within(20, || {
            run(
                PipelineOptions::new(4, 2, Proving::Pipelined),
                |sink: &Sink<'_, u64, u64>| (0..10u64).map(|i| sink.offer(i)).collect::<Vec<bool>>(),
                sleeping_prover(Duration::from_millis(5)),
            )
            .map(|r| (r.grind, r.dropped, r.proved.len()))
        })
        .unwrap();
        assert_eq!(r.0, [true, true, true, true, false, false, false, false, false, false]);
        assert_eq!((r.1, r.2), (6, 4));
    }

    /// Several grind threads winning at once never put more than k wins in
    /// hand: the surplus is dropped, unproved.
    #[test]
    fn concurrent_offers_never_exceed_k() {
        let proved_count = AtomicU64::new(0);
        let r = run(
            PipelineOptions::new(4, 2, Proving::Pipelined),
            |sink: &Sink<'_, u64, u64>| {
                std::thread::scope(|s| {
                    for t in 0..8u64 {
                        s.spawn(move || {
                            let mut i = t;
                            loop {
                                if !sink.need_more() && !sink.wait_for_more() {
                                    return;
                                }
                                sink.offer(i);
                                i += 8;
                            }
                        });
                    }
                });
            },
            |_, w: &u64| {
                proved_count.fetch_add(1, Ordering::Relaxed);
                Ok(*w)
            },
        )
        .unwrap();
        assert_eq!(r.proved.len(), 4);
        assert_eq!(proved_count.load(Ordering::Relaxed), 4, "nothing beyond k was proved");
        let wins: HashSet<u64> = r.proved.iter().map(|x| x.win).collect();
        assert_eq!(wins.len(), 4);
    }
}
