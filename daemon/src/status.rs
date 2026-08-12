//! 실행 중인 daemon의 Protocol v1 readiness를 UDS로 확인한다.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::codec::{FrameError, read_json_frame, write_json_frame};
use crate::protocol::{EmptyPayload, PROTOCOL_VERSION, Request, Response};

#[derive(Debug, Error)]
pub enum StatusError {
    #[error("status socket에 연결하지 못했습니다")]
    Connect(#[source] io::Error),
    #[error("status protocol frame을 처리하지 못했습니다")]
    Frame(#[from] FrameError),
    #[error("status 응답 시간이 {0:?}를 넘었습니다")]
    Timeout(Duration),
    #[error("status 응답 protocolVersion이 일치하지 않습니다: {0}")]
    ProtocolVersion(u32),
    #[error("status 응답 requestId가 요청과 일치하지 않습니다")]
    RequestId,
    #[error("daemon이 status 요청을 거절했습니다: code={code}, message={message}")]
    Rejected { code: String, message: String },
    #[error("daemon이 capabilities가 아닌 응답을 반환했습니다")]
    UnexpectedResponse,
    #[error("daemon capabilities가 protocol v1 readiness 계약을 충족하지 않습니다")]
    InvalidCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatusReport {
    pub status: ReadinessStatus,
    pub daemon_version: String,
    pub protocol_versions: Vec<u32>,
    pub max_frame_bytes: u32,
    pub max_concurrent_tasks: u32,
    pub cgroup_v2_ready: bool,
}

impl DaemonStatusReport {
    pub fn is_ready(&self) -> bool {
        self.status == ReadinessStatus::Ready
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ReadinessStatus {
    #[serde(rename = "READY")]
    Ready,
    #[serde(rename = "UNREADY")]
    Unready,
}

pub async fn check(
    socket_path: &Path,
    timeout_duration: Duration,
) -> Result<DaemonStatusReport, StatusError> {
    match timeout(timeout_duration, exchange(socket_path)).await {
        Ok(result) => result,
        Err(_) => Err(StatusError::Timeout(timeout_duration)),
    }
}

async fn exchange(socket_path: &Path) -> Result<DaemonStatusReport, StatusError> {
    let request_id = new_request_id();
    let request = Request::GetCapabilities {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        payload: EmptyPayload::default(),
    };
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(StatusError::Connect)?;
    write_json_frame(&mut stream, &request).await?;
    let response: Response = read_json_frame(&mut stream).await?;
    if response.protocol_version() != PROTOCOL_VERSION {
        return Err(StatusError::ProtocolVersion(response.protocol_version()));
    }
    if response.request_id() != request_id {
        return Err(StatusError::RequestId);
    }
    match response {
        Response::Capabilities { payload, .. } => {
            if !payload.protocol_versions.contains(&PROTOCOL_VERSION)
                || payload.max_frame_bytes == 0
                || payload.max_concurrent_tasks == 0
            {
                return Err(StatusError::InvalidCapabilities);
            }
            Ok(DaemonStatusReport {
                status: if payload.cgroup_v2_ready {
                    ReadinessStatus::Ready
                } else {
                    ReadinessStatus::Unready
                },
                daemon_version: payload.daemon_version,
                protocol_versions: payload.protocol_versions,
                max_frame_bytes: payload.max_frame_bytes,
                max_concurrent_tasks: payload.max_concurrent_tasks,
                cgroup_v2_ready: payload.cgroup_v2_ready,
            })
        }
        Response::Error { payload, .. } => Err(StatusError::Rejected {
            code: error_code_name(payload.code).to_owned(),
            message: payload.message,
        }),
        _ => Err(StatusError::UnexpectedResponse),
    }
}

fn new_request_id() -> String {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        ^ u128::from(std::process::id());
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn error_code_name(code: crate::protocol::ErrorCode) -> &'static str {
    use crate::protocol::ErrorCode;
    match code {
        ErrorCode::InvalidRequest => "INVALID_REQUEST",
        ErrorCode::UnsupportedProtocolVersion => "UNSUPPORTED_PROTOCOL_VERSION",
        ErrorCode::FrameTooLarge => "FRAME_TOO_LARGE",
        ErrorCode::EnvironmentUnavailable => "ENVIRONMENT_UNAVAILABLE",
        ErrorCode::CapacityExhausted => "CAPACITY_EXHAUSTED",
        ErrorCode::TaskNotFound => "TASK_NOT_FOUND",
        ErrorCode::TaskAlreadyFinished => "TASK_ALREADY_FINISHED",
        ErrorCode::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
        ErrorCode::LimitExceedsPolicy => "LIMIT_EXCEEDS_POLICY",
        ErrorCode::InternalError => "INTERNAL_ERROR",
        ErrorCode::ProfileNotFound => "PROFILE_NOT_FOUND",
        ErrorCode::InvalidProfileInput => "INVALID_PROFILE_INPUT",
        ErrorCode::InvalidArtifactPath => "INVALID_ARTIFACT_PATH",
        ErrorCode::ArtifactDigestMismatch => "ARTIFACT_DIGEST_MISMATCH",
        ErrorCode::TaskKindMismatch => "TASK_KIND_MISMATCH",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::net::UnixListener;

    use super::*;
    use crate::protocol::{CapabilitiesPayload, ErrorCode, ErrorPayload, MAX_FRAME_BYTES};

    struct SocketPath(PathBuf);

    impl SocketPath {
        fn new(name: &str) -> Self {
            Self(std::env::temp_dir().join(format!(
                "taskcage-status-{}-{name}.sock",
                std::process::id()
            )))
        }
    }

    impl Drop for SocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn serve_response(
        path: &Path,
        response: impl FnOnce(&Request) -> Response + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = read_json_frame(&mut stream).await.unwrap();
            let response = response(&request);
            write_json_frame(&mut stream, &response).await.unwrap();
        })
    }

    fn capabilities(request: &Request, ready: bool) -> Response {
        Response::Capabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id().to_owned(),
            payload: CapabilitiesPayload {
                daemon_version: "0.1.0".to_owned(),
                protocol_versions: vec![PROTOCOL_VERSION],
                max_frame_bytes: MAX_FRAME_BYTES as u32,
                max_concurrent_tasks: 4,
                cgroup_v2_ready: ready,
            },
        }
    }

    #[tokio::test]
    async fn reports_ready_and_unready_capabilities() {
        for (name, ready, expected) in [
            ("ready", true, ReadinessStatus::Ready),
            ("unready", false, ReadinessStatus::Unready),
        ] {
            let path = SocketPath::new(name);
            let server = serve_response(&path.0, move |request| capabilities(request, ready)).await;
            let report = check(&path.0, Duration::from_secs(1)).await.unwrap();
            server.await.unwrap();
            assert_eq!(report.status, expected);
            assert_eq!(report.cgroup_v2_ready, ready);
            assert_eq!(report.max_concurrent_tasks, 4);
        }
    }

    #[tokio::test]
    async fn rejects_mismatched_and_error_responses() {
        let path = SocketPath::new("mismatch");
        let server = serve_response(&path.0, |request| {
            let mut response = capabilities(request, true);
            if let Response::Capabilities { request_id, .. } = &mut response {
                *request_id = "11111111-1111-1111-1111-111111111111".to_owned();
            }
            response
        })
        .await;
        assert!(matches!(
            check(&path.0, Duration::from_secs(1)).await,
            Err(StatusError::RequestId)
        ));
        server.await.unwrap();

        let path = SocketPath::new("error");
        let server = serve_response(&path.0, |request| Response::Error {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id().to_owned(),
            payload: ErrorPayload {
                code: ErrorCode::EnvironmentUnavailable,
                message: "not ready".to_owned(),
                retryable: false,
            },
        })
        .await;
        assert!(matches!(
            check(&path.0, Duration::from_secs(1)).await,
            Err(StatusError::Rejected { code, .. }) if code == "ENVIRONMENT_UNAVAILABLE"
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_capabilities_and_times_out() {
        let path = SocketPath::new("invalid-capabilities");
        let server = serve_response(&path.0, |request| {
            let mut response = capabilities(request, true);
            if let Response::Capabilities { payload, .. } = &mut response {
                payload.protocol_versions.clear();
            }
            response
        })
        .await;
        assert!(matches!(
            check(&path.0, Duration::from_secs(1)).await,
            Err(StatusError::InvalidCapabilities)
        ));
        server.await.unwrap();

        let path = SocketPath::new("timeout");
        let listener = UnixListener::bind(&path.0).unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        assert!(matches!(
            check(&path.0, Duration::from_millis(50)).await,
            Err(StatusError::Timeout(_))
        ));
        server.abort();
    }
}
