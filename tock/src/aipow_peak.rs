//! AI-PoW track, **dense-peak statement** (M7 step 4): mainnet's production AI
//! work, the dense Pearl V3 shape upstream's peak CUDA kernel grinds (#159,
//! `ai_pow_miner::PEAK_PRODUCTION_PARAMS`: m=4096 k=8192 n=32768 r=512, 16×16
//! tiles).
//!
//! ## How consensus counts it
//!
//! One extranonce fixes a Pearl transcript (header → `kappa` → matrix
//! commitments → noise → noised `A'`, `B'`). Every 16×16 output tile of
//! `A'·B'` is then one jackpot ATTEMPT (a "ticket"), so one extranonce holds
//! `(m/16)·(n/16)` = 2^19 tickets. Consensus prices each at
//! `F = h·w·dot_product_length = 16·16·8192 = 2^21` MAC-equivalents, read from
//! the statement's own mining config (`PearlMiningConfig::shape_work_factor`),
//! and a ticket wins when `jackpot ≤ T·F` — the SCALED comparison, as on the
//! canonical-MoE path.
//!
//! The attempt index this benchmark speaks ([`AI_PEAK_NONCE_RULE`]) is
//! `extranonce · 2^19 + ordinal`, the ordinal being upstream's lexicographic
//! tile order (`row_tile · 2048 + col_tile`). Winning indices are strictly
//! ascending because the grind walks them in order.
//!
//! ## Upstream paths used, unchanged
//!
//! * Template and transcript: [`PreparedReferenceDenseTemplate`] (the gateway-free
//!   miner's own dense template; matrices from `AI_POW_PROD_SYNTH_SEED`).
//! * CPU grind: [`CpuSearchBackend::search_dense`] over
//!   `template.prepare(extranonce)` — upstream's CPU search, one worker per
//!   physical core.
//! * GPU grind (`gpu` feature): `ai_pow_miner::peak::MultiGpuPeakSearchBackend::
//!   search_peak`, the #159 kernel, with upstream's scalar recheck
//!   (`checked_search_winner` + transcript/jackpot comparison).
//! * Proof: `prove_with_rules(.., Hardened)` for the first win, then the same
//!   prover with the first run's reusable setup
//!   (`prove_pearl_merge_compact_recursive_certificate_checked_with_prover_cache_with_rules`).
//!   Measured on an M1 Max: 72–76 s cold, 61–64 s warm, 12–15 GB peak RSS.
//! * Wire format: the `%ai-pow` block artifact exactly as a node receives it,
//!   jammed — the registry decodes and verifies it with the consensus jet's own
//!   entry point.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_pow::difficulty::{
    effective_jackpot_threshold, shape_work_factor_for, AI_POW_MAX_CONSENSUS_TARGET,
};
use ai_pow::params::MatmulParams;
use ai_pow::pearl_compat::{
    PearlAuxInclusionProof, PearlIncompleteBlockHeader, PearlMergeCheckedTicketAttempt,
    PearlMiningConfig, PearlPeriodicPattern, PEARL_MINING_CONFIG_RESERVED_SIZE,
    PEARL_MMA_INT7XINT7_TO_INT32,
};
use ai_pow::synth::{synth_matrices, AI_POW_PROD_SYNTH_SEED};
use ai_pow::zk_bridge::{
    prove_pearl_merge_compact_recursive_certificate_checked_with_prover_cache_with_rules,
    AiPowCompactRecursiveCertificateRun, AiPowCompactRecursiveProverCache,
};
use ai_pow_miner::reference::{PreparedReferenceDenseTemplate, PreparedReferenceMoeTemplate};
use ai_pow_miner::run::build_ai_pow_pearl_merge_certificate_poke_from_ticket_compact_recursive_run;
use ai_pow_miner::search::{CpuSearchBackend, SearchBackend, SearchBatch};
use ai_pow_zk::proof_rules::ProofRules;
use nockapp::noun::slab::NounSlab;
use nockvm::noun::NounAllocator;

// ---------------------------------------------------------------------------
// Statement constants
// ---------------------------------------------------------------------------

/// Statement discriminator on the wire, the board and the store.
pub const STATEMENT_DENSE_PEAK: &str = "dense-peak";

/// The shape: upstream's constant itself, not a copy.
pub const AI_PEAK_PARAMS: MatmulParams = ai_pow_miner::PEAK_PRODUCTION_PARAMS;

