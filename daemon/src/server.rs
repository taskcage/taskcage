//! Unix domain socket에서 protocol v1 frame을 순서대로 처리한다.

use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;

use crate::codec::{FrameError, decode_json, read_frame_or_eof, write_json_frame};
use crate::deadline::MonotonicDeadline;
use crate::fail_stop::FailStopCoordinator;
use crate::handlers::{ProtocolHandlers, SubmitContext};
use crate::protocol::{ErrorCode, ErrorPayload, PROTOCOL_VERSION, Request, Response};
use crate::startup::StartupOwnership;
use crate::submit::{SubmitCoordinator, SubmitMetadata};

type DispatchFuture = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;
type Dispatch = Arc<dyn Fn(Request) -> DispatchFuture + Send + Sync + 'static>;
static TASK_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SOCKET_BIND_UMASK: Mutex<()> = Mutex::new(());

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("daemon socket 경로는 절대 경로여야 합니다: {0}")]
    RelativeSocketPath(PathBuf),
    #[error("daemon socket 경로가 이미 존재합니다: {0}")]
    ExistingPath(PathBuf),
    #[error("daemon socket 경로를 확인하지 못했습니다: {path}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("daemon socket을 bind하지 못했습니다: {path}")]
    Bind {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("daemon socket mode가 0600이 아닙니다: {path}, mode={mode:o}")]
    UnexpectedMode { path: PathBuf, mode: u32 },
    #[error("daemon socket owner가 현재 daemon과 다릅니다: {path}, owner={owner}")]
    UnexpectedOwner { path: PathBuf, owner: u32 },
    #[error("daemon socket의 link count가 1이 아닙니다: {path}, links={links}")]
    UnexpectedLinks { path: PathBuf, links: u64 },
    #[error("daemon socket 연결을 받지 못했습니다")]
    Accept(#[source] io::Error),
    #[error("daemon shutdown 신호를 처리하지 못했습니다")]
    Shutdown(#[source] io::Error),
    #[error("자신이 bind한 socket 경로가 다른 파일로 바뀌어 삭제하지 않았습니다: {0}")]
    OwnershipChanged(PathBuf),
    #[error("자신이 bind한 daemon socket을 제거하지 못했습니다: {path}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("UDS 처리 실패와 socket 정리가 함께 실패했습니다: server={server}; cleanup={cleanup}")]
    ServeAndCleanup { server: String, cleanup: String },
    #[error(
        "정리 불확실 fail-stop이 종료 기한 안에 daemon을 중단했습니다: taskId={task_id}, stage={stage}"
    )]
    FailStop { task_id: String, stage: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug)]
struct BoundSocket {
    listener: Option<UnixListener>,
    path: PathBuf,
    identity: SocketIdentity,
}

