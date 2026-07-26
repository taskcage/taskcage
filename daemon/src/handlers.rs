//! protocol v1 typed 요청을 기존 capability, submit과 Registry 경계에 연결한다.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "UDS listener가 다음 단계에서 typed handler를 호출합니다"
    )
)]

use std::future::Future;
use std::time::Instant;

use crate::capability::{CapabilityAdapter, CapabilityInitialization};
use crate::capacity::TaskCapacitySettings;
use crate::preflight::{PreflightError, VerifiedEnvironment};
use crate::protocol::{
    ErrorCode, ErrorPayload, PROTOCOL_VERSION, Request, Response, TaskAcceptedPayload, TaskPayload,
    TaskState,
};
#[cfg(target_os = "linux")]
use crate::submit::SubmitCoordinator;
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
    /// cancel handler가 추가될 때까지 유효한 요청을 wire 오류로 바꾸지 않는다.
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
}

impl<C> ProtocolHandlers<C> {
    fn initialize_with<E, F>(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        capacity_settings: TaskCapacitySettings,
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
            }),
            CapabilityInitialization::Unavailable { adapter } => Ok(Self {
                state: HandlerState::Unavailable {
                    capabilities: adapter,
                },
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
        RequestHandling::Handled(Response::Capabilities {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            payload: self.capabilities().payload(),
        })
    }
}

#[cfg(target_os = "linux")]
impl ProtocolHandlers<SubmitCoordinator> {
    #[allow(
        dead_code,
        reason = "UDS listener가 다음 단계에서 production handler를 초기화합니다"
    )]
    pub(crate) fn initialize(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        capacity_settings: TaskCapacitySettings,
    ) -> crate::Result<Self> {
        Self::initialize_with(preflight, capacity_settings, SubmitCoordinator::initialize)
    }
}

impl<C> ProtocolHandlers<C>
where
    C: ProtocolTaskCore,
{
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

        if let Err(code) = self.capabilities().submit_gate() {
            return RequestHandling::Handled(error_response(
                request_id,
                code,
                "cgroup v2 execution environment is unavailable",
            ));
        }

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
                effective_limits: outcome.effective_limits,
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::Value;
    #[cfg(target_os = "linux")]
    use tokio::time::{Duration as TokioDuration, timeout};

    use super::*;
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

    #[derive(Debug, Default)]
    struct FakeCore {
        submit_result: Mutex<Option<Result<SubmitOutcome, SubmitError>>>,
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
    }

    fn ready_handlers(core: FakeCore, maximum: u32) -> ProtocolHandlers<FakeCore> {
        ProtocolHandlers::initialize_with(
            Ok(VerifiedEnvironment::for_test()),
            TaskCapacitySettings::new(maximum).unwrap(),
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

    fn unavailable_handlers() -> ProtocolHandlers<FakeCore> {
        ProtocolHandlers::initialize_with(
            Err(PreflightError::MissingController {
                controller: "pids".to_owned(),
                path: "/delegated".into(),
            }),
            TaskCapacitySettings::new(2).unwrap(),
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
            SubmitMetadata {
                task_id: task_id.to_owned(),
                submitted_at: "2026-07-20T09:00:00Z".to_owned(),
                started_at: "2026-07-20T09:00:00Z".to_owned(),
                started_monotonic: Instant::now(),
                cleanup_timeout: Duration::from_secs(5),
            },
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
                effective_limits: expected_limits,
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
                effective_limits: submit_payload().limits.clone(),
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
                effective_limits: submit_payload().limits.clone(),
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

    #[test]
    fn cancel_request_stays_unhandled_for_the_next_typed_handler() {
        let handlers = ready_handlers(FakeCore::default(), 1);
        let request = Request::CancelTask {
            protocol_version: PROTOCOL_VERSION,
            request_id: REQUEST_ID.to_owned(),
            payload: TaskIdPayload {
                task_id: TASK_ID.to_owned(),
            },
        };

        assert_eq!(
            handlers.handle_get_capabilities(request.clone()),
            RequestHandling::Unhandled(request.clone())
        );
        assert_eq!(
            handlers.handle_get_task(request.clone()),
            RequestHandling::Unhandled(request)
        );
    }

    #[tokio::test]
    async fn submit_handler_does_not_consume_cancel_request_or_make_context() {
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
        let handlers =
            ProtocolHandlers::initialize(Ok(environment), TaskCapacitySettings::new(1).unwrap())
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
}
