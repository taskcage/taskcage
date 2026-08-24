//! TLS 1.3 Remote listener의 연결 제한, 인증 deadline과 순차 request dispatch를 제공한다.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};
use tokio_rustls::server::TlsStream;
use zeroize::Zeroizing;

use crate::remote_config::{PrincipalPolicy, REMOTE_ALPN, RemoteDaemonConfig};
use crate::remote_protocol::{
    ArtifactMode, CapabilitiesPayload, ErrorPayload, REMOTE_MAX_FRAME_BYTES,
    REMOTE_PROTOCOL_VERSION, RemoteErrorCode, RemoteRequest, RemoteResponse,
};

use super::auth::{AuthenticatedPrincipal, CredentialStore};
use super::codec::{FrameError, decode_json, read_frame, write_json_frame};

const MAX_CONCURRENT_AUTHENTICATIONS: usize = 4;

pub type RemoteResponseFuture<'a> = Pin<Box<dyn Future<Output = RemoteResponse> + Send + 'a>>;

pub trait RemoteOperationHandler: Send + Sync + 'static {
    fn handle<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request: RemoteRequest,
    ) -> RemoteResponseFuture<'a>;
}

impl<H> RemoteOperationHandler for Arc<H>
where
    H: RemoteOperationHandler + ?Sized,
{
    fn handle<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request: RemoteRequest,
    ) -> RemoteResponseFuture<'a> {
        self.as_ref().handle(principal, request)
    }
}

pub async fn serve_remote_until<H, S>(
    config: Arc<RemoteDaemonConfig>,
    credentials: CredentialStore,
    handler: Arc<H>,
    shutdown: S,
) -> Result<(), RemoteServerError>
where
    H: RemoteOperationHandler,
    S: Future<Output = ()>,
{
    let listener = TcpListener::bind(config.listen_address)
        .await
        .map_err(RemoteServerError::Bind)?;
    serve_remote_listener_until(listener, config, credentials, handler, shutdown).await
}

pub async fn serve_remote_listener_until<H, S>(
    listener: TcpListener,
    config: Arc<RemoteDaemonConfig>,
    credentials: CredentialStore,
    handler: Arc<H>,
    shutdown: S,
) -> Result<(), RemoteServerError>
where
    H: RemoteOperationHandler,
    S: Future<Output = ()>,
{
    let permits = Arc::new(Semaphore::new(config.max_remote_connections.get()));
    let authentication_permits = Arc::new(Semaphore::new(
        config
            .max_remote_connections
            .get()
            .min(MAX_CONCURRENT_AUTHENTICATIONS),
    ));
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(RemoteServerError::Accept)?;
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let config = Arc::clone(&config);
                let credentials = credentials.clone();
                let authentication_permits = Arc::clone(&authentication_permits);
                let handler = Arc::clone(&handler);
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_connection(stream, config, credentials, authentication_permits, handler).await {
                        tracing::debug!(event = "remote_connection_closed", cause = ?error, "Remote connection을 닫았습니다");
                    }
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::warn!(event = "remote_connection_task_failed", cause = %error, "Remote connection task가 실패했습니다");
                }
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

#[doc(hidden)]
pub async fn serve_remote_io_for_test<H>(
    stream: TcpStream,
    config: Arc<RemoteDaemonConfig>,
    credentials: CredentialStore,
    handler: Arc<H>,
) -> Result<(), String>
where
    H: RemoteOperationHandler,
{
    handle_connection(
        stream,
        config,
        credentials,
        Arc::new(Semaphore::new(1)),
        handler,
    )
    .await
    .map_err(|error| error.to_string())
}

async fn handle_connection<H: RemoteOperationHandler>(
    stream: TcpStream,
    config: Arc<RemoteDaemonConfig>,
    credentials: CredentialStore,
    authentication_permits: Arc<Semaphore>,
    handler: Arc<H>,
) -> Result<(), ConnectionError> {
    let tls = timeout(
        config.tls_handshake_timeout,
        config.tls_acceptor().accept(stream),
    )
    .await
    .map_err(|_| ConnectionError::TlsHandshakeTimeout)?
    .map_err(ConnectionError::TlsHandshake)?;
    if tls.get_ref().1.alpn_protocol() != Some(REMOTE_ALPN) {
        return Err(ConnectionError::Alpn);
    }

    let (mut tls, authenticated, session_deadline) =
        authenticate(tls, &config, &credentials, authentication_permits).await?;
    serve_authenticated(&mut tls, config, authenticated, session_deadline, handler).await
}

