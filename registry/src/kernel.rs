//! `RegistryKernel`: boots `tock/assets/registry.jam` (M2 Task 2) in-process
//! and exposes mint/submit/leaderboard as async Rust methods.
//!
//! Poke/peek plumbing mirrors `Roswell` in
//! `nockchain/crates/roswell/src/lib.rs` (boot via `boot::setup`, poke via
//! `app.poke(SystemWire.to_wire(), slab)`) and the effect-parsing idiom in
//! `tock/src/miner.rs` (walk the effect list, match on the head tag via
//! `eq_bytes`).
use std::path::Path;

use nockapp::kernel::boot::{self, Cli as BootCli, NockStackSize};
use nockapp::noun::slab::NounSlab;
use nockapp::noun::AtomExt;
use nockapp::wire::{SystemWire, Wire};
use nockapp::{NockApp, NockAppError, NounAllocator};
use nockvm::noun::{Atom, Noun, NounSpace, D, T};
use noun_serde::{NounDecode, NounDecodeError};
use serde::Serialize;
use zkvm_jetpack::hot::produce_prover_hot_state;

/// One recorded run from the registry kernel's `runs` list.
///
/// `issued-at`/`submitted-at` are Hoon `@da` atoms (absolute-date atoms —
/// Urbit's epoch-relative fixed point timestamps). They can exceed 64 bits,
/// and `noun-serde`'s built-in `NounDecode` impls stop at `u64` (see
/// `nockchain/crates/noun-serde/src/lib.rs`, which has no `u128` impl), so
/// this struct hand-writes `NounDecode` below instead of deriving it — the
/// only change from the brief's draft. The Hoon kernel (`hoon/registry.hoon`)
/// is untouched; `@da` values still fit in 128 bits (`as_u64_pair` reads the
/// atom as two 64-bit little-endian limbs), so no truncation is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunRecord {
    pub id: u64,
    pub nonce: u64,
    pub hardware: String,
    pub prover_version: String,
    pub k: u64,
    pub elapsed_ms: u64,
    pub issued_at: u128,
    pub submitted_at: u128,
}

/// Decode an atom holding a value that may not fit in `u64` (e.g. `@da`) as
/// `u128`, via the two little-endian 64-bit limbs `as_u64_pair` exposes.
fn atom_to_u128(noun: Noun, space: &NounSpace) -> Result<u128, NounDecodeError> {
    let atom = noun
        .in_space(space)
        .as_atom()
        .map_err(|_| NounDecodeError::ExpectedAtom)?;
    let [lo, hi] = atom
        .as_u64_pair()
        .map_err(|_| NounDecodeError::Custom("atom too large for u128".into()))?;
    Ok(((hi as u128) << 64) | lo as u128)
}

impl NounDecode for RunRecord {
    fn from_noun(noun: &Noun, space: &NounSpace) -> Result<Self, NounDecodeError> {
        // run = [id nonce hardware prover-version k elapsed-ms issued-at submitted-at],
        // a right-nested 8-tuple (hoon/registry.hoon `+$run`).
        let c = noun
            .in_space(space)
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let id = c
            .head()
            .as_atom()
            .map_err(|_| NounDecodeError::ExpectedAtom)?
            .as_u64()
            .map_err(|_| NounDecodeError::Custom("id too large for u64".into()))?;

        let c = c
            .tail()
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let nonce = c
            .head()
            .as_atom()
            .map_err(|_| NounDecodeError::ExpectedAtom)?
            .as_u64()
            .map_err(|_| NounDecodeError::Custom("nonce too large for u64".into()))?;

        let c = c
            .tail()
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let hardware = c
            .head()
            .as_atom()
            .map_err(|_| NounDecodeError::ExpectedAtom)?
            .into_string()
            .map_err(|e| NounDecodeError::Custom(format!("non-utf8 hardware: {e}")))?;

        let c = c
            .tail()
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let prover_version = c
            .head()
            .as_atom()
            .map_err(|_| NounDecodeError::ExpectedAtom)?
            .into_string()
            .map_err(|e| NounDecodeError::Custom(format!("non-utf8 prover_version: {e}")))?;

        let c = c
            .tail()
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let k = c
            .head()
            .as_atom()
            .map_err(|_| NounDecodeError::ExpectedAtom)?
            .as_u64()
            .map_err(|_| NounDecodeError::Custom("k too large for u64".into()))?;

        let c = c
            .tail()
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let elapsed_ms = c
            .head()
            .as_atom()
            .map_err(|_| NounDecodeError::ExpectedAtom)?
            .as_u64()
            .map_err(|_| NounDecodeError::Custom("elapsed_ms too large for u64".into()))?;

        let c = c
            .tail()
            .as_cell()
            .map_err(|_| NounDecodeError::ExpectedCell)?;
        let issued_at = atom_to_u128(c.head().noun(), space)?;
        let submitted_at = atom_to_u128(c.tail().noun(), space)?;

        Ok(RunRecord {
            id,
            nonce,
            hardware,
            prover_version,
            k,
            elapsed_ms,
            issued_at,
            submitted_at,
        })
    }
}

