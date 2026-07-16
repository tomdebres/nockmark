use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Mutex;

use crate::kernel::RegistryKernel;
use crate::verifier::Verifier;

pub const K_DEFAULT: u64 = 2;

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Mutex<RegistryKernel>>,
    pub verifier: Arc<Mutex<Verifier>>,
}

impl AppState {
    pub async fn boot(jam: &Path, data_dir: &Path) -> Result<Self, nockapp::NockAppError> {
        Ok(Self {
            kernel: Arc::new(Mutex::new(RegistryKernel::boot(jam, data_dir).await?)),
            verifier: Arc::new(Mutex::new(Verifier::boot().await?)),
        })
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/challenge", post(new_challenge))
        .with_state(state)
}

async fn new_challenge(State(st): State<AppState>) -> Json<serde_json::Value> {
    let nonce = st
        .kernel
        .lock()
        .await
        .mint_challenge()
        .await
        .expect("mint_challenge failed");
    Json(json!({
        "nonce": nonce.to_string(),
        "pow_len": tock::miner::DEFAULT_POW_LEN,
        "k": K_DEFAULT,
        "nonce_rule": tock::nonce::NONCE_RULE,
    }))
}