async fn authenticate(
    mut tls: TlsStream<TcpStream>,
    config: &RemoteDaemonConfig,
    credentials: &CredentialStore,
    authentication_permits: Arc<Semaphore>,
) -> Result<(TlsStream<TcpStream>, AuthenticatedPrincipal, Instant), ConnectionError> {
    let authentication_deadline = Instant::now() + config.authentication_timeout;
    let frame = Zeroizing::new(
        tokio::time::timeout_at(authentication_deadline, read_frame(&mut tls))
            .await
            .map_err(|_| ConnectionError::AuthenticationTimeout)?
            .map_err(|_| ConnectionError::InvalidPreAuthenticationFrame)?,
    );
    let raw: Value =
        decode_json(&frame).map_err(|_| ConnectionError::InvalidPreAuthenticationFrame)?;
    let Some(request_id) = raw
        .get("requestId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Err(ConnectionError::InvalidPreAuthenticationEnvelope);
    };
    if !crate::remote_protocol::is_uuid(&request_id) {
        return Err(ConnectionError::InvalidPreAuthenticationEnvelope);
    }
    let Some(version) = raw.get("remoteProtocolVersion").and_then(Value::as_u64) else {
        return Err(ConnectionError::InvalidPreAuthenticationEnvelope);
    };
    if version != u64::from(REMOTE_PROTOCOL_VERSION) {
        write_pre_auth_response(
            &mut tls,
            authentication_deadline,
            &error_response(
                request_id.clone(),
                RemoteErrorCode::UnsupportedRemoteProtocolVersion,
                "remote protocol version is unsupported",
                false,
            ),
        )
        .await?;
        return Err(ConnectionError::UnsupportedVersion);
    }
    let Some(operation) = raw.get("type").and_then(Value::as_str) else {
        return Err(ConnectionError::InvalidPreAuthenticationEnvelope);
    };
    if operation != "authenticate" {
        write_pre_auth_response(
            &mut tls,
            authentication_deadline,
            &error_response(
                request_id.clone(),
                RemoteErrorCode::AuthenticationRequired,
                "authenticate must be the first operation",
                false,
            ),
        )
        .await?;
        return Err(ConnectionError::AuthenticationRequired);
    }
    let request: RemoteRequest = match serde_json::from_value(raw) {
        Ok(request) => request,
        Err(_) => {
            return authentication_failed(tls, request_id, authentication_deadline).await;
        }
    };
    let RemoteRequest::Authenticate { payload, .. } = request else {
        unreachable!("authenticate type must decode to Authenticate")
    };
    if !payload.validate() {
        return authentication_failed(tls, request_id, authentication_deadline).await;
    }
    let authentication_permit = tokio::time::timeout_at(
        authentication_deadline,
        authentication_permits.acquire_owned(),
    )
    .await
    .map_err(|_| ConnectionError::AuthenticationTimeout)?
    .map_err(|_| ConnectionError::AuthenticationUnavailable)?;
    let credential_store = credentials.clone();
    let principal = tokio::task::spawn_blocking(move || {
        let _authentication_permit = authentication_permit;
        credential_store.authenticate(&payload.client_id, &payload.secret)
    })
    .await
    .map_err(ConnectionError::AuthenticationTask)?;
    if Instant::now() >= authentication_deadline {
        return Err(ConnectionError::AuthenticationTimeout);
    }
    let Some(principal) = principal else {
        return authentication_failed(tls, request_id, authentication_deadline).await;
    };
    let authenticated_at = SystemTime::now();
    let session_deadline = Instant::now() + config.session_lifetime;
    let expires_at = authenticated_at
        .checked_add(config.session_lifetime)
        .ok_or(ConnectionError::SessionDeadline)?;
    let response = RemoteResponse::Authenticated {
        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
        request_id,
        payload: crate::remote_protocol::AuthenticatedPayload {
            principal: principal.policy.client_id.clone(),
            session_expires_at: format_timestamp(expires_at)?,
        },
    };
    tokio::time::timeout_at(
        authentication_deadline,
        write_json_frame(&mut tls, &response),
    )
    .await
    .map_err(|_| ConnectionError::AuthenticationTimeout)?
    .map_err(ConnectionError::Write)?;
    Ok((tls, principal, session_deadline))
}