/// Attempt-index rule: `index = extranonce · 2^19 + ordinal`, where the
/// extranonce (u32) offsets the synthetic Pearl header timestamp
/// (`PreparedReferenceDenseTemplate::header_for`) and the ordinal is upstream's
/// lexicographic tile order within that transcript. Versioned; the registry
/// advertises it on the challenge and clients must match it.
pub const AI_PEAK_NONCE_RULE: &str = "peak-extranonce-tile-v1";

/// Tickets (16×16 tiles) per extranonce: `(m/16)·(n/16)` = 256 · 2048 = 2^19.
/// Pinned against upstream's `prepare_search().total_tickets()` by a test.
pub const PEAK_TICKETS_PER_EXTRANONCE: u64 = 1 << 19;

/// Tile columns per row of tiles: `n/16` = 2048.
pub const PEAK_TILE_COLS: u64 = (AI_PEAK_PARAMS.n / AI_PEAK_PARAMS.tile) as u64;

/// MAC-equivalents per dense-peak attempt: `F = 16 · 16 · 8192 = 2^21`. The
/// `f64` mirror of [`peak_shape_work_factor`]; tests pin both against the
/// mining config the statement actually carries.
pub const MAC_EQUIV_PER_PEAK_ATTEMPT: f64 = 2_097_152.0;

/// `max_pattern_len` the verifier passes for this shape: the tile side, as
/// upstream's peak miner passes `PEAK_PRODUCTION_PARAMS.tile`.
pub const AI_PEAK_MAX_PATTERN_LEN: usize = AI_PEAK_PARAMS.tile as usize;

/// Dense-peak tier floor (expected attempts per win), as on the other AI
/// statements.
pub const AI_PEAK_ATTEMPTS_MIN: u64 = 1 << 12;

/// Dense-peak tier ceiling. Higher than the other AI statements' 2^30: the
/// #159 kernel grinds ~1.6e8 tiles/s on a 5090, so R = 2 against a ~100 s CPU
/// prove asks for ~2^35 per win. 16 wins at 2^36 are ~2 h of grinding on a
/// 5090, inside the 3 h [`AI_PEAK_WINDOW_MS`].
pub const AI_PEAK_ATTEMPTS_MAX: u64 = 1 << 36;

/// How long a dense-peak challenge stays valid: 3 h (the other AI statements
/// keep 1 h). One certificate costs 60–76 s on an M1 Max and more on a typical
/// pod CPU, so 16 wins at R = 2 (≈ 48 proofs' worth of time) do not fit 1 h.
/// Tom chose this over k = 8 or R = 1 on 2026-11-12, so dense-peak rows keep
/// the same recovery (2/3) and noise (±25%) as every other row.
pub const AI_PEAK_WINDOW_MS: u64 = 3 * 3_600_000;

/// Per-win blob cap: the artifact jam measured 147.1–147.2 KB. 16 wins at
/// this cap, base64'd, are 3.5 MB — inside the registry's 4 MiB body limit.
pub const AI_PEAK_SUBMISSION_BLOB_MAX: usize = 160 * 1024;

/// `F` from the crate's own difficulty functions for this shape.
pub fn peak_shape_work_factor() -> u128 {
    shape_work_factor_for(
        AI_PEAK_PARAMS.tile,
        AI_PEAK_PARAMS.tile,
        AI_PEAK_PARAMS.k,
        AI_PEAK_PARAMS.noise_rank,
    )
    .expect("the dense-peak shape is admissible")
}

/// The mining config every dense-peak statement carries — upstream builds it
/// in `PreparedReferenceDenseTemplate::new` from a private `setup_pattern`, so
/// it is restated here and pinned against `template.config()` by a test. The
/// registry rejects any other config.
pub fn peak_mining_config() -> PearlMiningConfig {
    let pattern = PearlPeriodicPattern {
        shape: [
            (1, AI_PEAK_PARAMS.tile),
            (AI_PEAK_PARAMS.tile, 1),
            (AI_PEAK_PARAMS.tile, 1),
        ],
    };
    PearlMiningConfig {
        common_dim: AI_PEAK_PARAMS.k,
        rank: AI_PEAK_PARAMS.noise_rank as u16,
        mma_type: PEARL_MMA_INT7XINT7_TO_INT32,
        rows_pattern: pattern,
        cols_pattern: pattern,
        reserved: [0; PEARL_MINING_CONFIG_RESERVED_SIZE],
    }
}

/// The effective jackpot threshold `Θ = T · F`, fail-closed upstream.
pub fn peak_threshold(target: &[u8; 32]) -> Result<[u8; 32], String> {
    effective_jackpot_threshold(target, peak_shape_work_factor())
        .map_err(|e| format!("target outside the dense-peak domain: {e:?}"))
}

