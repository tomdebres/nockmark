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
        .route("/run", post(submit_run))
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

#[derive(serde::Deserialize)]
struct RunSubmission {
    nonce: String,
    hardware: String,
    prover_version: String,
    elapsed_ms: u64,
    proofs: Vec<String>,
}

async fn submit_run(
    State(st): State<AppState>,
    Json(sub): Json<RunSubmission>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    use axum::http::StatusCode;
    use base64::Engine;

    fn bad(msg: String) -> (axum::http::StatusCode, Json<serde_json::Value>) {
        (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "error": msg })))
    }

    let Ok(nonce) = sub.nonce.parse::<u64>() else {
        return bad("nonce must be a decimal u64".into());
    };
    if sub.proofs.len() as u64 != K_DEFAULT {
        return bad(format!("expected {} proofs, got {}", K_DEFAULT, sub.proofs.len()));
    }
    // decode + bind + verify every proof BEFORE touching kernel state
    for (i, b64) in sub.proofs.iter().enumerate() {
        let Ok(jam) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            return bad(format!("proof {i}: invalid base64"));
        };
        if let Err(e) = crate::binding::check_binding(&jam, &sub.nonce, i as u64, tock::miner::DEFAULT_POW_LEN) {
            return bad(format!("proof {i}: {e}"));
        }
        // `Verifier::verify`'s future is not `Send` (it holds a raw-pointer
        // `NockStack` across an internal await inside `roswell`), which
        // axum's `Handler` blanket impl requires of the whole handler
        // future. Run it to completion on a blocking-pool thread via a
        // nested `block_on` so the non-Send state never needs to cross a
        // cooperative-scheduling boundary.
        let verifier = st.verifier.clone();
        let jam_owned = jam;
        let verify_result = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current()
                .block_on(async move { verifier.lock().await.verify(&jam_owned).await })
        })
        .await;
        match verify_result {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return bad(format!("proof {i}: STARK verification failed")),
            Ok(Err(e)) => return bad(format!("proof {i}: verifier error: {e}")),
            Err(join_err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("proof {i}: verify task failed: {join_err}") })),
                )
            }
        }
    }
    match st.kernel.lock().await
        .submit_run(nonce, &sub.hardware, &sub.prover_version, K_DEFAULT, sub.elapsed_ms)
        .await
    {
        Ok(Ok(id)) => (StatusCode::OK, Json(json!({ "run_id": id }))),
        Ok(Err(reason)) => bad(reason),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("kernel error: {e}") })),
        ),
    }
}
