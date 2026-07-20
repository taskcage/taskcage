use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("taskcaged=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    match std::env::args_os().nth(1) {
        None => taskcaged::run().await?,
        Some(command) if command == "check-environment" => {
            if std::env::args_os().nth(2).is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "check-environment 뒤에는 인자를 받을 수 없습니다",
                )
                .into());
            }
            let report = taskcaged::check_environment()?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Some(command) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("알 수 없는 명령입니다: {command:?}"),
            )
            .into());
        }
    }
    Ok(())
}