/// Expected attempts per win at registry target `T`: `2^256 / (T·F + 1)`.
pub fn expected_attempts_per_peak_win(target: &[u8; 32]) -> Result<f64, String> {
    let theta = peak_threshold(target)?;
    let mut t = 0.0f64;
    for &b in theta.iter().rev() {
        t = t * 256.0 + b as f64;
    }
    Ok(2f64.powi(256) / (t + 1.0))
}

/// The loosest target used locally: consensus's own maximum
/// (`AI_POW_MAX_CONSENSUS_TARGET`, 2^232 − 1), at which 1 ticket in 8 wins.
pub fn max_peak_target() -> [u8; 32] {
    AI_POW_MAX_CONSENSUS_TARGET
}

/// Clamp-and-round a requested dense-peak tier to `[2^12, 2^36]`.
pub fn grant_peak_attempts(requested: u64) -> u64 {
    crate::aipow::grant_pow2_tier(requested, AI_PEAK_ATTEMPTS_MIN, AI_PEAK_ATTEMPTS_MAX)
}

/// The single-bit target realizing a granted dense-peak tier.
pub fn peak_target_for_attempts(attempts: u64) -> Option<[u8; 32]> {
    crate::aipow::target_for_granted_tier(attempts, grant_peak_attempts, |t| {
        expected_attempts_per_peak_win(t).unwrap_or(1.0)
    })
}

/// `extranonce · 2^19 + ordinal`.
pub fn attempt_index(extranonce: u32, ordinal: u64) -> u64 {
    debug_assert!(ordinal < PEAK_TICKETS_PER_EXTRANONCE);
    u64::from(extranonce) * PEAK_TICKETS_PER_EXTRANONCE + ordinal
}

/// Inverse of [`attempt_index`]; `None` when the extranonce does not fit u32.
pub fn split_attempt_index(index: u64) -> Option<(u32, u64)> {
    let extranonce = u32::try_from(index / PEAK_TICKETS_PER_EXTRANONCE).ok()?;
    Some((extranonce, index % PEAK_TICKETS_PER_EXTRANONCE))
}

/// The tile (`t_rows`, `t_cols`) an ordinal names, in upstream's order
/// (`PreparedReferenceDenseSearch::offsets_at_ordinal`).
pub fn tile_offsets(ordinal: u64) -> Option<(u32, u32)> {
    if ordinal >= PEAK_TICKETS_PER_EXTRANONCE {
        return None;
    }
    let tile = u64::from(AI_PEAK_PARAMS.tile);
    let row = u32::try_from(ordinal / PEAK_TILE_COLS * tile).ok()?;
    let col = u32::try_from(ordinal % PEAK_TILE_COLS * tile).ok()?;
    Some((row, col))
}

/// The synthetic Pearl header a dense-peak attempt at `extranonce` must carry
/// for `challenge`.
///
/// Upstream's dense and MoE templates share one private header builder
/// (`setup_aux_inclusion`: the challenge goes into the coinbase's aux
/// commitment, hence the merkle root; the extranonce offsets the timestamp).
/// The MoE template exposes it without 288 MB of dense matrices, so the
/// registry derives the header there; `tests::moe_template_header_is_the_dense_one`
/// pins the two equal.
pub fn expected_peak_header(
    challenge: &[u8; 32],
    extranonce: u32,
) -> Result<PearlIncompleteBlockHeader, String> {
    let template = PreparedReferenceMoeTemplate::new(
        &crate::aipow_moe::AI_MOE_PARAMS,
        crate::aipow_moe::AI_MOE_HW,
        crate::aipow_moe::AI_MOE_E,
        crate::aipow_moe::AI_MOE_TOP_K,
        *challenge,
    )
    .map_err(|e| format!("header template: {e}"))?;
    Ok(template.header_for(extranonce))
}

/// Fixed dev challenge for fully-local runs.
pub fn dev_challenge() -> [u8; 32] {
    *blake3::hash(b"nockmark-m7-ai-peak-bench-dev-challenge-v1").as_bytes()
}

// ---------------------------------------------------------------------------
// Client types
// ---------------------------------------------------------------------------

/// One dense-peak workload.
pub struct AiPeakChallenge {
    /// Plays the node's `nock_block_commitment` (bound through the aux
    /// commitment). The matrices do not depend on it.
    pub challenge: [u8; 32],
    /// Registry target `T_b`, compared SCALED (`jackpot ≤ T_b · F`).
    pub target: [u8; 32],
    pub k: u64,
}

