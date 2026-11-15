use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen: String,
    #[arg(long, default_value = "../tock/assets/registry.jam")]
    kernel: std::path::PathBuf,
    #[arg(long, default_value = "./registry-data")]
    data_dir: std::path::PathBuf,
    /// Proofs required per submission (challenge responses advertise this).
    #[arg(long, default_value_t = nockmark_registry::http::K_DEFAULT)]
    k: u64,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.data_dir).expect("create data dir");
    // Reclaim ~1 GB per stale pin before anything else touches the volume.
    nockmark_registry::aipow::sweep_stale_contexts(&cli.data_dir);
    let state = nockmark_registry::http::AppState::boot_with_k(&cli.kernel, &cli.data_dir, cli.k)
        .await
        .expect("boot registry");
    // One 10-minute loop refreshes both difficulties and the NOCK price.
    // It is worth running for either alone: an instance with a hand-set
    // difficulty still wants a live price, and an instance with the price
    // feed switched off still wants difficulty.
    let sources = nockmark_registry::economics::sources_from_env();
    if !sources.is_empty() || nockmark_registry::economics::price_url().is_some() {
        tokio::spawn(nockmark_registry::economics::refresh_loop(
            sources,
            Some(state.econ_history_path()),
            state.econ.clone(),
        ));
    }
    let app = nockmark_registry::http::router(state);
    let listener = tokio::net::TcpListener::bind(&cli.listen).await.expect("bind");
    eprintln!("nockmark registry listening on {}", cli.listen);
    axum::serve(listener, app).await.expect("serve");
}
