//! M0 spike: invoke Nockchain's STARK prover and verifier standalone.
//!
//! prove:  boots the miner kernel (hoon/apps/dumbnet/miner.hoon) in a
//!         SerfThread — exactly what the mining driver in
//!         nockchain/crates/nockchain/src/mining.rs does — and pokes it with
//!         [%2 header nonce target pow-len]. The nonce is derived from an
//!         arbitrary caller-supplied string. Target is set to the maximum
//!         tip5 atom so the PoW check always passes and the kernel hands the
//!         proof back in the %mine-result effect.
//!
//! verify: boots the roswell kernel (hoon/apps/roswell/roswell.hoon) and
//!         pokes %verify-proof with a cued proof jam. That runs
//!         verify:nock-verifier — the same gate dumbnet applies to incoming
//!         blocks (hoon/apps/dumbnet/inner.hoon, verify:nv call site).

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use nockapp::kernel::form::SerfThread;
use nockapp::noun::slab::NounSlab;
use nockapp::noun::AtomExt;
use nockapp::save::SaveableCheckpoint;
use nockapp::utils::{NOCK_STACK_SIZE_HUGE, NOCK_STACK_SIZE_TINY};
use nockapp::wire::{SystemWire, Wire, WireRepr};
use nockapp::NounAllocator;
use nockvm::noun::{Atom, Noun, D, T};
use nockvm_macros::tas;
use zkvm_jetpack::form::belt::PRIME;
use zkvm_jetpack::hot::produce_prover_hot_state;

/// Mainnet PoW puzzle length (pow-len in hoon/common/ztd/eight.hoon).
const DEFAULT_POW_LEN: u64 = 64;

/// Current mainnet proof version tag in the miner-kernel cause (%2).
const PROOF_VERSION: u64 = 2;

/// max-tip5-atom = p^5 - 1 (p = 2^64 - 2^32 + 1, Goldilocks prime) as
/// LSB-first 32-bit limbs — the bignum representation used by
/// check-target in hoon/common/pow.hoon. Setting the target to the max
/// means every proof "meets the target", so the miner kernel always
/// returns the proof instead of just the digest.
const MAX_TARGET_LIMBS: [u32; 10] = [
    0, 4294967291, 14, 4294967266, 44, 4294967245, 44, 4294967266, 14, 4294967291,
];

#[derive(Parser)]
#[command(about = "Nockmark M0 spike: standalone STARK prove/verify")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Produce a STARK proof whose input incorporates an arbitrary nonce.
    Prove {
        /// Arbitrary nonce seed (any string); mapped to 5 field elements.
        #[arg(long)]
        nonce: String,
        /// Path to the compiled miner kernel jam.
        #[arg(long)]
        kernel: PathBuf,
        /// Where to write the jammed proof.
        #[arg(long, default_value = "proof.jam")]
        out: PathBuf,
        #[arg(long, default_value_t = DEFAULT_POW_LEN)]
        pow_len: u64,
    },
    /// Verify a proof jam; prints ACCEPT or REJECT.
    Verify {
        /// Path to a jammed proof produced by `prove`.
        #[arg(long)]
        proof: PathBuf,
        /// Path to the compiled roswell kernel jam.
        #[arg(long)]
        kernel: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Prove {
            nonce,
            kernel,
            out,
            pow_len,
        } => prove(&nonce, &kernel, &out, pow_len).await,
        Command::Verify { proof, kernel } => verify(&proof, &kernel).await,
    }
}

/// Deterministically map an arbitrary string to 5 field elements (< PRIME),
/// the shape of noun-digest:tip5. splitmix64 over an FNV-1a seed; a real
/// harness would use tip5 itself, but any collision-resistant-enough map
/// proves the "arbitrary nonce" property for the spike.
fn seed_to_belts(seed: &str, domain: &str) -> [u64; 5] {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in domain.as_bytes().iter().chain(seed.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let mut out = [0u64; 5];
    for slot in out.iter_mut() {
        h = h.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = h;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        *slot = (z ^ (z >> 31)) % PRIME;
    }
    out
}

/// Build a noun-digest:tip5, i.e. a right-nested 5-tuple [a b c d e].
fn belts_to_digest(slab: &mut NounSlab, belts: &[u64; 5]) -> Noun {
    let nouns: Vec<Noun> = belts.iter().map(|b| D(*b)).collect();
    T(slab, &nouns)
}

/// Build the max target as a bignum noun: [%bn limbs-as-list].
fn max_target(slab: &mut NounSlab) -> Noun {
    let mut list = D(0);
    for limb in MAX_TARGET_LIMBS.iter().rev() {
        list = T(slab, &[D(*limb as u64), list]);
    }
    T(slab, &[D(tas!(b"bn")), list])
}

async fn boot_kernel(path: &PathBuf, stack_size: usize) -> SerfThread<SaveableCheckpoint> {
    let kernel_bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("could not read kernel jam {}: {e}", path.display()));
    let hot_state = produce_prover_hot_state();
    let t0 = Instant::now();
    let serf = SerfThread::<SaveableCheckpoint>::new(
        kernel_bytes,
        None,
        hot_state,
        stack_size,
        None,
        Vec::new(),
        Default::default(),
    )
    .await
    .expect("could not boot kernel");
    eprintln!("kernel boot: {:.2?}", t0.elapsed());
    serf
}

