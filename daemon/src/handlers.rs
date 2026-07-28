//! protocol v1 typed 요청을 기존 capability, submit과 Registry 경계에 연결한다.

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use crate::capability::{CapabilityAdapter, CapabilityInitialization};
use crate::capacity::TaskCapacitySettings;
use crate::fail_stop::FailStopCoordinator;
use crate::preflight::{PreflightError, VerifiedEnvironment};
use crate::protocol::{
    ErrorCode, ErrorPayload, PROTOCOL_VERSION, Request, Response, TaskAcceptedPayload,
    TaskCancelledPayload, TaskPayload, TaskState, TerminationReason,
};
#[cfg(target_os = "linux")]
use crate::submit::SubmitCoordinator;
#[cfg(test)]
use crate::submit::TaskStartTime;
use crate::submit::{
    RegistryError, SubmitError, SubmitFailure, SubmitMetadata, SubmitObservation, SubmitOutcome,
    SubmitValidationError, ValidatedSubmit,
};

type FinishedTime = Box<dyn FnOnce() -> (String, Instant) + Send + 'static>;

/// task ID와 시각 생성은 environment gate를 통과한 뒤에만 이 값으로 확정한다.
pub(crate) struct SubmitContext {
    metadata: SubmitMetadata,
    finished_time: FinishedTime,
}

