use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen: String,
    #[arg(long, default_value = "../tock/assets/registry.jam")]
    kernel: std::path::PathBuf,
    #[arg(long, default_value = "./registry-data")]
    data_dir: std::path::PathBuf,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.data_dir).expect("create data dir");
    let state = nockmark_registry::http::AppState::boot(&cli.kernel, &cli.data_dir)
        .await
        .expect("boot registry");
    let app = nockmark_registry::http::router(state);
    let listener = tokio::net::TcpListener::bind(&cli.listen).await.expect("bind");
    eprintln!("nockmark registry listening on {}", cli.listen);
    axum::serve(listener, app).await.expect("serve");
}
