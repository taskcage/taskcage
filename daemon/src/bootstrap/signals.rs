#[cfg(test)]
use std::future::Future;
use std::io;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use crate::{remote_auth, remote_config};

#[cfg(target_os = "linux")]
pub(super) async fn reload_remote_credentials(
    path: PathBuf,
    credentials: remote_auth::CredentialStore,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = hangup.recv() => {
                match remote_config::RemoteDaemonConfig::load(&path) {
                    Ok(config) => {
                        credentials.replace_all(config.principals);
                        tracing::info!(event = "remote_credentials_reloaded", "Remote principal 설정을 다시 읽었습니다");
                    }
                    Err(error) => {
                        tracing::error!(event = "remote_credentials_reload_failed", cause = %error, "Remote principal 설정 reload에 실패했습니다");
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) async fn wait_for_listener_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(test)]
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