impl BoundSocket {
    fn bind(path: &Path) -> Result<Self, ServerError> {
        if !path.is_absolute() {
            return Err(ServerError::RelativeSocketPath(path.to_path_buf()));
        }
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Err(ServerError::ExistingPath(path.to_path_buf())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ServerError::Inspect {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }

        let listener = bind_owner_only(path).map_err(|source| ServerError::Bind {
            path: path.to_path_buf(),
            source,
        })?;
        let current = std::fs::symlink_metadata(path).map_err(|source| ServerError::Inspect {
            path: path.to_path_buf(),
            source,
        })?;
        if !current.file_type().is_socket() {
            return Err(ServerError::OwnershipChanged(path.to_path_buf()));
        }
        let identity = SocketIdentity::from_metadata(&current);
        let owner = current.uid();
        if owner != unsafe { libc::geteuid() } {
            return Err(ServerError::UnexpectedOwner {
                path: path.to_path_buf(),
                owner,
            });
        }
        let mode = current.mode() & 0o777;
        if mode != 0o600 {
            let _ = remove_owned_path(path, identity);
            return Err(ServerError::UnexpectedMode {
                path: path.to_path_buf(),
                mode,
            });
        }
        let links = current.nlink();
        if links != 1 {
            return Err(ServerError::UnexpectedLinks {
                path: path.to_path_buf(),
                links,
            });
        }

        Ok(Self {
            listener: Some(listener),
            path: path.to_path_buf(),
            identity,
        })
    }

    fn take_listener(&mut self) -> UnixListener {
        self.listener
            .take()
            .expect("정리 전에는 listener가 존재해야 합니다")
    }

    fn cleanup(mut self) -> Result<(), ServerError> {
        drop(self.listener.take());
        remove_owned_path(&self.path, self.identity)
    }
}

fn bind_owner_only(path: &Path) -> io::Result<UnixListener> {
    let _guard = SOCKET_BIND_UMASK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // umask는 process-wide이므로 bind 직전의 짧은 동기 구간에서만 바꾸고 즉시 복원한다.
    let previous = unsafe { libc::umask(0o177) };
    let listener = UnixListener::bind(path);
    // 앞의 umask 호출이 반환한 기존 값을 그대로 복원한다.
    unsafe { libc::umask(previous) };
    listener
}

fn remove_owned_path(path: &Path, identity: SocketIdentity) -> Result<(), ServerError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ServerError::Inspect {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.file_type().is_socket() || SocketIdentity::from_metadata(&metadata) != identity {
        return Err(ServerError::OwnershipChanged(path.to_path_buf()));
    }
    std::fs::remove_file(path).map_err(|source| ServerError::Remove {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) async fn serve_protocol_until<S>(
    startup: StartupOwnership,
    cleanup_timeout: Duration,
    handlers: Arc<ProtocolHandlers<SubmitCoordinator>>,
    shutdown: S,
) -> Result<(), ServerError>
where
    S: Future<Output = io::Result<()>>,
{
    let dispatch_handlers = Arc::clone(&handlers);
    let dispatch: Dispatch = Arc::new(move |request| {
        let handlers = Arc::clone(&dispatch_handlers);
        Box::pin(async move {
            handlers
                .handle_request(request, || submit_context(cleanup_timeout))
                .await
        })
    });

    let result = serve_socket_until_fail_stop(
        &startup,
        dispatch,
        shutdown,
        Arc::clone(handlers.fail_stop()),
    )
    .await;
    // 연결 task를 닫아도 coordinator가 시작한 실행은 독립적으로 cleanup을 끝낸다.
    if !matches!(result, Err(ServerError::FailStop { .. })) {
        handlers.wait_idle().await;
    }
    // 실행 coordinator의 마지막 cleanup이 끝난 뒤에만 daemon 생존 기간 lock을 해제한다.
    drop(startup);
    result
}

#[cfg(test)]
async fn serve_socket_until<S>(
    startup: StartupOwnership,
    dispatch: Dispatch,
    shutdown: S,
) -> Result<(), ServerError>
where
    S: Future<Output = io::Result<()>>,
{
    let mut socket = BoundSocket::bind(startup.socket_path())?;
    let listener = socket.take_listener();
    let result = accept_until_shutdown(listener, dispatch, shutdown).await;
    let cleanup = socket.cleanup();
    combine_server_and_cleanup(result, cleanup)
}

async fn serve_socket_until_fail_stop<S>(
    startup: &StartupOwnership,
    dispatch: Dispatch,
    shutdown: S,
    fail_stop: Arc<FailStopCoordinator>,
) -> Result<(), ServerError>
where
    S: Future<Output = io::Result<()>>,
{
    let mut socket = BoundSocket::bind(startup.socket_path())?;
    let listener = socket.take_listener();
    let result = accept_until_shutdown_or_fail_stop(listener, dispatch, shutdown, fail_stop).await;
    let cleanup = socket.cleanup();
    combine_server_and_cleanup(result, cleanup)
}

fn combine_server_and_cleanup(
    result: Result<(), ServerError>,
    cleanup: Result<(), ServerError>,
) -> Result<(), ServerError> {
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(server), Ok(())) => Err(server),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(server), Err(cleanup)) => Err(ServerError::ServeAndCleanup {
            server: server.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

#[cfg(test)]
async fn accept_until_shutdown<S>(
    listener: UnixListener,
    dispatch: Dispatch,
    shutdown: S,
) -> Result<(), ServerError>
where
    S: Future<Output = io::Result<()>>,
{
    accept_connections(listener, dispatch, shutdown, None).await
}

async fn accept_until_shutdown_or_fail_stop<S>(
    listener: UnixListener,
    dispatch: Dispatch,
    shutdown: S,
    fail_stop: Arc<FailStopCoordinator>,
) -> Result<(), ServerError>
where
    S: Future<Output = io::Result<()>>,
{
    accept_connections(listener, dispatch, shutdown, Some(fail_stop)).await
}

async fn accept_connections<S>(
    listener: UnixListener,
    dispatch: Dispatch,
    shutdown: S,
    fail_stop: Option<Arc<FailStopCoordinator>>,
) -> Result<(), ServerError>
where
    S: Future<Output = io::Result<()>>,
{
    enum Stop {
        Normal(Result<(), ServerError>),
        FailStop(MonotonicDeadline),
    }

    tokio::pin!(shutdown);
    let mut connections = JoinSet::new();
    let stop = loop {
        tokio::select! {
            biased;
            deadline = wait_for_fail_stop(fail_stop.as_deref()) => break Stop::FailStop(deadline),
            result = &mut shutdown => break Stop::Normal(result.map_err(ServerError::Shutdown)),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let connection_dispatch = Arc::clone(&dispatch);
                    connections.spawn(async move {
                        handle_connection(stream, connection_dispatch).await
                    });
                }
                Err(error) => break Stop::Normal(Err(ServerError::Accept(error))),
            },
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(result) = joined {
                    log_connection_result(result);
                }
            }
        }
    };
    drop(listener);

    match stop {
        Stop::Normal(result) => {
            abort_connections(&mut connections).await;
            result
        }
        Stop::FailStop(deadline) => {
            let coordinator = fail_stop.expect("fail-stop 대기가 활성화돼 있어야 합니다");
            drain_fail_stop_connections(&mut connections, &coordinator, deadline).await;
            let report = coordinator
                .first_report()
                .expect("최초 fail-stop 보고가 있어야 합니다");
            Err(ServerError::FailStop {
                task_id: report.task_id,
                stage: report.stage.to_owned(),
            })
        }
    }
}

async fn wait_for_fail_stop(fail_stop: Option<&FailStopCoordinator>) -> MonotonicDeadline {
    match fail_stop {
        Some(coordinator) => coordinator.activated().await,
        None => std::future::pending().await,
    }
}

async fn drain_fail_stop_connections(
    connections: &mut JoinSet<Result<(), ConnectionError>>,
    fail_stop: &FailStopCoordinator,
    deadline: MonotonicDeadline,
) {
    loop {
        if connections.is_empty() && fail_stop.active_count() == 0 {
            break;
        }
        let Some(remaining) = deadline.remaining() else {
            break;
        };
        tokio::select! {
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(result) = joined {
                    log_connection_result(result);
                }
            }
            _ = fail_stop.active_changed() => {}
            _ = tokio::time::sleep(remaining) => break,
        }
    }
    abort_connections(connections).await;
}

