//! STARK-validity verification (M2 Task 5): wraps the nockchain `roswell`
//! crate's kernel to check that a proof jam is a valid STARK proof.

use nockapp::kernel::boot::Cli as BootCli;
use nockapp::noun::slab::NounSlab;
use nockapp::{NockAppError, NounAllocator};
use noun_serde::NounDecode;
use roswell::Roswell;
use zkvm_jetpack::form::Proof;
use zkvm_jetpack::hot::produce_prover_hot_state;

pub struct Verifier {
    roswell: Roswell,
}

impl Verifier {
    pub async fn boot() -> Result<Self, NockAppError> {
        let cli = <BootCli as clap::Parser>::parse_from(["nockmark-verifier"]);
        let roswell = Roswell::boot_with_hot_state(cli, &produce_prover_hot_state()).await?;
        Ok(Self { roswell })
    }

    pub async fn verify(&mut self, proof_jam: &[u8]) -> Result<bool, NockAppError> {
        let mut slab: NounSlab = NounSlab::new();
        let Ok(noun) = slab.cue_into(proof_jam.to_vec().into()) else {
            return Ok(false);
        };
        let space = slab.noun_space();
        let Ok(proof) = Proof::from_noun(&noun, &space) else {
            return Ok(false);
        };
        // A mangled-but-decodable proof can make the kernel poke itself
        // error out rather than return `false`; treat that the same as an
        // explicit invalid-proof result.
        match self.roswell.check_proof(&proof).await {
            Ok(b) => Ok(b),
            Err(_) => Ok(false),
        }
    }
}
