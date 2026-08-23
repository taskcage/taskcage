use std::io;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use super::signals::{reload_remote_credentials, shutdown_signal, wait_for_listener_shutdown};
use crate::application::task::SubmitCoordinator;
use crate::handlers::ProtocolHandlers;
use crate::remote_backend::LocalProfileRemoteBackend;
use crate::remote_config::RemoteDaemonConfig;
use crate::remote_dispatch::RemoteDispatcher;
use crate::startup::StartupOwnership;
use crate::{Error, Result, remote_artifact, remote_auth, remote_server, server};

pub(super) async fn serve(
    startup: StartupOwnership,
    cleanup_timeout: Duration,
    max_local_connections: NonZeroUsize,
    has_local_profile: bool,
    remote: Option<RemoteDaemonConfig>,
    handlers: Arc<ProtocolHandlers<SubmitCoordinator>>,
) -> Result<()> {
    if remote.is_some() && !has_local_profile {
        return Err(Error::InvalidArgument(
            "Remote listener에는 daemon-installed Profile 설정이 필요합니다".to_owned(),
        ));
    }
    if let Some(remote) = remote {
        let artifacts = remote_artifact::RemoteArtifactStore::open(
            &remote.artifact_root,
            remote.max_artifact_bytes.get(),
            remote.max_artifact_chunk_bytes.get(),
            remote.artifact_retention,
        )
        .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        let credentials = remote_auth::CredentialStore::new(remote.principals.clone());
        let backend = Arc::new(LocalProfileRemoteBackend::new(
            Arc::clone(&handlers),
            cleanup_timeout,
        ));
        let dispatcher = Arc::new(RemoteDispatcher::new(artifacts, backend));
        serve_local_and_remote(
            startup,
            cleanup_timeout,
            max_local_connections,
            handlers,
            Arc::new(remote),
            credentials,
            dispatcher,
        )
        .await
    } else {
        server::serve_protocol_until(
            startup,
            cleanup_timeout,
            max_local_connections,
            handlers,
            shutdown_signal(),
        )
        .await
        .map_err(map_local_server_error)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "local과 Remote listener의 명시적 runtime ownership이다"
)]
async fn serve_local_and_remote(
    startup: StartupOwnership,
    cleanup_timeout: Duration,
    max_local_connections: NonZeroUsize,
    handlers: Arc<ProtocolHandlers<SubmitCoordinator>>,
    remote: Arc<RemoteDaemonConfig>,
    credentials: remote_auth::CredentialStore,
    dispatcher: Arc<RemoteDispatcher<LocalProfileRemoteBackend>>,
) -> Result<()> {
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let local_shutdown_receiver = shutdown_receiver.clone();
    let local_shutdown = async move {
        wait_for_listener_shutdown(local_shutdown_receiver).await;
        Ok(())
    };
    let remote_shutdown = wait_for_listener_shutdown(shutdown_receiver.clone());
    let local = server::serve_protocol_until(
        startup,
        cleanup_timeout,
        max_local_connections,
        Arc::clone(&handlers),
        local_shutdown,
    );
    let reload = reload_remote_credentials(
        remote.source_path.clone(),
        credentials.clone(),
        shutdown_receiver,
    );
    let remote_listener =
        remote_server::serve_remote_until(remote, credentials, dispatcher, remote_shutdown);
    tokio::pin!(local);
    tokio::pin!(remote_listener);
    tokio::pin!(reload);
    enum First {
        Local(std::result::Result<(), server::ServerError>),
        Remote(std::result::Result<(), remote_server::RemoteServerError>),
        Reload(std::result::Result<(), io::Error>),
        Signal(std::result::Result<(), io::Error>),
    }
    let first = tokio::select! {
        result = &mut local => First::Local(result),
        result = &mut remote_listener => First::Remote(result),
        result = &mut reload => First::Reload(result),
        result = shutdown_signal() => First::Signal(result),
    };
    let _ = shutdown_sender.send(true);
    let (local_result, remote_result) = match first {
        First::Local(local_result) => (local_result, remote_listener.await),
        First::Remote(remote_result) => (local.await, remote_result),
        First::Reload(reload_result) => {
            reload_result.map_err(Error::Signal)?;
            tokio::join!(&mut local, &mut remote_listener)
        }
        First::Signal(signal) => {
            signal.map_err(Error::Signal)?;
            tokio::join!(&mut local, &mut remote_listener)
        }
    };
    local_result.map_err(map_local_server_error)?;
    remote_result.map_err(|error| Error::Server(error.to_string()))?;
    Ok(())
}

fn map_local_server_error(error: server::ServerError) -> Error {
    match error {
        server::ServerError::FailStop { task_id, stage } => Error::FailStop { task_id, stage },
        other => Error::Server(other.to_string()),
    }
}
