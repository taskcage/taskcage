//! TaskCage daemon 프로그램 진입점과 command dispatch를 제공한다.

mod cli;

use std::env;

use taskcaged::Error;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> taskcaged::Result<()> {
    configure_logging()?;
    dispatch(cli::parse(env::args_os().skip(1))?).await
}

async fn dispatch(command: cli::Command) -> taskcaged::Result<()> {
    match command {
        cli::Command::Serve(config) => taskcaged::run(config).await,
        cli::Command::CheckEnvironment => {
            let report = taskcaged::check_environment()?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        cli::Command::Status(config) => cli::status::execute(config).await,
        cli::Command::RunOnce(config) => cli::run_once::execute(config).await,
        cli::Command::ImportPackage(config) => cli::package::execute(config),
        cli::Command::Bundle(command) => cli::bundle::execute(command),
        cli::Command::Capsule(command) => cli::capsule::execute(command),
        cli::Command::CapsuleBuild(config) => cli::capsule_build::execute(config),
        cli::Command::HashRemoteSecret => cli::secret::execute(),
    }
}

fn configure_logging() -> taskcaged::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("taskcaged=info"));
    let format = env::var("TASKCAGE_LOG_FORMAT").unwrap_or_else(|_| "compact".to_owned());
    let result = match format.as_str() {
        "compact" => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .compact()
            .try_init(),
        "json" => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .try_init(),
        _ => {
            return Err(Error::InvalidArgument(
                "TASKCAGE_LOG_FORMAT은 compact 또는 json이어야 합니다".to_owned(),
            ));
        }
    };
    result.map_err(|error| Error::InvalidArgument(format!("log 초기화에 실패했습니다: {error}")))
}