/// The production matrices, synthesized once (~0.4 s, 288 MB).
pub struct PeakMatrices {
    pub a: Arc<Vec<i8>>,
    pub b: Arc<Vec<i8>>,
}

impl PeakMatrices {
    pub fn synth() -> Self {
        let (a, b) = synth_matrices(AI_POW_PROD_SYNTH_SEED, &AI_PEAK_PARAMS);
        Self {
            a: Arc::new(a),
            b: Arc::new(b),
        }
    }

    pub fn template(&self, challenge: [u8; 32]) -> PreparedReferenceDenseTemplate {
        PreparedReferenceDenseTemplate::new(
            &AI_PEAK_PARAMS,
            challenge,
            Arc::clone(&self.a),
            Arc::clone(&self.b),
        )
        .expect("dense-peak template")
    }
}

/// One scalar-checked winning ticket, ready to prove.
pub struct PeakWin {
    pub index: u64,
    pub attempt: PearlMergeCheckedTicketAttempt,
}

/// Grind phase output.
pub struct PeakGrindResult {
    pub wins: Vec<PeakWin>,
    /// Grind start to the k-th win (proving excluded).
    pub elapsed_ms: u64,
    /// Attempts through the last win: its index + 1.
    pub total_attempts: u64,
    pub attempts_per_sec: f64,
}

/// One proved win.
pub struct AiPeakWin {
    pub index: u64,
    /// The jammed `%ai-pow` artifact — the submission blob.
    pub artifact_jam: Vec<u8>,
    pub prove_ms: u64,
}

pub struct AiPeakBenchSummary {
    pub wins: Vec<AiPeakWin>,
    pub grind_elapsed_ms: u64,
    pub total_attempts: u64,
    pub attempts_per_sec: f64,
}

// ---------------------------------------------------------------------------
// CPU grind
// ---------------------------------------------------------------------------

/// Tickets per CPU dispatch: ~0.5 s on an M1 Max. The CPU backend evaluates a
/// whole dispatch and returns its lowest winner, so after a win the next
/// dispatch restarts just past it; a smaller batch wastes less on re-checks.
pub const CPU_BATCH_TICKETS: u64 = 1 << 16;

/// Grind until `k` wins on upstream's CPU backend. Each win is re-evaluated by
/// the scalar path (`checked_winner`, which also re-checks the target) before
/// it may reach the prover.
pub fn grind_peak_cpu(
    template: &PreparedReferenceDenseTemplate,
    ch: &AiPeakChallenge,
    backend: &CpuSearchBackend,
) -> PeakGrindResult {
    assert!(ch.k >= 1, "k must be at least 1");
    let threshold = peak_threshold(&ch.target).expect("dense-peak threshold");
    let t0 = Instant::now();
    let mut wins: Vec<PeakWin> = Vec::with_capacity(ch.k as usize);
    'grind: for extranonce in 0..=u32::MAX {
        let prepared = Arc::new(template.prepare(extranonce).expect("prepare dense-peak transcript"));
        let total = prepared.row_offsets().len() as u64 * prepared.col_offsets().len() as u64;
        assert_eq!(total, PEAK_TICKETS_PER_EXTRANONCE, "tickets per extranonce");
        let mut start = 0u64;
        while start < total {
            let len = CPU_BATCH_TICKETS.min(total - start);
            let batch = SearchBatch::new(start, len, threshold).expect("search batch");
            let winner = backend
                .search_dense(Arc::clone(&prepared), batch)
                .expect("CPU dense search");
            let Some(w) = winner else {
                start += len;
                continue;
            };
            let attempt = template
                .checked_winner(&prepared, w.ordinal, &ch.target)
                .expect("scalar recheck of a CPU winner");
            assert_eq!(
                attempt.ticket.jackpot_hash, w.jackpot_hash,
                "search jackpot disagrees with the scalar recheck"
            );
            let index = attempt_index(extranonce, w.ordinal);
            wins.push(PeakWin { index, attempt });
            eprintln!(
                "  win {}/{} at extranonce {extranonce} tile {} ({:.1}s)",
                wins.len(),
                ch.k,
                w.ordinal,
                t0.elapsed().as_secs_f64()
            );
            if wins.len() as u64 == ch.k {
                break 'grind;
            }
            start = w.ordinal + 1;
        }
    }
    finish_grind(wins, t0)
}