async fn authentication_failed(
    mut tls: TlsStream<TcpStream>,
    request_id: String,
    authentication_deadline: Instant,
) -> Result<(TlsStream<TcpStream>, AuthenticatedPrincipal, Instant), ConnectionError> {
    write_pre_auth_response(
        &mut tls,
        authentication_deadline,
        &error_response(
            request_id,
            RemoteErrorCode::AuthenticationFailed,
            "authentication failed",
            false,
        ),
    )
    .await?;
    Err(ConnectionError::AuthenticationFailed)
}

async fn write_pre_auth_response(
    tls: &mut TlsStream<TcpStream>,
    authentication_deadline: Instant,
    response: &RemoteResponse,
) -> Result<(), ConnectionError> {
    tokio::time::timeout_at(authentication_deadline, write_json_frame(tls, response))
        .await
        .map_err(|_| ConnectionError::AuthenticationTimeout)?
        .map_err(ConnectionError::Write)
}

async fn serve_authenticated<H: RemoteOperationHandler>(
    tls: &mut TlsStream<TcpStream>,
    config: Arc<RemoteDaemonConfig>,
    mut authenticated: AuthenticatedPrincipal,
    session_deadline: Instant,
    handler: Arc<H>,
) -> Result<(), ConnectionError> {
    loop {
        let frame = tokio::select! {
            _ = tokio::time::sleep_until(session_deadline) => return Err(ConnectionError::SessionExpired),
            _ = authenticated.revoked() => return Err(ConnectionError::CredentialRevoked),
            result = timeout(config.idle_connection_timeout, read_frame(tls)) => {
                result.map_err(|_| ConnectionError::IdleTimeout)?
                    .map_err(|_| ConnectionError::Read)?
            }
        };
        let raw: Value = match decode_json(&frame) {
            Ok(raw) => raw,
            Err(_) => return Err(ConnectionError::Read),
        };
        let Some(request_id) = raw
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ConnectionError::InvalidAuthenticatedEnvelope);
        };
        let request: RemoteRequest = match serde_json::from_value(raw) {
            Ok(request) => request,
            Err(_) => {
                send_with_timeout(
                    tls,
                    config.idle_connection_timeout,
                    &error_response(
                        request_id,
                        RemoteErrorCode::InvalidRequest,
                        "request envelope or payload is invalid",
                        false,
                    ),
                )
                .await?;
                continue;
            }
        };
        match request.validate_envelope() {
            Ok(()) => {}
            Err(crate::remote_protocol::RequestValidationError::UnsupportedVersion(_)) => {
                send_with_timeout(
                    tls,
                    config.idle_connection_timeout,
                    &error_response(
                        request_id,
                        RemoteErrorCode::UnsupportedRemoteProtocolVersion,
                        "remote protocol version is unsupported",
                        false,
                    ),
                )
                .await?;
                continue;
            }
            Err(
                crate::remote_protocol::RequestValidationError::InvalidRequestId
                | crate::remote_protocol::RequestValidationError::InvalidPayload,
            ) => {
                send_with_timeout(
                    tls,
                    config.idle_connection_timeout,
                    &error_response(
                        request_id,
                        RemoteErrorCode::InvalidRequest,
                        "request UUID value is invalid",
                        false,
                    ),
                )
                .await?;
                continue;
            }
        }
        let policy = authenticated.policy.clone();
        let operation_config = Arc::clone(&config);
        let operation_handler = Arc::clone(&handler);
        let mut operation = tokio::spawn(async move {
            dispatch_authenticated(
                operation_config.as_ref(),
                &policy,
                operation_handler.as_ref(),
                request,
            )
            .await
        });
        let response = tokio::select! {
            _ = tokio::time::sleep_until(session_deadline) => return Err(ConnectionError::SessionExpired),
            _ = authenticated.revoked() => return Err(ConnectionError::CredentialRevoked),
            _ = tokio::time::sleep(config.idle_connection_timeout) => return Err(ConnectionError::IdleTimeout),
            response = &mut operation => response.map_err(ConnectionError::OperationTask)?,
        };
        send_with_timeout(tls, config.idle_connection_timeout, &response).await?;
    }
}

