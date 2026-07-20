#[cfg(not(target_os = "linux"))]
compile_error!("taskcaged requires Linux with cgroup v2");

#[tokio::main]
async fn main() -> taskcaged::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "taskcaged=info".into()),
        )
        .init();

    taskcaged::run().await
}