fn finish_grind(wins: Vec<PeakWin>, t0: Instant) -> PeakGrindResult {
    let elapsed_s = t0.elapsed().as_secs_f64();
    let total_attempts = wins.last().map_or(0, |w| w.index + 1);
    PeakGrindResult {
        wins,
        // Round UP like the other benches: never claim faster than measured.
        elapsed_ms: (elapsed_s * 1000.0).ceil().max(1.0) as u64,
        total_attempts,
        attempts_per_sec: total_attempts as f64 / elapsed_s.max(1e-9),
    }
}

/// This machine's CPU attempt rate INCLUDING the per-extranonce transcript
/// preparation (commitments, noise, noised matrices): one `prepare` plus
/// `budget` of tickets, combined as `2^19 / (prepare + 2^19 / ticket_rate)`.
/// Runs on a throwaway template, before any challenge is minted.
pub fn calibrate_peak_cpu_attempts_per_sec(
    matrices: &PeakMatrices,
    backend: &CpuSearchBackend,
    budget: Duration,
) -> f64 {
    let template = matrices.template(dev_challenge());
    let t = Instant::now();
    let prepared = Arc::new(template.prepare(u32::MAX).expect("prepare calibration transcript"));
    let prepare_s = t.elapsed().as_secs_f64();
    // Threshold zero: nothing wins, every ticket is evaluated.
    let t = Instant::now();
    let mut tickets = 0u64;
    while t.elapsed() < budget && tickets < PEAK_TICKETS_PER_EXTRANONCE {
        let len = CPU_BATCH_TICKETS.min(PEAK_TICKETS_PER_EXTRANONCE - tickets);
        let batch = SearchBatch::new(tickets, len, [0u8; 32]).expect("search batch");
        let _ = backend.search_dense(Arc::clone(&prepared), batch).expect("CPU dense search");
        tickets += len;
    }
    let ticket_rate = tickets as f64 / t.elapsed().as_secs_f64().max(1e-9);
    let per_extranonce = prepare_s + PEAK_TICKETS_PER_EXTRANONCE as f64 / ticket_rate;
    PEAK_TICKETS_PER_EXTRANONCE as f64 / per_extranonce
}

// ---------------------------------------------------------------------------
// Prove
// ---------------------------------------------------------------------------

/// The jammed `%ai-pow` artifact for a proved ticket — exactly what the node's
/// `%ai-pow-verify-v2` jet receives. Built through upstream's public poke
/// builder (`[%command %pow artifact]`) and cut out of it, because the dense
/// artifact builder itself is crate-private.
pub fn artifact_jam(
    attempt: &PearlMergeCheckedTicketAttempt,
    aux_inclusion: &PearlAuxInclusionProof,
    matrices: &PeakMatrices,
    run: &AiPowCompactRecursiveCertificateRun,
) -> Result<Vec<u8>, String> {
    let poke = build_ai_pow_pearl_merge_certificate_poke_from_ticket_compact_recursive_run(
        attempt.attempt(),
        aux_inclusion,
        &matrices.a,
        &matrices.b,
        AI_PEAK_MAX_PATTERN_LEN,
        run,
    )
    .map_err(|e| format!("dense-peak poke: {e:?}"))?;
    let space = poke.noun_space();
    let root = unsafe { *poke.root() };
    let shape = |_| "poke is not [%command %pow artifact]".to_string();
    let artifact = root
        .in_space(&space)
        .as_cell()
        .map_err(shape)?
        .tail()
        .as_cell()
        .map_err(shape)?
        .tail()
        .noun();
    let mut slab: NounSlab = NounSlab::new();
    let copied = slab.copy_into(artifact, &space);
    slab.set_root(copied);
    Ok(slab.jam().to_vec())
}