async fn prove(nonce_seed: &str, kernel: &PathBuf, out: &PathBuf, pow_len: u64) {
    let serf = boot_kernel(kernel, NOCK_STACK_SIZE_TINY).await;

    // Fixed header for the spike; in Nockmark this would be derived from the
    // registry challenge. The nonce is what the caller controls.
    let header_belts = seed_to_belts("nockmark-m0-spike", "header");
    let nonce_belts = seed_to_belts(nonce_seed, "nonce");
    eprintln!("nonce belts: {nonce_belts:?}");

    let mut slab: NounSlab = NounSlab::new();
    let header = belts_to_digest(&mut slab, &header_belts);
    let nonce = belts_to_digest(&mut slab, &nonce_belts);
    let target = max_target(&mut slab);
    let poke = T(
        &mut slab,
        &[D(PROOF_VERSION), header, nonce, target, D(pow_len)],
    );
    slab.set_root(poke);

    // Same wire the mining driver uses (mining.rs MiningWire::Candidate).
    let wire = WireRepr::new("miner", 1, vec!["candidate".into()]);

    let t0 = Instant::now();
    let result = serf.poke(wire, slab).await.expect("prove poke failed");
    let prove_time = t0.elapsed();

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
            panic!("mine-result reported failure (%|) — target should always pass");
        }
        // [hash %command %pow proof dig header nonce]
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
        let proof_cell = after_pow.tail().as_cell().expect("expected [proof dig ...]");
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

    let proof = proof.expect("no mine-result effect found in prove output");
    let mut proof_slab: NounSlab = NounSlab::new();
    let proof_copied = proof_slab.copy_into(proof, &space);
    proof_slab.set_root(proof_copied);
    let jammed = proof_slab.jam();
    std::fs::write(out, &jammed).expect("could not write proof jam");

    println!("prove: OK");
    println!("  nonce seed:   {nonce_seed:?}");
    println!("  pow-len:      {pow_len}");
    println!("  prove time:   {prove_time:.2?}");
    println!("  proof size:   {} bytes (jammed)", jammed.len());
    println!("  proof digest: 0x{dig_hex}");
    println!("  written to:   {}", out.display());
}

async fn verify(proof_path: &PathBuf, kernel: &PathBuf) {
    let serf = boot_kernel(kernel, NOCK_STACK_SIZE_HUGE).await;

    let jammed = std::fs::read(proof_path)
        .unwrap_or_else(|e| panic!("could not read proof jam {}: {e}", proof_path.display()));
    let proof_bytes_len = jammed.len();

    let mut slab: NounSlab = NounSlab::new();
    let proof = slab
        .cue_into(jammed.into())
        .expect("could not cue proof jam");

    // Cause: [%verify-proof ~ ~ proof] — p is a (unit (unit proof)).
    let tag = Atom::from_value(&mut slab, "verify-proof")
        .expect("tag atom")
        .as_noun();
    let inner_some = T(&mut slab, &[D(0), proof]);
    let outer_some = T(&mut slab, &[D(0), inner_some]);
    let cause = T(&mut slab, &[tag, outer_some]);
    slab.set_root(cause);

    let wire = SystemWire.to_wire();
    let t0 = Instant::now();
    let result = serf.poke(wire, slab).await;
    let verify_time = t0.elapsed();

    // Effects: list of [%exit code]; code 0 = proof verified.
    let verdict = match &result {
        Err(e) => {
            eprintln!("verify poke errored: {e}");
            false
        }
        Ok(result) => {
            let space = result.noun_space();
            let mut effects = unsafe { *result.root() }.in_space(&space);
            let mut ok = false;
            while let Ok(cell) = effects.as_cell() {
                let effect = cell.head();
                effects = cell.tail();
                let Ok(effect_cell) = effect.as_cell() else {
                    continue;
                };
                if effect_cell.head().eq_bytes("exit") {
                    let code = effect_cell
                        .tail()
                        .as_atom()
                        .expect("exit code should be an atom")
                        .as_u64()
                        .expect("exit code should be small");
                    ok = code == 0;
                    break;
                }
            }
            ok
        }
    };

    println!("verify: {}", if verdict { "ACCEPT" } else { "REJECT" });
    println!("  proof size:  {proof_bytes_len} bytes (jammed)");
    println!("  verify time: {verify_time:.2?}");
    if !verdict {
        std::process::exit(1);
    }
}
