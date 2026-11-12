//! Miner-kernel plumbing: boot a SerfThread, poke it with a candidate, and
//! pull the proof out of the %mine-result effect. This mirrors what the
//! mining driver in nockchain/crates/nockchain/src/mining.rs does — it is
//! the exact mainnet proving path, minus the node.

use std::time::{Duration, Instant};

use nockapp::kernel::form::SerfThread;
use nockapp::noun::slab::NounSlab;
use nockapp::noun::AtomExt;
use nockapp::save::SaveableCheckpoint;
use nockapp::wire::WireRepr;
use nockapp::NounAllocator;
use nockvm::noun::{Atom, Noun, D, T};
use nockvm_macros::tas;
use zkvm_jetpack::hot::produce_prover_hot_state;

/// Mainnet PoW puzzle length (pow-len in hoon/common/ztd/eight.hoon).
pub const DEFAULT_POW_LEN: u64 = 64;

/// The nockchain commit the kernel jams are built from — the single
/// source of truth for the prover_version submitted with runs.
pub const NOCKCHAIN_PIN: &str = "ed1edfcc";

/// Proof version of the `bench`/`prove` commands: complete `%3` proofs, the
/// mainnet ZK proof from the Zoe fork (height 119,400) until Anthropos
/// (147,500). Mainnet now takes `%5`; `zk-bench` ([`crate::zkv5`]) proves it.
pub const PROOF_VERSION: u64 = 3;

/// max-tip5-atom = p^5 - 1 (p = 2^64 - 2^32 + 1, Goldilocks prime) as
/// LSB-first 32-bit limbs — the bignum representation check-target expects
/// (hoon/common/pow.hoon). Max target ⇒ every proof "meets the target", so
/// the kernel always returns the proof and timing is difficulty-independent.
const MAX_TARGET_LIMBS: [u32; 10] = [
    0, 4294967291, 14, 4294967266, 44, 4294967245, 44, 4294967266, 14, 4294967291,
];

pub struct ProveOutput {
    pub duration: Duration,
    /// Jammed proof, ready to write to disk or send to a verifier.
    pub proof_jam: Vec<u8>,
    /// tip5 hash of the proof (the PoW digest), hex.
    pub dig_hex: String,
}

/// Boot a kernel jam in an in-process NockVM with the prover jets loaded.
pub async fn boot_kernel(
    kernel_bytes: Vec<u8>,
    stack_size: usize,
) -> SerfThread<SaveableCheckpoint> {
    SerfThread::<SaveableCheckpoint>::new(
        kernel_bytes,
        None,
        produce_prover_hot_state(),
        stack_size,
        None,
        Vec::new(),
        Default::default(),
    )
    .await
    .expect("could not boot kernel")
}

/// Build a noun-digest:tip5, i.e. a right-nested 5-tuple [a b c d e].
/// Belts can exceed DIRECT_MAX (2^63-1), so go through Atom::from_value,
/// which allocates indirect atoms when needed (D() would panic).
pub fn belts_to_digest(slab: &mut NounSlab, belts: &[u64; 5]) -> Noun {
    let nouns: Vec<Noun> = belts
        .iter()
        .map(|b| {
            Atom::from_value(slab, *b)
                .expect("failed to allocate belt atom")
                .as_noun()
        })
        .collect();
    T(slab, &nouns)
}


/// Build a target bignum noun `[%bn limbs]` from LSB-first u32 limbs — the
/// representation `check-target` merges (hoon/common/pow.hoon).
fn target_from_limbs(slab: &mut NounSlab, limbs: &[u32]) -> Noun {
    let mut list = D(0);
    for limb in limbs.iter().rev() {
        list = T(slab, &[D(*limb as u64), list]);
    }
    T(slab, &[D(tas!(b"bn")), list])
}

/// One proving attempt: poke the miner kernel with a candidate and extract
/// the proof from the %mine-result effect. Panics on unexpected effect
/// shapes — a shape change means the nockchain commit moved under us and
/// the workload version must be re-pinned anyway.
pub async fn run_prove(
    serf: &SerfThread<SaveableCheckpoint>,
    header_belts: &[u64; 5],
    nonce_belts: &[u64; 5],
    pow_len: u64,
) -> ProveOutput {
    run_prove_at(serf, PROOF_VERSION, header_belts, nonce_belts, &MAX_TARGET_LIMBS, pow_len)
        .await
        .unwrap_or_else(|e| panic!("{e} — the max target should always pass"))
}