/// Prove every win: the first through `prove_with_rules` (cold, and the source
/// of the aux-inclusion proof and the reusable prover setup), the rest through
/// the cached prover. Hardened rules throughout — mainnet's since 154,500.
pub fn prove_peak_wins(
    template: &PreparedReferenceDenseTemplate,
    matrices: &PeakMatrices,
    wins: Vec<PeakWin>,
) -> Vec<AiPeakWin> {
    let mut out = Vec::with_capacity(wins.len());
    let mut cache: Option<AiPowCompactRecursiveProverCache> = None;
    let mut aux: Option<PearlAuxInclusionProof> = None;
    for PeakWin { index, attempt } in wins {
        let t0 = Instant::now();
        let (attempt, run) = match (&cache, &aux) {
            (Some(cache), Some(_)) => {
                let run = prove_pearl_merge_compact_recursive_certificate_checked_with_prover_cache_with_rules(
                    &attempt,
                    &AI_PEAK_PARAMS,
                    &matrices.a,
                    &matrices.b,
                    cache,
                    ProofRules::Hardened,
                )
                .expect("prove dense-peak win (cached setup)");
                (attempt, run)
            }
            _ => {
                let block = template
                    .prove_with_rules(attempt, ProofRules::Hardened)
                    .expect("prove dense-peak win");
                aux = Some(block.aux_inclusion.clone());
                (block.attempt, block.run)
            }
        };
        let prove_ms = t0.elapsed().as_millis() as u64;
        let artifact_jam = artifact_jam(
            &attempt,
            aux.as_ref().expect("aux inclusion from the first proof"),
            matrices,
            &run,
        )
        .expect("dense-peak artifact");
        eprintln!(
            "  cert for attempt {index}: {:.1}s ({} bytes)",
            prove_ms as f64 / 1000.0,
            artifact_jam.len()
        );
        if cache.is_none() {
            cache = run.into_prover_cache();
        }
        out.push(AiPeakWin {
            index,
            artifact_jam,
            prove_ms,
        });
    }
    out
}

/// What ONE dense-peak certificate costs this machine, cold: find a winner at
/// the max target on a throwaway challenge and prove it. Run before minting.
/// The run's later wins reuse the prover setup and come in ~11% faster, so a
/// tier sized on this number buys slightly more grinding than R = 2.
pub fn calibrate_peak_prove_secs(matrices: &PeakMatrices, backend: &CpuSearchBackend) -> f64 {
    let template = matrices.template(dev_challenge());
    let target = max_peak_target();
    let threshold = peak_threshold(&target).expect("max target threshold");
    let prepared = Arc::new(template.prepare(u32::MAX).expect("prepare calibration transcript"));
    let batch = SearchBatch::new(0, 4096, threshold).expect("search batch");
    let w = backend
        .search_dense(Arc::clone(&prepared), batch)
        .expect("CPU dense search")
        .expect("1 ticket in 8 wins at the max target");
    let attempt = template
        .checked_winner(&prepared, w.ordinal, &target)
        .expect("calibration winner");
    drop(prepared);
    let t0 = Instant::now();
    template
        .prove_with_rules(attempt, ProofRules::Hardened)
        .expect("prove throwaway dense-peak calibration certificate");
    t0.elapsed().as_secs_f64().max(1e-9)
}

/// The full local workload on the CPU: grind to k wins, then certify each.
pub fn run_cpu(ch: &AiPeakChallenge, matrices: &PeakMatrices, backend: &CpuSearchBackend) -> AiPeakBenchSummary {
    let template = matrices.template(ch.challenge);
    eprintln!("grinding to {} dense-peak win(s) on the CPU…", ch.k);
    let grind = grind_peak_cpu(&template, ch, backend);
    summarize(&template, matrices, grind)
}

fn summarize(
    template: &PreparedReferenceDenseTemplate,
    matrices: &PeakMatrices,
    grind: PeakGrindResult,
) -> AiPeakBenchSummary {
    eprintln!(
        "grind window closed: {} attempts in {:.1}s",
        grind.total_attempts,
        grind.elapsed_ms as f64 / 1000.0
    );
    eprintln!("proving {} certificate(s)…", grind.wins.len());
    let PeakGrindResult {
        wins,
        elapsed_ms,
        total_attempts,
        attempts_per_sec,
    } = grind;
    let wins = prove_peak_wins(template, matrices, wins);
    AiPeakBenchSummary {
        wins,
        grind_elapsed_ms: elapsed_ms,
        total_attempts,
        attempts_per_sec,
    }
}

// ---------------------------------------------------------------------------
// GPU grind (the #159 peak kernel)
// ---------------------------------------------------------------------------

#[cfg(feature = "gpu")]
pub mod gpu {
    //! The dense-peak grind on upstream's peak CUDA kernel. Only the search
    //! moves to the device: every winner is re-derived on the CPU
    //! (`checked_search_winner`) and its transcript and jackpot compared with
    //! the device's, as upstream's `revalidate_peak_winner` does, before the
    //! CPU prover sees it.

    use super::*;
    use ai_pow_miner::peak::MultiGpuPeakSearchBackend;

