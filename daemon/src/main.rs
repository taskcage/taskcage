use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("taskcaged=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    taskcaged::run().await
}