async fn dispatch_authenticated<H: RemoteOperationHandler>(
    config: &RemoteDaemonConfig,
    principal: &PrincipalPolicy,
    handler: &H,
    request: RemoteRequest,
) -> RemoteResponse {
    let request_id = request.request_id().to_owned();
    match request {
        RemoteRequest::GetCapabilities { .. } => RemoteResponse::Capabilities {
            remote_protocol_version: REMOTE_PROTOCOL_VERSION,
            request_id,
            payload: CapabilitiesPayload {
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                remote_protocol_versions: vec![REMOTE_PROTOCOL_VERSION],
                max_frame_bytes: REMOTE_MAX_FRAME_BYTES as u32,
                artifact_modes: vec![ArtifactMode::ManagedTransfer],
                max_artifact_bytes: config.max_artifact_bytes.get(),
                max_artifact_chunk_bytes: config.max_artifact_chunk_bytes.get(),
                artifact_retention_seconds: config.artifact_retention.as_secs(),
            },
        },
        RemoteRequest::SubmitTask { .. } => error_response(
            request_id,
            RemoteErrorCode::AuthorizationDenied,
            "Remote Raw Command is not authorized",
            false,
        ),
        RemoteRequest::Authenticate { .. } => error_response(
            request_id,
            RemoteErrorCode::InvalidRequest,
            "connection is already authenticated",
            false,
        ),
        RemoteRequest::SubmitProfile { ref payload, .. }
            if !principal.allows_profile(&payload.profile) =>
        {
            error_response(
                request_id,
                RemoteErrorCode::AuthorizationDenied,
                "principal is not allowed to run the requested profile",
                false,
            )
        }
        RemoteRequest::SubmitProfile { ref payload, .. }
            if !principal.allows_resource_overrides(payload.resource_overrides.as_ref()) =>
        {
            error_response(
                request_id,
                RemoteErrorCode::AuthorizationDenied,
                "principal is not allowed to request these resource overrides",
                false,
            )
        }
        request => handler.handle(principal, request).await,
    }
}

async fn send_with_timeout(
    tls: &mut TlsStream<TcpStream>,
    deadline: Duration,
    response: &RemoteResponse,
) -> Result<(), ConnectionError> {
    timeout(deadline, write_json_frame(tls, response))
        .await
        .map_err(|_| ConnectionError::IdleTimeout)?
        .map_err(ConnectionError::Write)
}

pub fn error_response(
    request_id: String,
    code: RemoteErrorCode,
    message: impl Into<String>,
    retryable: bool,
) -> RemoteResponse {
    RemoteResponse::Error {
        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
        request_id,
        payload: ErrorPayload {
            code,
            message: message.into(),
            retryable,
        },
    }
}

fn format_timestamp(value: SystemTime) -> Result<String, ConnectionError> {
    let value = time::OffsetDateTime::from(value);
    value
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| ConnectionError::SessionDeadline)
}

#[derive(Debug, Error)]
pub enum RemoteServerError {
    #[error("Remote TCP listener를 bind하지 못했습니다")]
    Bind(#[source] io::Error),
    #[error("Remote TCP connection을 accept하지 못했습니다")]
    Accept(#[source] io::Error),
}

#[derive(Debug, Error)]
enum ConnectionError {
    #[error("TLS handshake timeout")]
    TlsHandshakeTimeout,
    #[error("TLS handshake failed")]
    TlsHandshake(#[source] io::Error),
    #[error("Remote ALPN mismatch")]
    Alpn,
    #[error("authentication timeout")]
    AuthenticationTimeout,
    #[error("invalid pre-authentication frame")]
    InvalidPreAuthenticationFrame,
    #[error("invalid pre-authentication envelope")]
    InvalidPreAuthenticationEnvelope,
    #[error("unsupported Remote protocol version")]
    UnsupportedVersion,
    #[error("authentication required")]
    AuthenticationRequired,
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("authentication worker failed")]
    AuthenticationTask(#[source] tokio::task::JoinError),
    #[error("authentication worker is unavailable")]
    AuthenticationUnavailable,
    #[error("session deadline could not be represented")]
    SessionDeadline,
    #[error("session expired")]
    SessionExpired,
    #[error("credential revoked")]
    CredentialRevoked,
    #[error("Remote operation task failed")]
    OperationTask(#[source] tokio::task::JoinError),
    #[error("idle connection timeout")]
    IdleTimeout,
    #[error("invalid authenticated envelope")]
    InvalidAuthenticatedEnvelope,
    #[error("Remote frame read failed")]
    Read,
    #[error("Remote frame write failed")]
    Write(#[source] FrameError),
}