    /// The CUDA device a dense-peak run uses — the one the hardware descriptor
    /// names (`crate::aipow_gpu::GPU_DEVICE_ORDINAL`).
    pub fn backend(matrices: &PeakMatrices) -> MultiGpuPeakSearchBackend {
        let backend = MultiGpuPeakSearchBackend::new(vec![crate::aipow_gpu::GPU_DEVICE_ORDINAL])
            .expect("peak CUDA backend");
        backend
            .preflight(&matrices.a, &matrices.b)
            .expect("peak CUDA preflight");
        backend
    }

    pub fn grind(
        template: &PreparedReferenceDenseTemplate,
        ch: &AiPeakChallenge,
        backend: &MultiGpuPeakSearchBackend,
    ) -> PeakGrindResult {
        assert!(ch.k >= 1, "k must be at least 1");
        let threshold = peak_threshold(&ch.target).expect("dense-peak threshold");
        let t0 = Instant::now();
        let mut wins: Vec<PeakWin> = Vec::with_capacity(ch.k as usize);
        'grind: for extranonce in 0..=u32::MAX {
            let prepared = Arc::new(
                template
                    .prepare_search(extranonce)
                    .expect("prepare dense-peak search"),
            );
            let total = prepared.total_tickets();
            assert_eq!(total, PEAK_TICKETS_PER_EXTRANONCE, "tickets per extranonce");
            let mut start = 0u64;
            while start < total {
                let batch =
                    SearchBatch::new(start, total - start, threshold).expect("search batch");
                let outcome = backend
                    .search_peak(Arc::clone(&prepared), batch)
                    .expect("peak CUDA search");
                let Some(w) = outcome.winner else { break };
                let attempt = template
                    .checked_search_winner(&prepared, w.ordinal, &ch.target)
                    .expect("scalar recheck of a GPU winner");
                assert_eq!(
                    attempt.commitments, outcome.commitments,
                    "device transcript disagrees with the scalar recheck"
                );
                assert_eq!(
                    attempt.ticket.jackpot_hash, w.jackpot_hash,
                    "device jackpot disagrees with the scalar recheck"
                );
                let index = attempt_index(extranonce, w.ordinal);
                wins.push(PeakWin { index, attempt });
                eprintln!(
                    "  win {}/{} at extranonce {extranonce} tile {} ({:.1}s)",
                    wins.len(),
                    ch.k,
                    w.ordinal,
                    t0.elapsed().as_secs_f64()
                );
                if wins.len() as u64 == ch.k {
                    break 'grind;
                }
                start = w.ordinal + 1;
            }
        }
        finish_grind(wins, t0)
    }

    /// Device rate over whole extranonces (transcript preparation included —
    /// on a GPU it is a large share of each extranonce), for `budget`.
    pub fn calibrate_attempts_per_sec(
        matrices: &PeakMatrices,
        backend: &MultiGpuPeakSearchBackend,
        budget: Duration,
    ) -> f64 {
        let template = matrices.template(dev_challenge());
        let t = Instant::now();
        let mut tickets = 0u64;
        let mut extranonce = u32::MAX;
        while t.elapsed() < budget {
            let prepared = Arc::new(
                template
                    .prepare_search(extranonce)
                    .expect("prepare calibration search"),
            );
            let total = prepared.total_tickets();
            let batch = SearchBatch::new(0, total, [0u8; 32]).expect("search batch");
            let _ = backend
                .search_peak(prepared, batch)
                .expect("peak CUDA search");
            tickets += total;
            extranonce = extranonce.wrapping_sub(1);
        }
        tickets as f64 / t.elapsed().as_secs_f64().max(1e-9)
    }

    pub fn run(
        ch: &AiPeakChallenge,
        matrices: &PeakMatrices,
        backend: &MultiGpuPeakSearchBackend,
    ) -> AiPeakBenchSummary {
        let template = matrices.template(ch.challenge);
        eprintln!("grinding to {} dense-peak win(s) on CUDA…", ch.k);
        let grind = grind(&template, ch, backend);
        summarize(&template, matrices, grind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_pow_miner::certificate_noun::{
        decode_ai_pow_pearl_merge_artifact_jam_with_rules, CertificateNounLimits,
    };

    /// F from the statement's own mining config — the verifier's route — and
    /// from the crate's difficulty decomposition, both equal to our constant.
    #[test]
    fn shape_work_factor_matches_the_crate() {
        let f = peak_shape_work_factor();
        assert_eq!(f, 1 << 21, "F = 16·16·8192");
        assert_eq!(f as f64, MAC_EQUIV_PER_PEAK_ATTEMPT);
        assert_eq!(peak_mining_config().shape_work_factor().unwrap(), f);
    }

    /// The restated config and tile geometry are upstream's: checked against
    /// the dense template and its search transcript, with the real matrices.
    #[test]
    fn config_and_geometry_match_the_upstream_template() {
        let m = PeakMatrices::synth();
        let template = m.template([0x5a; 32]);
        assert_eq!(template.config(), peak_mining_config());
        let search = template.prepare_search(7).unwrap();
        assert_eq!(search.total_tickets(), PEAK_TICKETS_PER_EXTRANONCE);
        for ordinal in [0, 1, 2047, 2048, 300_001, PEAK_TICKETS_PER_EXTRANONCE - 1] {
            assert_eq!(search.offsets_at_ordinal(ordinal), tile_offsets(ordinal), "{ordinal}");
        }
        assert_eq!(tile_offsets(PEAK_TICKETS_PER_EXTRANONCE), None);
    }

    /// The registry derives the header from the (cheap) MoE template; it must
    /// be the dense template's header for the same challenge and extranonce.
    #[test]
    fn moe_template_header_is_the_dense_one() {
        let m = PeakMatrices::synth();
        for challenge in [[0x5a; 32], dev_challenge()] {
            let template = m.template(challenge);
            for x in [0u32, 1, 77, u32::MAX] {
                assert_eq!(expected_peak_header(&challenge, x).unwrap(), template.header_for(x));
            }
        }
        assert_ne!(
            expected_peak_header(&[1; 32], 0).unwrap(),
            expected_peak_header(&[2; 32], 0).unwrap(),
            "the challenge must reach the header"
        );
    }

    #[test]
    fn attempt_index_round_trips() {
        for (x, o) in [(0u32, 0u64), (0, 524_287), (1, 0), (u32::MAX, 524_287)] {
            assert_eq!(split_attempt_index(attempt_index(x, o)), Some((x, o)));
        }
        assert_eq!(split_attempt_index(u64::from(u32::MAX) * PEAK_TICKETS_PER_EXTRANONCE + PEAK_TICKETS_PER_EXTRANONCE), None);
    }

    /// Scaled threshold: `Θ = T·F`, so the same tier is `2^(235−a)` here, not
    /// the MoE `2^(240−a)` or the dense `2^(256−a)`.
    #[test]
    fn tiers_round_trip_and_are_scaled_by_f() {
        for a in 12..=36u32 {
            let tier = 1u64 << a;
            let t = peak_target_for_attempts(tier).unwrap_or_else(|| panic!("tier 2^{a}"));
            assert_eq!(expected_attempts_per_peak_win(&t).unwrap(), tier as f64);
            assert_eq!(t, crate::aipow::pow2_target(235 - a).unwrap(), "tier 2^{a}");
        }
        assert_eq!(grant_peak_attempts(1), 1 << 12);
        assert_eq!(grant_peak_attempts(u64::MAX), 1 << 36);
        assert_eq!(peak_target_for_attempts(5_000), None, "not a granted tier");
        // Consensus's max target: 1 ticket in 8.
        assert_eq!(expected_attempts_per_peak_win(&max_peak_target()).unwrap(), 8.0);
    }

    #[test]
    fn nonce_rule_is_distinct() {
        assert_ne!(AI_PEAK_NONCE_RULE, crate::aipow::AI_NONCE_RULE);
        assert_ne!(AI_PEAK_NONCE_RULE, crate::aipow_moe::AI_MOE_NONCE_RULE);
    }

    /// End to end on the CPU at the max target, k = 1: the grind finds a win,
    /// the proof verifies through upstream's consensus decode, and the jam is
    /// the artifact (not the poke). ~75 s and ~12 GB, so ignored by default.
    #[test]
    #[ignore = "proves a full dense-peak certificate (~75 s, ~12 GB)"]
    fn cpu_grind_and_prove_one_win() {
        let m = PeakMatrices::synth();
        let backend = CpuSearchBackend::default();
        let ch = AiPeakChallenge {
            challenge: dev_challenge(),
            target: max_peak_target(),
            k: 1,
        };
        let s = run_cpu(&ch, &m, &backend);
        assert_eq!(s.wins.len(), 1);
        let art = decode_ai_pow_pearl_merge_artifact_jam_with_rules(
            &s.wins[0].artifact_jam,
            CertificateNounLimits::default(),
            ProofRules::Hardened,
        )
        .expect("artifact decodes");
        assert!(art.moe.is_none());
        assert!(s.wins[0].artifact_jam.len() <= AI_PEAK_SUBMISSION_BLOB_MAX);
    }
}