impl SubmitContext {
    pub(crate) fn new(metadata: SubmitMetadata, finished_time: FinishedTime) -> Self {
        Self {
            metadata,
            finished_time,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestHandling {
    Handled(Response),
    /// 개별 typed helper가 다른 요청 종류를 wire 오류로 바꾸지 않는다.
    Unhandled(Request),
}

pub(crate) trait ProtocolTaskCore {
    fn submit_validated(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        context: SubmitContext,
    ) -> impl Future<Output = Result<SubmitOutcome, SubmitError>> + Send;

    fn snapshot(&self, task_id: &str) -> Result<Option<TaskPayload>, RegistryError>;

    fn cancel(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<TaskPayload, RegistryError>> + Send;
}

#[cfg(target_os = "linux")]
impl ProtocolTaskCore for SubmitCoordinator {
    fn submit_validated(
        &self,
        request_id: String,
        validated: ValidatedSubmit,
        context: SubmitContext,
    ) -> impl Future<Output = Result<SubmitOutcome, SubmitError>> + Send {
        SubmitCoordinator::submit_validated(
            self,
            request_id,
            validated,
            context.metadata,
            context.finished_time,
        )
    }

    fn snapshot(&self, task_id: &str) -> Result<Option<TaskPayload>, RegistryError> {
        SubmitCoordinator::snapshot(self, task_id)
    }

    fn cancel(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<TaskPayload, RegistryError>> + Send {
        SubmitCoordinator::cancel(self, task_id)
    }
}

#[derive(Debug)]
enum HandlerState<C> {
    Ready {
        capabilities: CapabilityAdapter,
        core: C,
    },
    Unavailable {
        capabilities: CapabilityAdapter,
    },
}

#[derive(Debug)]
pub(crate) struct ProtocolHandlers<C> {
    state: HandlerState<C>,
    fail_stop: Arc<FailStopCoordinator>,
}

impl<C> ProtocolHandlers<C> {
    fn initialize_with<E, F>(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        capacity_settings: TaskCapacitySettings,
        fail_stop: Arc<FailStopCoordinator>,
        build_core: F,
    ) -> Result<Self, E>
    where
        F: FnOnce(VerifiedEnvironment, TaskCapacitySettings) -> Result<C, E>,
    {
        match CapabilityAdapter::from_preflight(preflight, capacity_settings) {
            CapabilityInitialization::Ready {
                adapter,
                environment,
            } => Ok(Self {
                state: HandlerState::Ready {
                    capabilities: adapter,
                    core: build_core(environment, capacity_settings)?,
                },
                fail_stop,
            }),
            CapabilityInitialization::Unavailable { adapter } => Ok(Self {
                state: HandlerState::Unavailable {
                    capabilities: adapter,
                },
                fail_stop,
            }),
        }
    }

    fn capabilities(&self) -> &CapabilityAdapter {
        match &self.state {
            HandlerState::Ready { capabilities, .. }
            | HandlerState::Unavailable { capabilities } => capabilities,
        }
    }

    pub(crate) fn handle_get_capabilities(&self, request: Request) -> RequestHandling {
        let Request::GetCapabilities {
            protocol_version,
            request_id,
            ..
        } = request
        else {
            return RequestHandling::Unhandled(request);
        };

        if let Some(error) = validate_envelope(protocol_version, &request_id) {
            return RequestHandling::Handled(error);
        }
        let mut payload = self.capabilities().payload();
        if self.fail_stop.is_fail_stopping() {
            payload.cgroup_v2_ready = false;
        }
        RequestHandling::Handled(Response::Capabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            payload,
        })
    }
}

#[cfg(target_os = "linux")]
impl ProtocolHandlers<SubmitCoordinator> {
    pub(crate) fn initialize(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        capacity_settings: TaskCapacitySettings,
        fail_stop: Arc<FailStopCoordinator>,
    ) -> crate::Result<Self> {
        let core_fail_stop = Arc::clone(&fail_stop);
        Self::initialize_with(
            preflight,
            capacity_settings,
            fail_stop,
            move |environment, settings| {
                SubmitCoordinator::initialize(environment, settings, core_fail_stop)
            },
        )
    }

    pub(crate) async fn wait_idle(&self) {
        if let HandlerState::Ready { core, .. } = &self.state {
            core.wait_idle().await;
        }
    }

    pub(crate) fn fail_stop(&self) -> &Arc<FailStopCoordinator> {
        &self.fail_stop
    }
}

impl<C> ProtocolHandlers<C>
where
    C: ProtocolTaskCore,
{
    /// 네 가지 protocol v1 요청을 모두 내부 typed handler 하나로 닫는다.
    pub(crate) async fn handle_request<F>(&self, request: Request, make_context: F) -> Response
    where
        F: FnOnce() -> SubmitContext,
    {
        let handling = match request {
            request @ Request::GetCapabilities { .. } => self.handle_get_capabilities(request),
            request @ Request::SubmitTask { .. } => self.handle_submit(request, make_context).await,
            request @ Request::GetTask { .. } => self.handle_get_task(request),
            request @ Request::CancelTask { .. } => self.handle_cancel(request).await,
        };
        match handling {
            RequestHandling::Handled(response) => response,
            RequestHandling::Unhandled(_) => {
                unreachable!("exhaustive dispatcher가 올바른 typed handler를 선택했습니다")
            }
        }
    }

    pub(crate) async fn handle_submit<F>(
        &self,
        request: Request,
        make_context: F,
    ) -> RequestHandling
    where
        F: FnOnce() -> SubmitContext,
    {
        if !matches!(&request, Request::SubmitTask { .. }) {
            return RequestHandling::Unhandled(request);
        }
        let request_id = request.request_id().to_owned();
        let (validated_request_id, validated) = match ValidatedSubmit::try_from_request(request) {
            Ok(validated) => validated,
            Err(error) => {
                return RequestHandling::Handled(submit_validation_error(request_id, error));
            }
        };
        debug_assert_eq!(validated_request_id, request_id);

        let core = match &self.state {
            HandlerState::Ready { core, .. } => core,
            HandlerState::Unavailable { .. } => {
                return RequestHandling::Handled(error_response(
                    request_id,
                    ErrorCode::EnvironmentUnavailable,
                    "cgroup v2 execution environment is unavailable",
                ));
            }
        };
        let outcome = core
            .submit_validated(validated_request_id, validated, make_context())
            .await;
        RequestHandling::Handled(match outcome {
            Ok(outcome) => submit_response(request_id, outcome),
            Err(error) => submit_error(request_id, error),
        })
    }

    pub(crate) fn handle_get_task(&self, request: Request) -> RequestHandling {
        let Request::GetTask {
            protocol_version,
            request_id,
            payload,
        } = request
        else {
            return RequestHandling::Unhandled(request);
        };

        if let Some(error) = validate_envelope(protocol_version, &request_id) {
            return RequestHandling::Handled(error);
        }
        let snapshot = match &self.state {
            HandlerState::Ready { core, .. } => core.snapshot(&payload.task_id),
            HandlerState::Unavailable { .. } => Ok(None),
        };
        RequestHandling::Handled(match snapshot {
            Ok(Some(payload)) => Response::Task {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                payload,
            },
            Ok(None) => error_response(
                request_id,
                ErrorCode::TaskNotFound,
                format!("task was not found: {}", payload.task_id),
            ),
            Err(error) => registry_error_response(request_id, error),
        })
    }

    pub(crate) async fn handle_cancel(&self, request: Request) -> RequestHandling {
        let Request::CancelTask {
            protocol_version,
            request_id,
            payload,
        } = request
        else {
            return RequestHandling::Unhandled(request);
        };

        if let Some(error) = validate_envelope(protocol_version, &request_id) {
            return RequestHandling::Handled(error);
        }
        let result = match &self.state {
            HandlerState::Ready { core, .. } => core.cancel(&payload.task_id).await,
            HandlerState::Unavailable { .. } => {
                Err(RegistryError::TaskNotFound(payload.task_id.clone()))
            }
        };
        RequestHandling::Handled(match result {
            Ok(TaskPayload::Finished {
                task_id,
                termination_reason: TerminationReason::Cancelled,
                ..
            }) => Response::TaskCancelled {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                payload: TaskCancelledPayload {
                    task_id,
                    state: TaskState::Finished,
                    termination_reason: TerminationReason::Cancelled,
                },
            },
            Ok(TaskPayload::Finished { task_id, .. }) => error_response(
                request_id,
                ErrorCode::TaskAlreadyFinished,
                format!("task is already finished: {task_id}"),
            ),
            Ok(TaskPayload::Running { task_id, .. }) => error_response(
                request_id,
                ErrorCode::InternalError,
                format!("cancel completed without a FINISHED result: {task_id}"),
            ),
            Err(error) => registry_error_response(request_id, error),
        })
    }
}

fn validate_envelope(protocol_version: u32, request_id: &str) -> Option<Response> {
    if protocol_version != PROTOCOL_VERSION {
        return Some(error_response(
            request_id.to_owned(),
            ErrorCode::UnsupportedProtocolVersion,
            format!("unsupported protocolVersion: {protocol_version}"),
        ));
    }
    if !is_uuid(request_id) {
        return Some(error_response(
            request_id.to_owned(),
            ErrorCode::InvalidRequest,
            "requestId must be a UUID",
        ));
    }
    None
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn submit_response(request_id: String, outcome: SubmitOutcome) -> Response {
    match outcome.observation {
        SubmitObservation::Task(TaskPayload::Running { .. }) => Response::TaskAccepted {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            payload: TaskAcceptedPayload {
                task_id: outcome.task_id,
                state: TaskState::Running,
                effective_limits: outcome
                    .effective_limits
                    .expect("RUNNING 응답에는 적용 확인된 effectiveLimits가 있어야 합니다")
                    .into_protocol(),
            },
        },
        SubmitObservation::Task(payload @ TaskPayload::Finished { .. }) => Response::Task {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            payload,
        },
        SubmitObservation::Failed(failure) => submit_failure_response(request_id, failure),
    }
}

fn submit_validation_error(request_id: String, error: SubmitValidationError) -> Response {
    let code = match &error {
        SubmitValidationError::UnsupportedProtocolVersion(_) => {
            ErrorCode::UnsupportedProtocolVersion
        }
        _ => ErrorCode::InvalidRequest,
    };
    error_response(request_id, code, error.to_string())
}

fn submit_error(request_id: String, error: SubmitError) -> Response {
    let code = match &error {
        SubmitError::Validation(SubmitValidationError::UnsupportedProtocolVersion(_)) => {
            ErrorCode::UnsupportedProtocolVersion
        }
        SubmitError::Validation(_) => ErrorCode::InvalidRequest,
        SubmitError::Registry(error) => registry_error_code(error),
        SubmitError::CoordinatorStopped => ErrorCode::InternalError,
    };
    error_response(request_id, code, error.to_string())
}

fn submit_failure_response(request_id: String, failure: SubmitFailure) -> Response {
    error_response(request_id, failure.code, failure.message)
}

fn registry_error_response(request_id: String, error: RegistryError) -> Response {
    let code = registry_error_code(&error);
    error_response(request_id, code, error.to_string())
}

fn registry_error_code(error: &RegistryError) -> ErrorCode {
    error.error_code().unwrap_or(ErrorCode::InternalError)
}

fn error_response(request_id: String, code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        payload: ErrorPayload {
            code,
            message: message.into(),
            retryable: matches!(code, ErrorCode::CapacityExhausted),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::convert::Infallible;
    #[cfg(target_os = "linux")]
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::Value;
    #[cfg(target_os = "linux")]
    use tokio::time::{Duration as TokioDuration, timeout};

    use super::*;
    #[cfg(target_os = "linux")]
    use crate::cgroup::CgroupCreateFaults;
    #[cfg(target_os = "linux")]
    use crate::preflight::{CapabilityProbe, SystemProbe};
    use crate::protocol::{
        CommandSpec, CpuMax, EmptyPayload, OutputLimits, ProcessResult, ResourceLimits,
        SubmitTaskPayload, TaskIdPayload, TaskOutput, TaskTiming, TaskUsage, TerminationReason,
    };

    const REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER_REQUEST_ID: &str = "77777777-7777-7777-7777-777777777777";
    const CLIENT_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";
    const TASK_ID: &str = "33333333-3333-3333-3333-333333333333";
    #[cfg(target_os = "linux")]
    const EXEC_FAILURE_CLIENT_REQUEST_ID: &str = "88888888-8888-8888-8888-888888888888";
    #[cfg(target_os = "linux")]
    const EXEC_FAILURE_TASK_ID: &str = "99999999-9999-9999-9999-999999999999";
    #[cfg(target_os = "linux")]
    const CANCEL_CLIENT_REQUEST_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    #[cfg(target_os = "linux")]
    const CANCEL_TASK_ID: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    #[cfg(target_os = "linux")]
    const CANCEL_REQUEST_ID: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";
    #[cfg(target_os = "linux")]
    const SECOND_CANCEL_REQUEST_ID: &str = "dddddddd-dddd-dddd-dddd-dddddddddddd";
    #[cfg(target_os = "linux")]
    const TIMEOUT_CLIENT_REQUEST_ID: &str = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
    #[cfg(target_os = "linux")]
    const TIMEOUT_TASK_ID: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";
    #[cfg(target_os = "linux")]
    const READ_BACK_TASK_ID: &str = "12121212-1212-1212-1212-121212121212";
    #[cfg(target_os = "linux")]
    const READ_BACK_RETRY_TASK_ID: &str = "13131313-1313-1313-1313-131313131313";
    #[cfg(target_os = "linux")]
    const READ_BACK_UNCERTAIN_TASK_ID: &str = "14141414-1414-1414-1414-141414141414";
    #[cfg(target_os = "linux")]
    const READ_BACK_CLIENT_REQUEST_ID: &str = "15151515-1515-1515-1515-151515151515";
    #[cfg(target_os = "linux")]
    const READ_BACK_UNCERTAIN_CLIENT_REQUEST_ID: &str = "16161616-1616-1616-1616-161616161616";

    #[derive(Debug, Default)]
    struct FakeCore {
        submit_result: Mutex<Option<Result<SubmitOutcome, SubmitError>>>,
        cancel_result: Mutex<Option<Result<TaskPayload, RegistryError>>>,
        snapshots: Mutex<HashMap<String, TaskPayload>>,
        submit_calls: AtomicUsize,
    }

    impl FakeCore {
        fn with_submit(result: Result<SubmitOutcome, SubmitError>) -> Self {
            Self {
                submit_result: Mutex::new(Some(result)),
                ..Self::default()
            }
        }

        fn with_snapshots(snapshots: impl IntoIterator<Item = (String, TaskPayload)>) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into_iter().collect()),
                ..Self::default()
            }
        }

        fn with_cancel(result: Result<TaskPayload, RegistryError>) -> Self {
            Self {
                cancel_result: Mutex::new(Some(result)),
                ..Self::default()
            }
        }
    }

    impl ProtocolTaskCore for FakeCore {
        fn submit_validated(
            &self,
            _request_id: String,
            _validated: ValidatedSubmit,
            context: SubmitContext,
        ) -> impl Future<Output = Result<SubmitOutcome, SubmitError>> + Send {
            let _ = (context.metadata, context.finished_time);
            self.submit_calls.fetch_add(1, Ordering::SeqCst);
            let result = self
                .submit_result
                .lock()
                .unwrap()
                .take()
                .expect("가짜 submit 결과가 필요합니다");
            async move { result }
        }

        fn snapshot(&self, task_id: &str) -> Result<Option<TaskPayload>, RegistryError> {
            Ok(self.snapshots.lock().unwrap().get(task_id).cloned())
        }

        fn cancel(
            &self,
            _task_id: &str,
        ) -> impl Future<Output = Result<TaskPayload, RegistryError>> + Send {
            let result = self
                .cancel_result
                .lock()
                .unwrap()
                .take()
                .expect("가짜 cancel 결과가 필요합니다");
            async move { result }
        }
    }

    fn ready_handlers(core: FakeCore, maximum: u32) -> ProtocolHandlers<FakeCore> {
        let fail_stop = test_fail_stop();
        ProtocolHandlers::initialize_with(
            Ok(VerifiedEnvironment::for_test()),
            TaskCapacitySettings::new(maximum).unwrap(),
            fail_stop,
            |environment, _| {
                assert_eq!(
                    environment.report().delegated_root.to_string_lossy(),
                    "/delegated"
                );
                Ok::<_, Infallible>(core)
            },
        )
        .unwrap()
    }

    fn test_fail_stop() -> Arc<FailStopCoordinator> {
        FailStopCoordinator::new(
            crate::fail_stop::FailStopSettings::new(Duration::from_secs(30)).unwrap(),
        )
    }

    fn unavailable_handlers() -> ProtocolHandlers<FakeCore> {
        let fail_stop = test_fail_stop();
        ProtocolHandlers::initialize_with(
            Err(PreflightError::MissingController {
                controller: "pids".to_owned(),
                path: "/delegated".into(),
            }),
            TaskCapacitySettings::new(2).unwrap(),
            fail_stop,
            |_environment, _| -> Result<FakeCore, Infallible> {
                panic!("preflight 실패에서는 실행 코어를 만들면 안 됩니다")
            },
        )
        .unwrap()
    }

    fn submit_payload() -> SubmitTaskPayload {
        SubmitTaskPayload {
            client_request_id: CLIENT_REQUEST_ID.to_owned(),
            command: CommandSpec {
                program: "/usr/bin/true".to_owned(),
                args: Vec::new(),
                working_directory: "/tmp".to_owned(),
                environment: BTreeMap::new(),
            },
            limits: ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 1,
                    period_micros: 1,
                },
                memory_max_bytes: 1,
                pids_max: 1,
                wall_time_limit_ms: 1,
            },
            output: OutputLimits {
                stdout_tail_max_bytes: 1,
                stderr_tail_max_bytes: 1,
            },
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_payload(
        client_request_id: &str,
        program: &str,
        args: &[String],
        wall_time_limit_ms: u64,
    ) -> SubmitTaskPayload {
        let mut payload = submit_payload();
        payload.client_request_id = client_request_id.to_owned();
        payload.command.program = program.to_owned();
        payload.command.args = args.to_vec();
        payload.command.working_directory = "/".to_owned();
        payload.limits.cpu_max.quota_micros = 50_000;
        payload.limits.cpu_max.period_micros = 100_000;
        payload.limits.memory_max_bytes = 64 * 1024 * 1024;
        payload.limits.pids_max = 8;
        payload.limits.wall_time_limit_ms = wall_time_limit_ms;
        payload.output.stdout_tail_max_bytes = 1_024;
        payload.output.stderr_tail_max_bytes = 1_024;
        payload
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_ghost_processes(path: &std::path::Path) -> (u32, u32) {
        timeout(TokioDuration::from_secs(5), async {
            loop {
                if let Ok(contents) = fs::read_to_string(path) {
                    let mut child = None;
                    let mut grandchild = None;
                    for line in contents.lines() {
                        if let Some(value) = line.strip_prefix("child=") {
                            child = value.parse().ok();
                        }
                        if let Some(value) = line.strip_prefix("grandchild=") {
                            grandchild = value.parse().ok();
                        }
                    }
                    if let (Some(child), Some(grandchild)) = (child, grandchild) {
                        return (child, grandchild);
                    }
                }
                tokio::time::sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("ghost child와 grandchild가 준비돼야 합니다")
    }

    #[cfg(target_os = "linux")]
    async fn assert_process_gone(pid: u32) {
        timeout(TokioDuration::from_secs(2), async {
            loop {
                let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
                if result == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return;
                }
                tokio::time::sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("취소 뒤 PID {pid}가 남아 있습니다"));
    }

    fn submit_request(request_id: &str, payload: SubmitTaskPayload) -> Request {
        Request::SubmitTask {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            payload,
        }
    }

    fn context() -> SubmitContext {
        context_for(TASK_ID)
    }

    fn context_for(task_id: &str) -> SubmitContext {
        SubmitContext::new(
            SubmitMetadata::fixed(
                task_id.to_owned(),
                "2026-07-20T09:00:00Z".to_owned(),
                || TaskStartTime::new("2026-07-20T09:00:00Z".to_owned(), Instant::now()),
                Duration::from_secs(5),
            ),
            Box::new(|| ("2026-07-20T09:00:01Z".to_owned(), Instant::now())),
        )
    }

    fn running() -> TaskPayload {
        running_for(TASK_ID)
    }

    fn running_for(task_id: &str) -> TaskPayload {
        TaskPayload::Running {
            task_id: task_id.to_owned(),
            submitted_at: "2026-07-20T09:00:00Z".to_owned(),
            started_at: "2026-07-20T09:00:00Z".to_owned(),
        }
    }

    fn finished_for(task_id: &str) -> TaskPayload {
        TaskPayload::Finished {
            task_id: task_id.to_owned(),
            termination_reason: TerminationReason::ExecutionFailed,
            process: ProcessResult {
                exit_code: None,
                signal: None,
            },
            timing: TaskTiming {
                submitted_at: "2026-07-20T09:00:00Z".to_owned(),
                started_at: "2026-07-20T09:00:00Z".to_owned(),
                finished_at: "2026-07-20T09:00:00Z".to_owned(),
                wall_time_ms: 12,
            },
            usage: TaskUsage {
                cpu_time_micros: 0,
                memory_peak_bytes: 0,
            },
            output: TaskOutput {
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        }
    }

    fn cancelled_for(task_id: &str) -> TaskPayload {
        let mut payload = finished_for(task_id);
        let TaskPayload::Finished {
            termination_reason,
            process,
            ..
        } = &mut payload
        else {
            unreachable!()
        };
        *termination_reason = TerminationReason::Cancelled;
        *process = ProcessResult {
            exit_code: None,
            signal: Some("SIGKILL".to_owned()),
        };
        payload
    }

    fn handled(handling: RequestHandling) -> Response {
        match handling {
            RequestHandling::Handled(response) => response,
            RequestHandling::Unhandled(request) => panic!("처리되지 않은 요청: {request:?}"),
        }
    }

    fn assert_error(response: Response, code: ErrorCode, retryable: bool) {
        assert!(matches!(
            response,
            Response::Error {
                protocol_version: PROTOCOL_VERSION,
                payload: ErrorPayload {
                    code: actual,
                    retryable: actual_retryable,
                    ..
                },
                ..
            } if actual == code && actual_retryable == retryable
        ));
    }

    #[test]
    fn capabilities_preserve_request_id_and_use_actual_readiness_and_capacity() {
        let handlers = ready_handlers(FakeCore::default(), 3);
        let response = handled(handlers.handle_get_capabilities(Request::GetCapabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id: REQUEST_ID.to_owned(),
            payload: EmptyPayload {},
        }));

        assert!(matches!(
            response,
            Response::Capabilities {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                payload,
            } if request_id == REQUEST_ID
                && payload.cgroup_v2_ready
                && payload.max_concurrent_tasks == 3
        ));

        let unavailable = unavailable_handlers();
        let response = handled(
            unavailable.handle_get_capabilities(Request::GetCapabilities {
                protocol_version: PROTOCOL_VERSION,
                request_id: OTHER_REQUEST_ID.to_owned(),
                payload: EmptyPayload {},
            }),
        );
        assert!(matches!(
            response,
            Response::Capabilities { request_id, payload, .. }
                if request_id == OTHER_REQUEST_ID && !payload.cgroup_v2_ready
        ));
    }

    #[test]
    fn fail_stop_makes_capability_unavailable_without_new_wire_fields() {
        let handlers = ready_handlers(FakeCore::default(), 3);
        handlers
            .fail_stop
            .activate(crate::fail_stop::CleanupFailureReport::new(
                TASK_ID,
                "시험 정리",
                vec!["작업 cgroup"],
                "실패",
            ));

        let response = handled(handlers.handle_get_capabilities(Request::GetCapabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id: REQUEST_ID.to_owned(),
            payload: EmptyPayload {},
        }));
        let Response::Capabilities { payload, .. } = response else {
            panic!("capabilities 응답이어야 합니다");
        };
        assert!(!payload.cgroup_v2_ready);
        let value = serde_json::to_value(payload).unwrap();
        let Value::Object(fields) = value else {
            panic!("capability payload는 object여야 합니다");
        };
        assert_eq!(fields.len(), 5);
    }

    #[tokio::test]
    async fn submit_running_matches_the_existing_task_accepted_fixture() {
        let fixture = include_str!("../../protocol-fixtures/v1/submit-task-valid.json");
        let request: Request = serde_json::from_str(fixture).unwrap();
        let expected_limits = match &request {
            Request::SubmitTask { payload, .. } => payload.limits.clone(),
            _ => unreachable!(),
        };
        let handlers = ready_handlers(
            FakeCore::with_submit(Ok(SubmitOutcome {
                request_id: "ignored-by-handler".to_owned(),
                task_id: TASK_ID.to_owned(),
                effective_limits: Some(crate::resource_budget::VerifiedEffectiveLimits::for_test(
                    expected_limits,
                )),
                observation: SubmitObservation::Task(running()),
            })),
            1,
        );

        let response = handled(handlers.handle_submit(request, context).await);
        let expected: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v1/task-accepted.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
    }

    #[tokio::test]
    async fn exec_start_failure_matches_the_existing_finished_fixture() {
        let response_fixture =
            include_str!("../../protocol-fixtures/v1/task-result-execution-failed.json");
        let expected: Response = serde_json::from_str(response_fixture).unwrap();
        let Response::Task { payload, .. } = &expected else {
            unreachable!();
        };
        let handlers = ready_handlers(
            FakeCore::with_submit(Ok(SubmitOutcome {
                request_id: OTHER_REQUEST_ID.to_owned(),
                task_id: TASK_ID.to_owned(),
                effective_limits: None,
                observation: SubmitObservation::Task(payload.clone()),
            })),
            1,
        );

        let response = handled(
            handlers
                .handle_submit(submit_request(OTHER_REQUEST_ID, submit_payload()), context)
                .await,
        );
        assert_eq!(response, expected);
    }

    #[test]
    fn get_task_returns_immutable_running_and_finished_snapshots_or_not_found() {
        let handlers = ready_handlers(
            FakeCore::with_snapshots([
                (TASK_ID.to_owned(), running()),
                (
                    "44444444-4444-4444-4444-444444444444".to_owned(),
                    finished_for("44444444-4444-4444-4444-444444444444"),
                ),
            ]),
            1,
        );

        for (request_id, task_id, expected) in [
            (REQUEST_ID, TASK_ID, running()),
            (
                OTHER_REQUEST_ID,
                "44444444-4444-4444-4444-444444444444",
                finished_for("44444444-4444-4444-4444-444444444444"),
            ),
        ] {
            let response = handled(handlers.handle_get_task(Request::GetTask {
                protocol_version: PROTOCOL_VERSION,
                request_id: request_id.to_owned(),
                payload: TaskIdPayload {
                    task_id: task_id.to_owned(),
                },
            }));
            assert_eq!(
                response,
                Response::Task {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: request_id.to_owned(),
                    payload: expected,
                }
            );
        }

        let missing = handled(handlers.handle_get_task(Request::GetTask {
            protocol_version: PROTOCOL_VERSION,
            request_id: REQUEST_ID.to_owned(),
            payload: TaskIdPayload {
                task_id: "55555555-5555-5555-5555-555555555555".to_owned(),
            },
        }));
        assert_error(missing, ErrorCode::TaskNotFound, false);
    }

    #[tokio::test]
    async fn capacity_and_idempotency_errors_keep_existing_codes() {
        let capacity = ready_handlers(
            FakeCore::with_submit(Ok(SubmitOutcome {
                request_id: OTHER_REQUEST_ID.to_owned(),
                task_id: TASK_ID.to_owned(),
                effective_limits: None,
                observation: SubmitObservation::Failed(SubmitFailure::new(
                    ErrorCode::CapacityExhausted,
                    "all task execution slots are in use",
                )),
            })),
            1,
        );
        let capacity_response = handled(
            capacity
                .handle_submit(submit_request(OTHER_REQUEST_ID, submit_payload()), context)
                .await,
        );
        let expected: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v1/error-capacity-exhausted.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(capacity_response).unwrap(), expected);

        let conflict = ready_handlers(
            FakeCore::with_submit(Err(SubmitError::Registry(
                RegistryError::IdempotencyConflict(CLIENT_REQUEST_ID.to_owned()),
            ))),
            1,
        );
        let conflict_response = handled(
            conflict
                .handle_submit(submit_request(REQUEST_ID, submit_payload()), context)
                .await,
        );
        assert_error(conflict_response, ErrorCode::IdempotencyConflict, false);
    }

    #[tokio::test]
    async fn unavailable_and_invalid_requests_do_not_create_submit_context() {
        let context_calls = AtomicUsize::new(0);
        let unavailable = unavailable_handlers();
        let unavailable_response = handled(
            unavailable
                .handle_submit(submit_request(REQUEST_ID, submit_payload()), || {
                    context_calls.fetch_add(1, Ordering::SeqCst);
                    context()
                })
                .await,
        );
        assert_error(
            unavailable_response,
            ErrorCode::EnvironmentUnavailable,
            false,
        );
        assert_eq!(context_calls.load(Ordering::SeqCst), 0);

        let ready = ready_handlers(FakeCore::default(), 1);
        let unsupported = Request::SubmitTask {
            protocol_version: 2,
            request_id: REQUEST_ID.to_owned(),
            payload: submit_payload(),
        };
        let response = handled(
            ready
                .handle_submit(unsupported, || {
                    context_calls.fetch_add(1, Ordering::SeqCst);
                    context()
                })
                .await,
        );
        assert_error(response, ErrorCode::UnsupportedProtocolVersion, false);
        assert_eq!(context_calls.load(Ordering::SeqCst), 0);

        let ready = ready_handlers(FakeCore::default(), 1);
        let mut invalid_payload = submit_payload();
        invalid_payload.limits.memory_max_bytes = 0;
        let response = handled(
            ready
                .handle_submit(submit_request(REQUEST_ID, invalid_payload), || {
                    context_calls.fetch_add(1, Ordering::SeqCst);
                    context()
                })
                .await,
        );
        assert_error(response, ErrorCode::InvalidRequest, false);
        assert_eq!(context_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn envelope_validation_preserves_request_id_and_rejects_bad_version_or_id() {
        let handlers = ready_handlers(FakeCore::default(), 1);
        let unsupported = handled(handlers.handle_get_capabilities(Request::GetCapabilities {
            protocol_version: 2,
            request_id: REQUEST_ID.to_owned(),
            payload: EmptyPayload {},
        }));
        assert!(matches!(
            unsupported,
            Response::Error { request_id, payload, .. }
                if request_id == REQUEST_ID
                    && payload.code == ErrorCode::UnsupportedProtocolVersion
        ));

        let invalid = handled(handlers.handle_get_task(Request::GetTask {
            protocol_version: PROTOCOL_VERSION,
            request_id: "not-a-uuid".to_owned(),
            payload: TaskIdPayload {
                task_id: TASK_ID.to_owned(),
            },
        }));
        assert!(matches!(
            invalid,
            Response::Error { request_id, payload, .. }
                if request_id == "not-a-uuid" && payload.code == ErrorCode::InvalidRequest
        ));
    }

    #[tokio::test]
    async fn cancel_returns_task_cancelled_only_for_a_stored_cancelled_result() {
        let handlers = ready_handlers(FakeCore::with_cancel(Ok(cancelled_for(TASK_ID))), 1);
        let response = handled(
            handlers
                .handle_cancel(Request::CancelTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: TASK_ID.to_owned(),
                    },
                })
                .await,
        );
        assert_eq!(
            response,
            Response::TaskCancelled {
                protocol_version: PROTOCOL_VERSION,
                request_id: REQUEST_ID.to_owned(),
                payload: TaskCancelledPayload {
                    task_id: TASK_ID.to_owned(),
                    state: TaskState::Finished,
                    termination_reason: TerminationReason::Cancelled,
                },
            }
        );

        let finished = ready_handlers(FakeCore::with_cancel(Ok(finished_for(TASK_ID))), 1);
        let response = handled(
            finished
                .handle_cancel(Request::CancelTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: OTHER_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: TASK_ID.to_owned(),
                    },
                })
                .await,
        );
        assert_error(response, ErrorCode::TaskAlreadyFinished, false);
    }

    #[tokio::test]
    async fn cancel_maps_missing_and_finished_registry_errors_without_calling_context() {
        for (error, expected) in [
            (
                RegistryError::TaskNotFound(TASK_ID.to_owned()),
                ErrorCode::TaskNotFound,
            ),
            (
                RegistryError::TaskAlreadyFinished(TASK_ID.to_owned()),
                ErrorCode::TaskAlreadyFinished,
            ),
        ] {
            let handlers = ready_handlers(FakeCore::with_cancel(Err(error)), 1);
            let response = handlers
                .handle_request(
                    Request::CancelTask {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: REQUEST_ID.to_owned(),
                        payload: TaskIdPayload {
                            task_id: TASK_ID.to_owned(),
                        },
                    },
                    || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
                )
                .await;
            assert_error(response, expected, false);
        }
    }

    #[tokio::test]
    async fn exhaustive_dispatcher_handles_all_four_protocol_requests() {
        let core = FakeCore {
            submit_result: Mutex::new(Some(Ok(SubmitOutcome {
                request_id: REQUEST_ID.to_owned(),
                task_id: TASK_ID.to_owned(),
                effective_limits: Some(crate::resource_budget::VerifiedEffectiveLimits::for_test(
                    submit_payload().limits,
                )),
                observation: SubmitObservation::Task(running()),
            }))),
            cancel_result: Mutex::new(Some(Ok(cancelled_for(TASK_ID)))),
            snapshots: Mutex::new(HashMap::from([(TASK_ID.to_owned(), running())])),
            submit_calls: AtomicUsize::new(0),
        };
        let handlers = ready_handlers(core, 1);

        assert!(matches!(
            handlers
                .handle_request(
                    Request::GetCapabilities {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: REQUEST_ID.to_owned(),
                        payload: EmptyPayload {},
                    },
                    || panic!("capability는 submit 문맥을 만들면 안 됩니다"),
                )
                .await,
            Response::Capabilities { .. }
        ));
        assert!(matches!(
            handlers
                .handle_request(submit_request(REQUEST_ID, submit_payload()), context,)
                .await,
            Response::TaskAccepted { .. }
        ));
        assert!(matches!(
            handlers
                .handle_request(
                    Request::GetTask {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: REQUEST_ID.to_owned(),
                        payload: TaskIdPayload {
                            task_id: TASK_ID.to_owned(),
                        },
                    },
                    || panic!("getTask는 submit 문맥을 만들면 안 됩니다"),
                )
                .await,
            Response::Task { .. }
        ));
        assert!(matches!(
            handlers
                .handle_request(
                    Request::CancelTask {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: REQUEST_ID.to_owned(),
                        payload: TaskIdPayload {
                            task_id: TASK_ID.to_owned(),
                        },
                    },
                    || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
                )
                .await,
            Response::TaskCancelled { .. }
        ));
    }

    #[tokio::test]
    async fn dispatcher_rejects_invalid_cancel_version_before_calling_core() {
        let handlers = ready_handlers(FakeCore::with_cancel(Ok(cancelled_for(TASK_ID))), 1);
        let response = handlers
            .handle_request(
                Request::CancelTask {
                    protocol_version: 2,
                    request_id: REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: TASK_ID.to_owned(),
                    },
                },
                || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
            )
            .await;
        assert_error(response, ErrorCode::UnsupportedProtocolVersion, false);
    }

    #[tokio::test]
    async fn typed_submit_handler_still_leaves_cancel_for_the_dispatcher() {
        let handlers = ready_handlers(FakeCore::default(), 1);
        let request = Request::CancelTask {
            protocol_version: PROTOCOL_VERSION,
            request_id: REQUEST_ID.to_owned(),
            payload: TaskIdPayload {
                task_id: TASK_ID.to_owned(),
            },
        };
        let context_calls = AtomicUsize::new(0);

        assert_eq!(
            handlers
                .handle_submit(request.clone(), || {
                    context_calls.fetch_add(1, Ordering::SeqCst);
                    context()
                })
                .await,
            RequestHandling::Unhandled(request)
        );
        assert_eq!(context_calls.load(Ordering::SeqCst), 0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_handlers_connect_submit_and_get_task_to_the_runner() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_HANDLER_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let handlers = ProtocolHandlers::initialize(
            Ok(environment),
            TaskCapacitySettings::new(1).unwrap(),
            test_fail_stop(),
        )
        .unwrap();

        let capabilities = handled(handlers.handle_get_capabilities(Request::GetCapabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id: REQUEST_ID.to_owned(),
            payload: EmptyPayload {},
        }));
        assert!(matches!(
            capabilities,
            Response::Capabilities { payload, .. }
                if payload.cgroup_v2_ready && payload.max_concurrent_tasks == 1
        ));

        let mut normal_payload = submit_payload();
        normal_payload.command.program = "/bin/true".to_owned();
        normal_payload.command.working_directory = "/".to_owned();
        normal_payload.limits.cpu_max.quota_micros = 50_000;
        normal_payload.limits.cpu_max.period_micros = 100_000;
        normal_payload.limits.memory_max_bytes = 64 * 1024 * 1024;
        normal_payload.limits.pids_max = 8;
        normal_payload.limits.wall_time_limit_ms = 5_000;
        normal_payload.output.stdout_tail_max_bytes = 1_024;
        normal_payload.output.stderr_tail_max_bytes = 1_024;

        let submitted = handled(
            handlers
                .handle_submit(submit_request(REQUEST_ID, normal_payload), || {
                    context_for(TASK_ID)
                })
                .await,
        );
        assert!(matches!(
            submitted,
            Response::TaskAccepted {
                request_id,
                payload: TaskAcceptedPayload {
                    state: TaskState::Running,
                    ..
                },
                ..
            } if request_id == REQUEST_ID
        ));

        let finished = timeout(TokioDuration::from_secs(5), async {
            loop {
                let response = handled(handlers.handle_get_task(Request::GetTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: OTHER_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: TASK_ID.to_owned(),
                    },
                }));
                if let Response::Task {
                    payload: payload @ TaskPayload::Finished { .. },
                    ..
                } = response
                {
                    break payload;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("정리 뒤 FINISHED를 getTask로 조회해야 합니다");
        assert!(matches!(
            finished,
            TaskPayload::Finished {
                termination_reason: TerminationReason::Exited,
                ..
            }
        ));

        let mut missing_payload = submit_payload();
        missing_payload.client_request_id = EXEC_FAILURE_CLIENT_REQUEST_ID.to_owned();
        missing_payload.command.program = "/definitely/missing/taskcage-target".to_owned();
        missing_payload.command.working_directory = "/".to_owned();
        missing_payload.limits.cpu_max.quota_micros = 50_000;
        missing_payload.limits.cpu_max.period_micros = 100_000;
        missing_payload.limits.memory_max_bytes = 64 * 1024 * 1024;
        missing_payload.limits.pids_max = 8;
        missing_payload.limits.wall_time_limit_ms = 5_000;
        missing_payload.output.stdout_tail_max_bytes = 1_024;
        missing_payload.output.stderr_tail_max_bytes = 1_024;
        let exec_failed = handled(
            handlers
                .handle_submit(submit_request(OTHER_REQUEST_ID, missing_payload), || {
                    context_for(EXEC_FAILURE_TASK_ID)
                })
                .await,
        );
        assert!(matches!(
            exec_failed,
            Response::Task {
                request_id,
                payload: TaskPayload::Finished {
                    termination_reason: TerminationReason::ExecutionFailed,
                    process: ProcessResult {
                        exit_code: None,
                        signal: None,
                    },
                    ..
                },
                ..
            } if request_id == OTHER_REQUEST_ID
        ));

        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(
            remaining_jobs, 0,
            "handler 실행 뒤 작업 cgroup이 남아 있습니다"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_read_back_mismatch_enforces_public_error_and_rollback_contract() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_READ_BACK_CONTRACT").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let marker_program = std::env::var("TASKCAGE_READ_BACK_MARKER_BIN").unwrap();
        let marker = std::env::temp_dir().join(format!(
            "taskcage-read-back-{}-success.marker",
            std::process::id()
        ));
        let uncertain_marker = std::env::temp_dir().join(format!(
            "taskcage-read-back-{}-uncertain.marker",
            std::process::id()
        ));
        let _ = fs::remove_file(&marker);
        let _ = fs::remove_file(&uncertain_marker);

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let fail_stop_clock_calls = Arc::new(AtomicUsize::new(0));
        let fail_stop_clock = {
            let calls = Arc::clone(&fail_stop_clock_calls);
            let now = Instant::now();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                now
            })
        };
        let fail_stop = FailStopCoordinator::with_test_clock(
            crate::fail_stop::FailStopSettings::new(Duration::from_secs(5)).unwrap(),
            fail_stop_clock,
        );
        let faults = Arc::new(CgroupCreateFaults::default());
        let core_fail_stop = Arc::clone(&fail_stop);
        let core_faults = Arc::clone(&faults);
        let handlers = ProtocolHandlers::initialize_with(
            Ok(environment),
            TaskCapacitySettings::new(1).unwrap(),
            Arc::clone(&fail_stop),
            move |environment, settings| {
                SubmitCoordinator::initialize_with_cgroup_create_faults(
                    environment,
                    settings,
                    core_fail_stop,
                    core_faults,
                )
            },
        )
        .unwrap();
        let HandlerState::Ready { core, .. } = &handlers.state else {
            panic!("read-back 계약 시험에는 준비된 실행 코어가 필요합니다");
        };

        let mut payload = submit_payload();
        payload.client_request_id = READ_BACK_CLIENT_REQUEST_ID.to_owned();
        payload.command.program = marker_program.clone();
        payload.command.args = vec![marker.to_string_lossy().into_owned()];
        payload.command.working_directory = "/".to_owned();
        payload.limits.cpu_max.quota_micros = 50_000;
        payload.limits.cpu_max.period_micros = 100_000;
        payload.limits.memory_max_bytes = 64 * 1024 * 1024;
        payload.limits.pids_max = 8;
        payload.limits.wall_time_limit_ms = 5_000;
        payload.output.stdout_tail_max_bytes = 1_024;
        payload.output.stderr_tail_max_bytes = 1_024;
        let expected_limits = payload.limits.clone();

        faults.inject_read_back_mismatch(false);
        let mismatch = handled(
            handlers
                .handle_submit(submit_request(REQUEST_ID, payload.clone()), || {
                    context_for(READ_BACK_TASK_ID)
                })
                .await,
        );
        let Response::Error {
            request_id,
            payload: error,
            ..
        } = &mismatch
        else {
            panic!("read-back 불일치는 error 응답이어야 합니다: {mismatch:?}");
        };
        assert_eq!(request_id, REQUEST_ID);
        assert_eq!(error.code, ErrorCode::InternalError);
        assert!(!error.retryable);
        assert_eq!(error.message, "cgroup limit read-back verification failed");
        let public_json = serde_json::to_string(&mismatch).unwrap();
        assert!(!public_json.contains(&jobs_path.to_string_lossy().into_owned()));
        assert!(!public_json.contains("injected-read-back-value"));
        assert!(!public_json.contains("effectiveLimits"));
        assert!(!public_json.contains("taskId"));
        let public_value = serde_json::to_value(&mismatch).unwrap();
        let error_fields = public_value
            .get("payload")
            .and_then(Value::as_object)
            .expect("error payload는 object여야 합니다");
        assert_eq!(error_fields.len(), 3);
        assert!(
            !marker.exists(),
            "read-back 실패 전에 target이 실행되면 안 됩니다"
        );
        assert_eq!(core.snapshot(READ_BACK_TASK_ID), Ok(None));
        assert_eq!(
            core.snapshot_by_client_request_id(READ_BACK_CLIENT_REQUEST_ID),
            Ok(None)
        );
        assert!(!jobs_path.join(format!("job-{READ_BACK_TASK_ID}")).exists());
        assert!(core.capacity_is_available_for_test());
        assert_eq!(faults.read_back_attempts(), 1);
        assert_eq!(faults.rollback_attempts(), 1);

        let retry = handled(
            handlers
                .handle_submit(submit_request(OTHER_REQUEST_ID, payload), || {
                    context_for(READ_BACK_RETRY_TASK_ID)
                })
                .await,
        );
        assert!(matches!(
            retry,
            Response::TaskAccepted {
                payload: TaskAcceptedPayload {
                    task_id,
                    effective_limits,
                    ..
                },
                ..
            } if task_id == READ_BACK_RETRY_TASK_ID && effective_limits == expected_limits
        ));
        timeout(TokioDuration::from_secs(5), async {
            loop {
                if matches!(
                    core.snapshot(READ_BACK_RETRY_TASK_ID),
                    Ok(Some(TaskPayload::Finished { .. }))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("재시도 작업은 정리된 FINISHED가 되어야 합니다");
        assert!(
            marker.exists(),
            "rollback 성공 뒤 동일 요청을 다시 실행해야 합니다"
        );
        assert!(core.capacity_is_available_for_test());
        assert!(
            !jobs_path
                .join(format!("job-{READ_BACK_RETRY_TASK_ID}"))
                .exists()
        );

        let mut uncertain_payload = submit_payload();
        uncertain_payload.client_request_id = READ_BACK_UNCERTAIN_CLIENT_REQUEST_ID.to_owned();
        uncertain_payload.command.program = marker_program;
        uncertain_payload.command.args = vec![uncertain_marker.to_string_lossy().into_owned()];
        uncertain_payload.command.working_directory = "/".to_owned();
        uncertain_payload.limits = expected_limits;
        uncertain_payload.output.stdout_tail_max_bytes = 1_024;
        uncertain_payload.output.stderr_tail_max_bytes = 1_024;

        faults.inject_read_back_mismatch(true);
        let uncertain = handled(
            handlers
                .handle_submit(
                    submit_request(REQUEST_ID, uncertain_payload.clone()),
                    || context_for(READ_BACK_UNCERTAIN_TASK_ID),
                )
                .await,
        );
        assert!(matches!(
            uncertain,
            Response::Error {
                payload: ErrorPayload {
                    code: ErrorCode::EnvironmentUnavailable,
                    retryable: false,
                    ..
                },
                ..
            }
        ));
        assert!(!uncertain_marker.exists());
        assert!(fail_stop.is_fail_stopping());
        assert_eq!(fail_stop_clock_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fail_stop.active_count(), 1);
        assert_eq!(core.retained_capacity_for_test(), 1);
        assert!(!core.capacity_is_available_for_test());
        assert!(
            jobs_path
                .join(format!("job-{READ_BACK_UNCERTAIN_TASK_ID}"))
                .exists()
        );
        assert_eq!(
            core.snapshot_by_client_request_id(READ_BACK_UNCERTAIN_CLIENT_REQUEST_ID),
            Ok(None)
        );
        let deadline = fail_stop.deadline().unwrap();
        let repeated = fail_stop.activate(crate::fail_stop::CleanupFailureReport::new(
            READ_BACK_UNCERTAIN_TASK_ID,
            "read-back rollback 재관찰",
            vec!["작업 cgroup"],
            "기존 deadline 유지",
        ));
        assert_eq!(deadline, repeated);
        assert_eq!(fail_stop_clock_calls.load(Ordering::SeqCst), 1);

        let capabilities = handled(handlers.handle_get_capabilities(Request::GetCapabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id: OTHER_REQUEST_ID.to_owned(),
            payload: EmptyPayload {},
        }));
        assert!(matches!(
            capabilities,
            Response::Capabilities { payload, .. } if !payload.cgroup_v2_ready
        ));
        let attempts_before_rejection = faults.read_back_attempts();
        let rejected = handled(
            handlers
                .handle_submit(submit_request(OTHER_REQUEST_ID, uncertain_payload), || {
                    context_for("17171717-1717-1717-1717-171717171717")
                })
                .await,
        );
        assert!(matches!(
            rejected,
            Response::Error {
                payload: ErrorPayload {
                    code: ErrorCode::EnvironmentUnavailable,
                    ..
                },
                ..
            }
        ));
        assert_eq!(faults.read_back_attempts(), attempts_before_rejection);
        assert!(!uncertain_marker.exists());
        assert_eq!(
            core.snapshot("17171717-1717-1717-1717-171717171717"),
            Ok(None)
        );

        fs::remove_dir(jobs_path.join(format!("job-{READ_BACK_UNCERTAIN_TASK_ID}"))).unwrap();
        fs::remove_file(marker).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_cancel_handler_cleans_descendants_and_preserves_timeout_winner() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_CANCELLATION_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let ghost_program = std::env::var("TASKCAGE_GHOST_BIN").unwrap();
        let ready_path =
            std::env::temp_dir().join(format!("taskcage-cancel-ready-{}", std::process::id()));
        let _ = fs::remove_file(&ready_path);

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let handlers = ProtocolHandlers::initialize(
            Ok(environment),
            TaskCapacitySettings::new(1).unwrap(),
            test_fail_stop(),
        )
        .unwrap();

        let ghost_payload = linux_payload(
            CANCEL_CLIENT_REQUEST_ID,
            &ghost_program,
            &[
                "--hold-parent".to_owned(),
                ready_path.to_string_lossy().into_owned(),
            ],
            30_000,
        );
        let submitted = handlers
            .handle_request(submit_request(REQUEST_ID, ghost_payload), || {
                context_for(CANCEL_TASK_ID)
            })
            .await;
        assert!(matches!(
            submitted,
            Response::TaskAccepted {
                payload: TaskAcceptedPayload {
                    state: TaskState::Running,
                    ..
                },
                ..
            }
        ));

        let (child_pid, grandchild_pid) = wait_for_ghost_processes(&ready_path).await;
        let first_cancel = handlers.handle_request(
            Request::CancelTask {
                protocol_version: PROTOCOL_VERSION,
                request_id: CANCEL_REQUEST_ID.to_owned(),
                payload: TaskIdPayload {
                    task_id: CANCEL_TASK_ID.to_owned(),
                },
            },
            || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
        );
        let second_cancel = handlers.handle_request(
            Request::CancelTask {
                protocol_version: PROTOCOL_VERSION,
                request_id: SECOND_CANCEL_REQUEST_ID.to_owned(),
                payload: TaskIdPayload {
                    task_id: CANCEL_TASK_ID.to_owned(),
                },
            },
            || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
        );
        let (first_cancel, second_cancel) = timeout(TokioDuration::from_secs(5), async {
            tokio::join!(first_cancel, second_cancel)
        })
        .await
        .expect("동시 cancel은 전체 정리 뒤 응답해야 합니다");
        for response in [first_cancel, second_cancel] {
            assert!(matches!(
                response,
                Response::TaskCancelled {
                    payload: TaskCancelledPayload {
                        state: TaskState::Finished,
                        termination_reason: TerminationReason::Cancelled,
                        ..
                    },
                    ..
                }
            ));
        }

        let cancelled = handlers
            .handle_request(
                Request::GetTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: OTHER_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: CANCEL_TASK_ID.to_owned(),
                    },
                },
                || panic!("getTask는 submit 문맥을 만들면 안 됩니다"),
            )
            .await;
        assert!(matches!(
            cancelled,
            Response::Task {
                payload: TaskPayload::Finished {
                    termination_reason: TerminationReason::Cancelled,
                    process: ProcessResult {
                        exit_code: None,
                        signal: Some(_),
                    },
                    ..
                },
                ..
            }
        ));
        assert_process_gone(child_pid).await;
        assert_process_gone(grandchild_pid).await;
        fs::remove_file(&ready_path).unwrap();

        let late_cancel = handlers
            .handle_request(
                Request::CancelTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: CANCEL_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: CANCEL_TASK_ID.to_owned(),
                    },
                },
                || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
            )
            .await;
        assert_error(late_cancel, ErrorCode::TaskAlreadyFinished, false);

        let missing_cancel = handlers
            .handle_request(
                Request::CancelTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: CANCEL_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: "abababab-abab-abab-abab-abababababab".to_owned(),
                    },
                },
                || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
            )
            .await;
        assert_error(missing_cancel, ErrorCode::TaskNotFound, false);

        let timeout_payload = linux_payload(
            TIMEOUT_CLIENT_REQUEST_ID,
            "/bin/sleep",
            &["30".to_owned()],
            100,
        );
        let timeout_submit = handlers
            .handle_request(submit_request(REQUEST_ID, timeout_payload), || {
                context_for(TIMEOUT_TASK_ID)
            })
            .await;
        assert!(matches!(timeout_submit, Response::TaskAccepted { .. }));
        tokio::time::sleep(TokioDuration::from_millis(200)).await;
        let timeout_cancel = handlers
            .handle_request(
                Request::CancelTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: CANCEL_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: TIMEOUT_TASK_ID.to_owned(),
                    },
                },
                || panic!("cancel은 submit 문맥을 만들면 안 됩니다"),
            )
            .await;
        assert_error(timeout_cancel, ErrorCode::TaskAlreadyFinished, false);

        let timed_out = handlers
            .handle_request(
                Request::GetTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: OTHER_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: TIMEOUT_TASK_ID.to_owned(),
                    },
                },
                || panic!("getTask는 submit 문맥을 만들면 안 됩니다"),
            )
            .await;
        assert!(matches!(
            timed_out,
            Response::Task {
                payload: TaskPayload::Finished {
                    termination_reason: TerminationReason::TimedOut,
                    ..
                },
                ..
            }
        ));

        let remaining_jobs = fs::read_dir(jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(
            remaining_jobs, 0,
            "cancel과 timeout 뒤 작업 cgroup이 남아 있습니다"
        );
    }
}