/// One proving poke at an explicit proof `version` and target (LSB-first u32
/// limbs of the integer target).
///
/// For `%5` the kernel first computes the object-0 digest and proves ONLY if
/// it meets `target` (hoon/apps/dumbnet/miner.hoon); a losing nonce comes back
/// as `%mine-result %|` and is returned here as `Err`, never as a proof. The
/// kernel also asserts that the proof's own PoW digest equals the one it
/// checked, so a proof returned for `%5` is bound to `nonce` and `target`.
pub async fn run_prove_at(
    serf: &SerfThread<SaveableCheckpoint>,
    version: u64,
    header_belts: &[u64; 5],
    nonce_belts: &[u64; 5],
    target_limbs: &[u32],
    pow_len: u64,
) -> Result<ProveOutput, String> {
    let mut slab: NounSlab = NounSlab::new();
    let header = belts_to_digest(&mut slab, header_belts);
    let nonce = belts_to_digest(&mut slab, nonce_belts);
    let target = target_from_limbs(&mut slab, target_limbs);
    let poke = T(&mut slab, &[D(version), header, nonce, target, D(pow_len)]);
    slab.set_root(poke);

    // Same wire the mining driver uses (mining.rs MiningWire::Candidate).
    let wire = WireRepr::new("miner", 1, vec!["candidate".into()]);

    let t0 = Instant::now();
    let result = serf
        .poke(wire, slab)
        .await
        .map_err(|e| format!("prove poke failed: {e:?}"))?;
    let duration = t0.elapsed();

    // Effects: list containing [%mine-result %& hash %command %pow proof dig header nonce]
    let space = result.noun_space();
    let mut effects = unsafe { *result.root() }.in_space(&space);
    let mut proof: Option<Noun> = None;
    let mut dig_hex = String::new();
    while let Ok(cell) = effects.as_cell() {
        let effect = cell.head();
        effects = cell.tail();
        let Ok(effect_cell) = effect.as_cell() else {
            continue;
        };
        if !effect_cell.head().eq_bytes("mine-result") {
            continue;
        }
        let each = effect_cell
            .tail()
            .as_cell()
            .expect("mine-result payload should be a cell");
        let flag = each
            .head()
            .as_atom()
            .expect("each flag should be an atom")
            .as_u64()
            .expect("each flag should be small");
        if flag != 0 {
            return Err(format!(
                "mine-result reported failure (%|): the version-{version} digest of this nonce \
                 does not meet the target"
            ));
        }
        // [hash %command %pow %dumb-zkpow proof dig header nonce]
        // (%dumb-zkpow selects the zk variant of the consensus kernel's
        // pow-variant union; added alongside proof v3 in the Zoe fork.)
        let after_hash = each
            .tail()
            .as_cell()
            .expect("expected [hash success]")
            .tail()
            .as_cell()
            .expect("expected [%command ...]");
        assert!(after_hash.head().eq_bytes("command"), "expected %command");
        let after_pow = after_hash.tail().as_cell().expect("expected [%pow ...]");
        assert!(after_pow.head().eq_bytes("pow"), "expected %pow");
        let after_variant = after_pow.tail().as_cell().expect("expected [%dumb-zkpow ...]");
        assert!(
            after_variant.head().eq_bytes("dumb-zkpow"),
            "expected %dumb-zkpow pow-variant tag"
        );
        let proof_cell = after_variant.tail().as_cell().expect("expected [proof dig ...]");
        proof = Some(proof_cell.head().noun());
        let dig = proof_cell
            .tail()
            .as_cell()
            .expect("expected [dig header nonce]")
            .head();
        if let Ok(atom) = dig.as_atom() {
            for b in atom.to_ne_bytes().iter().rev() {
                dig_hex.push_str(&format!("{b:02x}"));
            }
        }
        break;
    }

    let proof = proof.ok_or("no mine-result effect found in prove output")?;
    let mut proof_slab: NounSlab = NounSlab::new();
    let proof_copied = proof_slab.copy_into(proof, &space);
    proof_slab.set_root(proof_copied);
    let proof_jam = proof_slab.jam().to_vec();

    Ok(ProveOutput {
        duration,
        proof_jam,
        dig_hex,
    })
}