/// Difference of two `@da` atoms (64.64 fixed-point seconds) in milliseconds.
/// The absolute Urbit epoch cancels out — only the difference is meaningful.
pub fn da_diff_to_ms(issued_at: u128, submitted_at: u128) -> u64 {
    let diff = submitted_at.saturating_sub(issued_at);
    // diff < 2^76 for windows under ~1 h, so ×1000 stays well inside u128.
    (diff.saturating_mul(1_000) >> 64) as u64
}

pub struct RegistryKernel {
    app: NockApp,
}

impl RegistryKernel {
    /// Boot the registry kernel jam at `jam_path` into a fresh (or existing)
    /// data directory at `data_dir`.
    pub async fn boot(jam_path: &Path, data_dir: &Path) -> Result<Self, NockAppError> {
        // boot::setup takes `jam: &[u8]` with no lifetime tied to the
        // returned NockApp, but the underlying SerfThread holds onto it for
        // the app's lifetime; leaking keeps it alive for the process, which
        // is fine for a test/CLI-lifetime kernel.
        let jam: &'static [u8] = Box::leak(
            std::fs::read(jam_path)
                .map_err(|e| NockAppError::OtherError(format!("read {}: {e}", jam_path.display())))?
                .into_boxed_slice(),
        );
        let mut cli = <BootCli as clap::Parser>::parse_from(["nockmark-registry"]);
        cli.data_dir = Some(data_dir.to_path_buf());
        cli.stack_size = NockStackSize::Medium;
        let app = boot::setup(jam, cli, &produce_prover_hot_state(), "registry", None)
            .await
            .map_err(|e| NockAppError::OtherError(format!("boot registry: {e}")))?;
        Ok(Self { app })
    }

    /// Poke `[%new-challenge ~]` and return the minted nonce.
    pub async fn mint_challenge(&mut self) -> Result<u64, NockAppError> {
        let mut slab: NounSlab = NounSlab::new();
        let tag = Atom::from_value(&mut slab, "new-challenge")
            .expect("tas")
            .as_noun();
        let cause = T(&mut slab, &[tag, D(0)]);
        slab.set_root(cause);
        let effects = self.app.poke(SystemWire.to_wire(), slab).await?;
        for eff in &effects {
            let space = eff.noun_space();
            let root = unsafe { *eff.root() }.in_space(&space);
            let Ok(cell) = root.as_cell() else {
                continue;
            };
            if cell.head().eq_bytes("challenge-minted") {
                return cell
                    .tail()
                    .as_atom()
                    .map_err(|_| NockAppError::OtherError("bad nonce atom".into()))?
                    .as_u64()
                    .map_err(|_| NockAppError::OtherError("nonce too large for u64".into()));
            }
        }
        Err(NockAppError::OtherError(
            "no challenge-minted effect".into(),
        ))
    }

    /// Poke `%submit-run`. `Ok(Ok(id))` = recorded, `Ok(Err(reason))` = rejected.
    pub async fn submit_run(
        &mut self,
        nonce: u64,
        hardware: &str,
        prover_version: &str,
        k: u64,
        elapsed_ms: u64,
    ) -> Result<Result<u64, String>, NockAppError> {
        let mut slab: NounSlab = NounSlab::new();
        let tag = Atom::from_value(&mut slab, "submit-run")
            .expect("tas")
            .as_noun();
        let hw = Atom::from_value(&mut slab, hardware)
            .expect("cord")
            .as_noun();
        let pv = Atom::from_value(&mut slab, prover_version)
            .expect("cord")
            .as_noun();
        // `nonce` is a full 64-bit atom minted from kernel entropy
        // (`(end 6 eny)` in hoon/registry.hoon) and can exceed DIRECT_MAX
        // (2^63-1), so it must go through `Atom::new` (direct-or-indirect)
        // rather than `D()`, which panics above DIRECT_MAX — see the same
        // concern documented in tock/src/miner.rs::belts_to_digest.
        let nonce_noun = Atom::new(&mut slab, nonce).as_noun();
        let cause = T(&mut slab, &[tag, nonce_noun, hw, pv, D(k), D(elapsed_ms)]);
        slab.set_root(cause);
        let effects = self.app.poke(SystemWire.to_wire(), slab).await?;
        for eff in &effects {
            let space = eff.noun_space();
            let root = unsafe { *eff.root() }.in_space(&space);
            let Ok(cell) = root.as_cell() else {
                continue;
            };
            if cell.head().eq_bytes("run-recorded") {
                let id = cell
                    .tail()
                    .as_atom()
                    .map_err(|_| NockAppError::OtherError("bad id atom".into()))?
                    .as_u64()
                    .map_err(|_| NockAppError::OtherError("id too large for u64".into()))?;
                return Ok(Ok(id));
            }
            if cell.head().eq_bytes("rejected") {
                let reason = cell
                    .tail()
                    .as_atom()
                    .map_err(|_| NockAppError::OtherError("bad reason atom".into()))?
                    .into_string()
                    .map_err(|_| NockAppError::OtherError("non-utf8 reason".into()))?;
                return Ok(Err(reason));
            }
        }
        Err(NockAppError::OtherError("no effect from submit-run".into()))
    }

    /// Peek `[%leaderboard ~]` and decode the run list.
    pub async fn leaderboard(&mut self) -> Result<Vec<RunRecord>, NockAppError> {
        let mut slab: NounSlab = NounSlab::new();
        let tag = Atom::from_value(&mut slab, "leaderboard")
            .expect("tas")
            .as_noun();
        let path = T(&mut slab, &[tag, D(0)]);
        slab.set_root(path);
        let res = self.app.peek(slab).await?;
        let space = res.noun_space();
        let root = unsafe { *res.root() };
        // peek returns (unit (unit *)) — unwrap two [0 …] layers (see
        // unwrap_peeked_value in nockchain/crates/roswell/src/lib.rs).
        let inner = {
            let c1 = root
                .in_space(&space)
                .as_cell()
                .map_err(|_| NockAppError::OtherError("peek: empty outer unit".into()))?;
            let c2 = c1
                .tail()
                .as_cell()
                .map_err(|_| NockAppError::OtherError("peek: empty inner unit".into()))?;
            c2.tail().noun()
        };
        Vec::<RunRecord>::from_noun(&inner, &space)
            .map_err(|e| NockAppError::OtherError(format!("decode leaderboard: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::da_diff_to_ms;

    #[test]
    fn da_diff_to_ms_converts_fixed_point_seconds() {
        let one_second: u128 = 1 << 64; // @dr: 64.64 fixed-point seconds
        assert_eq!(da_diff_to_ms(0, one_second), 1_000);
        assert_eq!(da_diff_to_ms(one_second, one_second * 43), 42_000);
        assert_eq!(da_diff_to_ms(0, one_second / 2), 500);
        // clock nonsense (submitted before issued) must not panic
        assert_eq!(da_diff_to_ms(one_second, 0), 0);
    }
}
