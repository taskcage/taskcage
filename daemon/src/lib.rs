//! TaskCage Rust 데몬의 실행 생명주기를 제공한다.

pub mod cgroup;
pub mod preflight;

use std::future::Future;
use std::io;

use preflight::{CapabilityProbe, CapabilityReport, PreflightError, SystemProbe};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    Preflight(#[from] PreflightError),
    #[error("운영체제 종료 신호를 처리하지 못했습니다")]
    Signal(#[source] io::Error),
}

/// 지원 환경을 먼저 확인한 뒤 운영체제의 종료 신호가 올 때까지 데몬을 실행한다.
pub async fn run() -> Result<(), DaemonError> {
    let report = check_environment()?;
    tracing::info!(
        root = %report.delegated_root.display(),
        manager = %report.manager_cgroup.display(),
        "cgroup 사전 검사를 통과했습니다"
    );
    run_until(shutdown_signal())
        .await
        .map_err(DaemonError::Signal)
}

/// 서비스 시작 전 cgroup 기능과 권한을 한 번에 확인한다.
pub fn check_environment() -> Result<CapabilityReport, PreflightError> {
    SystemProbe::from_environment().check()
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}

async fn run_until<F>(shutdown: F) -> io::Result<()>
where
    F: Future<Output = io::Result<()>>,
{
    tracing::info!("TaskCage daemon started");
    shutdown.await?;
    tracing::info!("TaskCage daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stops_after_shutdown_signal() {
        run_until(async { Ok(()) }).await.unwrap();
    }

    #[tokio::test]
    async fn returns_shutdown_signal_errors() {
        let error = run_until(async { Err(io::Error::other("signal failed")) })
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
    }
}