async fn abort_connections(connections: &mut JoinSet<Result<(), ConnectionError>>) {
    connections.abort_all();
    while let Some(joined) = connections.join_next().await {
        log_connection_result(joined);
    }
}

fn log_connection_result(result: Result<Result<(), ConnectionError>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(cause = %error, "UDS 연결을 종료했습니다"),
        Err(error) if error.is_cancelled() => {}
        Err(error) => tracing::warn!(cause = %error, "UDS 연결 task가 비정상 종료했습니다"),
    }
}

#[derive(Debug, Error)]
enum ConnectionError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("protocol v1 요청 object를 해석할 수 없습니다")]
    InvalidRequest,
}

async fn handle_connection(
    mut stream: UnixStream,
    dispatch: Dispatch,
) -> Result<(), ConnectionError> {
    loop {
        let Some(payload) = read_frame_or_eof(&mut stream).await? else {
            return Ok(());
        };
        let value = decode_json::<Value>(&payload)?;
        let request = match serde_json::from_value::<Request>(value.clone()) {
            Ok(request) => request,
            Err(error) => {
                tracing::debug!(cause = %error, "protocol v1 요청 schema를 거절했습니다");
                let Some(request_id) = request_id_from_value(&value) else {
                    return Err(ConnectionError::InvalidRequest);
                };
                let response = invalid_request_response(request_id);
                write_json_frame(&mut stream, &response).await?;
                continue;
            }
        };
        let response = dispatch(request).await;
        write_json_frame(&mut stream, &response).await?;
    }
}

fn request_id_from_value(value: &Value) -> Option<String> {
    value
        .as_object()?
        .get("requestId")?
        .as_str()
        .map(str::to_owned)
}

fn invalid_request_response(request_id: String) -> Response {
    Response::Error {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        payload: ErrorPayload {
            code: ErrorCode::InvalidRequest,
            message: "request does not match the protocol v1 schema".to_owned(),
            retryable: false,
        },
    }
}

fn submit_context(cleanup_timeout: Duration) -> SubmitContext {
    let submitted_at = timestamp_now();
    let started_at = timestamp_now();
    let started_monotonic = Instant::now();
    SubmitContext::new(
        SubmitMetadata::lazy(
            new_task_id,
            submitted_at,
            started_at,
            started_monotonic,
            cleanup_timeout,
        ),
        Box::new(|| (timestamp_now(), Instant::now())),
    )
}

fn timestamp_now() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let Ok(seconds) = libc::time_t::try_from(duration.as_secs()) else {
        return "1970-01-01T00:00:00.000000000Z".to_owned();
    };
    let mut calendar = std::mem::MaybeUninit::<libc::tm>::uninit();
    // gmtime_r는 전달한 time_t를 caller 소유 tm에 기록하므로 공유 정적 버퍼를 사용하지 않는다.
    let converted = unsafe { libc::gmtime_r(&seconds, calendar.as_mut_ptr()) };
    if converted.is_null() {
        return "1970-01-01T00:00:00.000000000Z".to_owned();
    }
    // null이 아닌 gmtime_r 반환은 tm 전체 초기화를 보장한다.
    let calendar = unsafe { calendar.assume_init() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        calendar.tm_year + 1900,
        calendar.tm_mon + 1,
        calendar.tm_mday,
        calendar.tm_hour,
        calendar.tm_min,
        calendar.tm_sec,
        duration.subsec_nanos()
    )
}

fn new_task_id() -> String {
    let sequence = TASK_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let process_sequence = (u64::from(std::process::id()) << 32) ^ sequence;
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&time.to_be_bytes());
    bytes[8..].copy_from_slice(&process_sequence.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{Notify, oneshot};
    use tokio::time::{sleep, timeout};

    use super::*;
    use crate::capacity::TaskCapacitySettings;
    use crate::codec::read_json_frame;
    use crate::preflight::{CapabilityProbe, SystemProbe};
    use crate::protocol::{
        CapabilitiesPayload, CommandSpec, CpuMax, EmptyPayload, OutputLimits, ProcessResult,
        ResourceLimits, SubmitTaskPayload, TaskIdPayload, TaskPayload, TaskState,
        TerminationReason,
    };

    const FIRST_REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const SECOND_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";

    struct TestSocketPath {
        directory: PathBuf,
        socket: PathBuf,
    }

    impl TestSocketPath {
        fn new(label: &str) -> Self {
            let directory = std::env::current_dir()
                .unwrap()
                .join("target")
                .join("taskcage-server-tests")
                .join(format!(
                    "taskcage-uds-{label}-{}-{}",
                    std::process::id(),
                    new_task_id()
                ));
            std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            let socket = directory.join("taskcaged.sock");
            Self { directory, socket }
        }

        fn lock(&self) -> PathBuf {
            self.directory.join(".taskcaged.lock")
        }
    }

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            if let Ok(metadata) = std::fs::symlink_metadata(&self.socket) {
                if metadata.file_type().is_dir() {
                    let _ = std::fs::remove_dir(&self.socket);
                } else {
                    let _ = std::fs::remove_file(&self.socket);
                }
            }
            let _ = std::fs::remove_file(self.lock());
            let _ = std::fs::remove_dir(&self.directory);
        }
    }

    fn capabilities(request_id: String) -> Response {
        Response::Capabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            payload: CapabilitiesPayload {
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol_versions: vec![PROTOCOL_VERSION],
                max_frame_bytes: crate::protocol::MAX_FRAME_BYTES as u32,
                max_concurrent_tasks: 1,
                cgroup_v2_ready: false,
            },
        }
    }

    fn echo_dispatch() -> Dispatch {
        Arc::new(|request| {
            let request_id = request.request_id().to_owned();
            Box::pin(async move { capabilities(request_id) })
        })
    }

    async fn start_server(
        path: PathBuf,
        dispatch: Dispatch,
    ) -> (
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), ServerError>>,
    ) {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let startup = StartupOwnership::acquire(&path).unwrap();
        let server = tokio::spawn(async move {
            serve_socket_until(startup, dispatch, async move {
                let _ = shutdown_receiver.await;
                Ok(())
            })
            .await
        });
        (shutdown_sender, server)
    }

    async fn start_fail_stop_server(
        path: PathBuf,
        dispatch: Dispatch,
        fail_stop: Arc<FailStopCoordinator>,
    ) -> tokio::task::JoinHandle<Result<(), ServerError>> {
        let startup = StartupOwnership::acquire(&path).unwrap();
        tokio::spawn(async move {
            serve_socket_until_fail_stop(
                &startup,
                dispatch,
                std::future::pending::<io::Result<()>>(),
                fail_stop,
            )
            .await
        })
    }

    async fn connect(path: &Path) -> UnixStream {
        timeout(Duration::from_secs(2), async {
            loop {
                match UnixStream::connect(path).await {
                    Ok(stream) => return stream,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        sleep(Duration::from_millis(5)).await;
                    }
                    Err(error) => panic!("시험 UDS 연결 실패: {error}"),
                }
            }
        })
        .await
        .expect("시험 UDS가 시작돼야 합니다")
    }

    fn capability_request(request_id: &str) -> Request {
        Request::GetCapabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            payload: EmptyPayload {},
        }
    }

    async fn exchange(stream: &mut UnixStream, request: &Request) -> Response {
        write_json_frame(stream, request).await.unwrap();
        read_json_frame(stream).await.unwrap()
    }

    #[tokio::test]
    async fn one_connection_processes_multiple_requests_in_order() {
        let path = TestSocketPath::new("sequential");
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatch_calls = Arc::clone(&calls);
        let dispatch: Dispatch = Arc::new(move |request| {
            let request_id = request.request_id().to_owned();
            let dispatch_calls = Arc::clone(&dispatch_calls);
            Box::pin(async move {
                dispatch_calls.fetch_add(1, Ordering::SeqCst);
                capabilities(request_id)
            })
        });
        let (shutdown, server) = start_server(path.socket.clone(), dispatch).await;
        let mut stream = connect(&path.socket).await;

        for request_id in [FIRST_REQUEST_ID, SECOND_REQUEST_ID] {
            let response = exchange(&mut stream, &capability_request(request_id)).await;
            assert_eq!(response.request_id(), request_id);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        shutdown.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn different_connections_are_processed_concurrently() {
        let path = TestSocketPath::new("concurrent");
        let first_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let dispatch: Dispatch = {
            let first_started = Arc::clone(&first_started);
            let release_first = Arc::clone(&release_first);
            Arc::new(move |request| {
                let request_id = request.request_id().to_owned();
                let first_started = Arc::clone(&first_started);
                let release_first = Arc::clone(&release_first);
                Box::pin(async move {
                    if request_id == FIRST_REQUEST_ID {
                        first_started.notify_one();
                        release_first.notified().await;
                    }
                    capabilities(request_id)
                })
            })
        };
        let (shutdown, server) = start_server(path.socket.clone(), dispatch).await;

        let first_path = path.socket.clone();
        let first = tokio::spawn(async move {
            let mut stream = connect(&first_path).await;
            exchange(&mut stream, &capability_request(FIRST_REQUEST_ID)).await
        });
        first_started.notified().await;
        let mut second = connect(&path.socket).await;
        let second_response = timeout(
            Duration::from_millis(200),
            exchange(&mut second, &capability_request(SECOND_REQUEST_ID)),
        )
        .await
        .expect("다른 연결은 첫 연결의 느린 요청을 기다리지 않아야 합니다");
        assert_eq!(second_response.request_id(), SECOND_REQUEST_ID);

        release_first.notify_one();
        assert_eq!(first.await.unwrap().request_id(), FIRST_REQUEST_ID);
        shutdown.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_stops_accepting_and_aborts_existing_connection_handlers() {
        let path = TestSocketPath::new("handler-shutdown");
        let started = Arc::new(Notify::new());
        let never_finish = Arc::new(Notify::new());
        let dispatch: Dispatch = {
            let started = Arc::clone(&started);
            let never_finish = Arc::clone(&never_finish);
            Arc::new(move |request| {
                let request_id = request.request_id().to_owned();
                let started = Arc::clone(&started);
                let never_finish = Arc::clone(&never_finish);
                Box::pin(async move {
                    started.notify_one();
                    never_finish.notified().await;
                    capabilities(request_id)
                })
            })
        };
        let (shutdown, server) = start_server(path.socket.clone(), dispatch).await;
        let mut stream = connect(&path.socket).await;
        write_json_frame(&mut stream, &capability_request(FIRST_REQUEST_ID))
            .await
            .unwrap();
        started.notified().await;

        shutdown.send(()).unwrap();
        timeout(Duration::from_secs(1), server)
            .await
            .expect("shutdown은 기존 connection handler를 정리해야 합니다")
            .unwrap()
            .unwrap();
        assert!(!path.socket.exists());
    }

    #[tokio::test]
    async fn fail_stop_closes_listener_but_keeps_existing_queries_until_disconnect() {
        let path = TestSocketPath::new("fail-stop-existing");
        let fail_stop = FailStopCoordinator::new(
            crate::fail_stop::FailStopSettings::new(Duration::from_secs(1)).unwrap(),
        );
        let dispatch: Dispatch = {
            let fail_stop = Arc::clone(&fail_stop);
            Arc::new(move |request| {
                let mut response = capabilities(request.request_id().to_owned());
                let fail_stop = Arc::clone(&fail_stop);
                Box::pin(async move {
                    if let Response::Capabilities { payload, .. } = &mut response {
                        payload.cgroup_v2_ready = !fail_stop.is_fail_stopping();
                    }
                    response
                })
            })
        };
        let server =
            start_fail_stop_server(path.socket.clone(), dispatch, Arc::clone(&fail_stop)).await;
        let mut existing = connect(&path.socket).await;
        assert!(matches!(
            exchange(&mut existing, &capability_request(FIRST_REQUEST_ID)).await,
            Response::Capabilities { payload, .. } if payload.cgroup_v2_ready
        ));

        fail_stop.activate(crate::fail_stop::CleanupFailureReport::new(
            "task",
            "시험 정리",
            vec!["작업 cgroup"],
            "실패",
        ));
        assert!(matches!(
            exchange(&mut existing, &capability_request(SECOND_REQUEST_ID)).await,
            Response::Capabilities { payload, .. } if !payload.cgroup_v2_ready
        ));
        timeout(Duration::from_secs(1), async {
            while let Ok(stream) = UnixStream::connect(&path.socket).await {
                drop(stream);
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fail-stop은 신규 연결 listener를 닫아야 합니다");
        drop(existing);

        assert!(matches!(
            timeout(Duration::from_secs(1), server)
                .await
                .unwrap()
                .unwrap(),
            Err(ServerError::FailStop { .. })
        ));
        assert!(!path.socket.exists());
    }

    #[tokio::test]
    async fn fail_stop_deadline_aborts_a_stuck_existing_handler() {
        let path = TestSocketPath::new("fail-stop-deadline");
        let fail_stop = FailStopCoordinator::new(
            crate::fail_stop::FailStopSettings::new(Duration::from_millis(50)).unwrap(),
        );
        let started = Arc::new(Notify::new());
        let never_finish = Arc::new(Notify::new());
        let dispatch: Dispatch = {
            let started = Arc::clone(&started);
            let never_finish = Arc::clone(&never_finish);
            Arc::new(move |request| {
                let started = Arc::clone(&started);
                let never_finish = Arc::clone(&never_finish);
                let request_id = request.request_id().to_owned();
                Box::pin(async move {
                    started.notify_one();
                    never_finish.notified().await;
                    capabilities(request_id)
                })
            })
        };
        let server =
            start_fail_stop_server(path.socket.clone(), dispatch, Arc::clone(&fail_stop)).await;
        let mut stream = connect(&path.socket).await;
        write_json_frame(&mut stream, &capability_request(FIRST_REQUEST_ID))
            .await
            .unwrap();
        started.notified().await;
        fail_stop.activate(crate::fail_stop::CleanupFailureReport::new(
            "task",
            "시험 정리",
            vec!["작업 cgroup"],
            "실패",
        ));

        assert!(matches!(
            timeout(Duration::from_secs(1), server)
                .await
                .unwrap()
                .unwrap(),
            Err(ServerError::FailStop { .. })
        ));
        assert!(!path.socket.exists());
        assert_eq!(
            stream.read_u8().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn malformed_and_oversized_frames_close_only_the_connection() {
        let path = TestSocketPath::new("invalid-frame");
        let (shutdown, server) = start_server(path.socket.clone(), echo_dispatch()).await;

        for bytes in [
            0_u32.to_be_bytes().to_vec(),
            ((crate::protocol::MAX_FRAME_BYTES + 1) as u32)
                .to_be_bytes()
                .to_vec(),
            {
                let payload = br#"{"requestId":"a","requestId":"b"}"#;
                let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
                frame.extend_from_slice(payload);
                frame
            },
            {
                let payload = br#"{"#;
                let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
                frame.extend_from_slice(payload);
                frame
            },
        ] {
            let mut stream = connect(&path.socket).await;
            stream.write_all(&bytes).await.unwrap();
            assert_eq!(
                stream.read_u8().await.unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }

        let mut healthy = connect(&path.socket).await;
        assert!(matches!(
            exchange(&mut healthy, &capability_request(FIRST_REQUEST_ID)).await,
            Response::Capabilities { .. }
        ));
        shutdown.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unknown_request_fields_return_invalid_request_without_new_wire_values() {
        let path = TestSocketPath::new("invalid-request");
        let (shutdown, server) = start_server(path.socket.clone(), echo_dispatch()).await;
        let mut stream = connect(&path.socket).await;
        let payload = format!(
            r#"{{"protocolVersion":1,"requestId":"{FIRST_REQUEST_ID}","type":"getCapabilities","payload":{{}},"unknown":true}}"#
        );
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(payload.as_bytes());
        stream.write_all(&frame).await.unwrap();

        let response: Response = read_json_frame(&mut stream).await.unwrap();
        assert!(matches!(
            response,
            Response::Error {
                request_id,
                payload: ErrorPayload {
                    code: ErrorCode::InvalidRequest,
                    retryable: false,
                    ..
                },
                ..
            } if request_id == FIRST_REQUEST_ID
        ));
        assert!(matches!(
            exchange(&mut stream, &capability_request(SECOND_REQUEST_ID)).await,
            Response::Capabilities { .. }
        ));

        shutdown.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn exact_maximum_frame_is_accepted() {
        let path = TestSocketPath::new("max-frame");
        let (shutdown, server) = start_server(path.socket.clone(), echo_dispatch()).await;
        let mut stream = connect(&path.socket).await;
        let mut payload = format!(
            r#"{{"protocolVersion":1,"requestId":"{FIRST_REQUEST_ID}","type":"getCapabilities","payload":{{}}}}"#
        )
        .into_bytes();
        payload.resize(crate::protocol::MAX_FRAME_BYTES, b' ');
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        stream.write_all(&frame).await.unwrap();

        let response: Response = read_json_frame(&mut stream).await.unwrap();
        assert!(matches!(response, Response::Capabilities { .. }));
        shutdown.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn normal_shutdown_removes_only_the_owned_owner_only_socket() {
        let path = TestSocketPath::new("shutdown");
        let (shutdown, server) = start_server(path.socket.clone(), echo_dispatch()).await;
        let _stream = connect(&path.socket).await;
        let mode = std::fs::symlink_metadata(&path.socket).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(matches!(
            StartupOwnership::acquire(&path.socket),
            Err(crate::startup::StartupError::LockHeld(_))
        ));

        shutdown.send(()).unwrap();
        server.await.unwrap().unwrap();
        assert!(!path.socket.exists());
        assert!(path.lock().exists());
        StartupOwnership::acquire(&path.socket).unwrap();
    }

    #[test]
    fn existing_file_symlink_and_socket_are_not_removed() {
        let regular = TestSocketPath::new("regular");
        std::fs::write(&regular.socket, b"owner data").unwrap();
        assert!(matches!(
            BoundSocket::bind(&regular.socket),
            Err(ServerError::ExistingPath(_))
        ));
        assert_eq!(std::fs::read(&regular.socket).unwrap(), b"owner data");

        let linked = TestSocketPath::new("symlink");
        let target = linked.directory.join("target");
        std::fs::write(&target, b"target data").unwrap();
        symlink(&target, &linked.socket).unwrap();
        assert!(matches!(
            BoundSocket::bind(&linked.socket),
            Err(ServerError::ExistingPath(_))
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"target data");
        std::fs::remove_file(&target).unwrap();

        let socket = TestSocketPath::new("existing-socket");
        let existing = std::os::unix::net::UnixListener::bind(&socket.socket).unwrap();
        assert!(matches!(
            BoundSocket::bind(&socket.socket),
            Err(ServerError::ExistingPath(_))
        ));
        assert!(
            std::fs::symlink_metadata(&socket.socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        drop(existing);
        assert!(matches!(
            BoundSocket::bind(&socket.socket),
            Err(ServerError::ExistingPath(_))
        ));
    }

    #[tokio::test]
    async fn cleanup_does_not_remove_a_replaced_path() {
        let path = TestSocketPath::new("replaced");
        let socket = BoundSocket::bind(&path.socket).unwrap();
        std::fs::remove_file(&path.socket).unwrap();
        std::fs::write(&path.socket, b"replacement").unwrap();

        assert!(matches!(
            socket.cleanup(),
            Err(ServerError::OwnershipChanged(_))
        ));
        assert_eq!(std::fs::read(&path.socket).unwrap(), b"replacement");
    }

    fn linux_payload(
        client_request_id: &str,
        program: &str,
        args: Vec<String>,
    ) -> SubmitTaskPayload {
        SubmitTaskPayload {
            client_request_id: client_request_id.to_owned(),
            command: CommandSpec {
                program: program.to_owned(),
                args,
                working_directory: "/".to_owned(),
                environment: BTreeMap::new(),
            },
            limits: ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 50_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 64 * 1024 * 1024,
                pids_max: 8,
                wall_time_limit_ms: 5_000,
            },
            output: OutputLimits {
                stdout_tail_max_bytes: 1_024,
                stderr_tail_max_bytes: 1_024,
            },
        }
    }

    async fn poll_finished(stream: &mut UnixStream, task_id: &str) -> TaskPayload {
        timeout(Duration::from_secs(5), async {
            loop {
                let response = exchange(
                    stream,
                    &Request::GetTask {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: new_task_id(),
                        payload: TaskIdPayload {
                            task_id: task_id.to_owned(),
                        },
                    },
                )
                .await;
                if let Response::Task {
                    payload: payload @ TaskPayload::Finished { .. },
                    ..
                } = response
                {
                    return payload;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("작업이 FINISHED가 돼야 합니다")
    }

    #[tokio::test]
    async fn actual_uds_server_runs_disconnect_poll_and_cancel_through_cgroups() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_UDS_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 Linux UDS와 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let path = TestSocketPath::new("actual-cgroup");
        let startup = StartupOwnership::acquire(&path.socket).unwrap();
        assert!(!path.socket.exists());
        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let handlers = Arc::new(
            ProtocolHandlers::initialize(
                Ok(environment),
                TaskCapacitySettings::new(2).unwrap(),
                FailStopCoordinator::new(
                    crate::fail_stop::FailStopSettings::new(Duration::from_secs(5)).unwrap(),
                ),
            )
            .unwrap(),
        );
        let server_handlers = Arc::clone(&handlers);
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let server = tokio::spawn(async move {
            serve_protocol_until(
                startup,
                Duration::from_secs(5),
                server_handlers,
                async move {
                    let _ = shutdown_receiver.await;
                    Ok(())
                },
            )
            .await
        });

        let mut first = connect(&path.socket).await;
        let capability = exchange(&mut first, &capability_request(FIRST_REQUEST_ID)).await;
        assert!(matches!(
            capability,
            Response::Capabilities {
                payload: CapabilitiesPayload {
                    cgroup_v2_ready: true,
                    max_concurrent_tasks: 2,
                    ..
                },
                ..
            }
        ));

        let disconnect_submit = exchange(
            &mut first,
            &Request::SubmitTask {
                protocol_version: PROTOCOL_VERSION,
                request_id: new_task_id(),
                payload: linux_payload(
                    "33333333-3333-3333-3333-333333333333",
                    "/bin/sleep",
                    vec!["0.2".to_owned()],
                ),
            },
        )
        .await;
        let disconnect_task_id = match disconnect_submit {
            Response::TaskAccepted { payload, .. } => payload.task_id,
            other => panic!("submitTask가 RUNNING을 반환해야 합니다: {other:?}"),
        };
        first.write_all(&[0, 0]).await.unwrap();
        drop(first);

        let mut polling = connect(&path.socket).await;
        assert!(matches!(
            poll_finished(&mut polling, &disconnect_task_id).await,
            TaskPayload::Finished {
                termination_reason: TerminationReason::Exited,
                process: ProcessResult {
                    exit_code: Some(0),
                    ..
                },
                ..
            }
        ));

        let cancel_submit = exchange(
            &mut polling,
            &Request::SubmitTask {
                protocol_version: PROTOCOL_VERSION,
                request_id: new_task_id(),
                payload: linux_payload(
                    "44444444-4444-4444-4444-444444444444",
                    "/bin/sleep",
                    vec!["30".to_owned()],
                ),
            },
        )
        .await;
        let cancel_task_id = match cancel_submit {
            Response::TaskAccepted {
                payload:
                    crate::protocol::TaskAcceptedPayload {
                        task_id,
                        state: TaskState::Running,
                        ..
                    },
                ..
            } => task_id,
            other => panic!("cancel 시험 작업이 RUNNING이어야 합니다: {other:?}"),
        };
        let cancelled = exchange(
            &mut polling,
            &Request::CancelTask {
                protocol_version: PROTOCOL_VERSION,
                request_id: new_task_id(),
                payload: TaskIdPayload {
                    task_id: cancel_task_id.clone(),
                },
            },
        )
        .await;
        assert!(matches!(cancelled, Response::TaskCancelled { .. }));
        assert!(matches!(
            poll_finished(&mut polling, &cancel_task_id).await,
            TaskPayload::Finished {
                termination_reason: TerminationReason::Cancelled,
                ..
            }
        ));
        drop(polling);

        let remaining_jobs = std::fs::read_dir(&jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(remaining_jobs, 0, "UDS 작업 cgroup이 남아 있습니다");

        shutdown_sender.send(()).unwrap();
        server.await.unwrap().unwrap();
        assert!(!path.socket.exists());
    }
}
