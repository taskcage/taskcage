//! protocol v1 typed 요청을 기존 capability, submit과 Registry 경계에 연결한다.

#[cfg(test)]
use std::future::Future;
use std::sync::Arc;
#[cfg(test)]
use std::time::Instant;
#[cfg(target_os = "linux")]
use std::{collections::HashMap, fs::File, sync::Mutex};

use taskcage_core::task::{
    TaskSnapshot as TaskPayload, TerminationReason as DomainTerminationReason,
};

#[cfg(target_os = "linux")]
use crate::application::task::SubmitCoordinator;
#[cfg(test)]
use crate::application::task::SubmitMetadata;
#[cfg(target_os = "linux")]
use crate::application::task::TaskRegistrySettings;
#[cfg(test)]
use crate::application::task::TaskStartTime;
use crate::application::task::ports::TaskUseCases as ProtocolTaskCore;
use crate::application::task::{
    RegistryError, SubmitContext, SubmitError, SubmitFailure, SubmitObservation, SubmitOutcome,
    SubmitValidationError, ValidatedSubmit,
};
use crate::capability::{CapabilityAdapter, CapabilityInitialization};
use crate::capacity::TaskCapacitySettings;
use crate::deployment_policy::DeploymentResourcePolicy;
use crate::fail_stop::FailStopCoordinator;
use crate::preflight::{PreflightError, VerifiedEnvironment};
#[cfg(target_os = "linux")]
use crate::profile::{LocalProfileRuntime, ProfileReservation, ProfileTaskRecord};
use crate::protocol::{
    ErrorCode, ErrorPayload, PROTOCOL_VERSION, Request, Response, TaskAcceptedPayload,
    TaskCancelledPayload, TaskState,
};
#[cfg(target_os = "linux")]
use crate::protocol::{
    PROFILE_PROTOCOL_VERSION, ProfileAcceptedPayload, ProfileEffectiveResources,
};
use crate::protocol_mapper;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestHandling {
    Handled(Response),
    /// 개별 typed helper가 다른 요청 종류를 wire 오류로 바꾸지 않는다.
    Unhandled(Request),
}

#[derive(Debug)]
enum HandlerState<C> {
    Ready {
        capabilities: CapabilityAdapter,
        core: Arc<C>,
    },
    Unavailable {
        capabilities: CapabilityAdapter,
    },
}

#[derive(Debug)]
pub(crate) struct ProtocolHandlers<C> {
    state: HandlerState<C>,
    deployment_policy: DeploymentResourcePolicy,
    fail_stop: Arc<FailStopCoordinator>,
    #[cfg(target_os = "linux")]
    profile: Option<Arc<LocalProfileRuntime>>,
    #[cfg(target_os = "linux")]
    remote_tasks: Mutex<HashMap<String, String>>,
}

impl<C> ProtocolHandlers<C> {
    fn initialize_with<E, F>(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        capacity_settings: TaskCapacitySettings,
        deployment_policy: DeploymentResourcePolicy,
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
                    core: Arc::new(build_core(environment, capacity_settings)?),
                },
                deployment_policy,
                fail_stop,
                #[cfg(target_os = "linux")]
                profile: None,
                #[cfg(target_os = "linux")]
                remote_tasks: Mutex::new(HashMap::new()),
            }),
            CapabilityInitialization::Unavailable { adapter } => Ok(Self {
                state: HandlerState::Unavailable {
                    capabilities: adapter,
                },
                deployment_policy,
                fail_stop,
                #[cfg(target_os = "linux")]
                profile: None,
                #[cfg(target_os = "linux")]
                remote_tasks: Mutex::new(HashMap::new()),
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
        registry_settings: TaskRegistrySettings,
        deployment_policy: DeploymentResourcePolicy,
        fail_stop: Arc<FailStopCoordinator>,
        profile: Option<LocalProfileRuntime>,
    ) -> crate::Result<Self> {
        let core_fail_stop = Arc::clone(&fail_stop);
        let mut handlers = Self::initialize_with(
            preflight,
            capacity_settings,
            deployment_policy,
            fail_stop,
            move |environment, settings| {
                SubmitCoordinator::initialize(
                    environment,
                    settings,
                    registry_settings,
                    core_fail_stop,
                )
            },
        )?;
        handlers.profile = profile.map(Arc::new);
        Ok(handlers)
    }

    pub(crate) async fn wait_idle(&self) {
        if let HandlerState::Ready { core, .. } = &self.state {
            core.wait_idle().await;
        }
    }

    pub(crate) fn fail_stop(&self) -> &Arc<FailStopCoordinator> {
        &self.fail_stop
    }

    pub(crate) fn render_metrics(&self) -> String {
        match &self.state {
            HandlerState::Ready { core, .. } => core.render_metrics(),
            HandlerState::Unavailable { .. } => {
                crate::metrics::RuntimeMetrics::default().render(0, 0)
            }
        }
    }

    /// daemon production dispatcher다. Protocol v1 handler를 보존하면서 additive v2 Profile 요청만
    /// 별도 경계에서 받는다.
    pub(crate) async fn handle_daemon_request<F>(
        &self,
        request: Request,
        make_context: F,
    ) -> Response
    where
        F: FnOnce() -> SubmitContext,
    {
        if let Some(task_id) = task_id_for_visibility_check(&request)
            && self.is_remote_task(task_id)
        {
            return error_response_for(
                request.protocol_version(),
                request.request_id().to_owned(),
                ErrorCode::TaskNotFound,
                format!("task was not found: {task_id}"),
            );
        }
        match request {
            request @ Request::GetCapabilities {
                protocol_version: PROTOCOL_VERSION,
                ..
            } => self.profile_capabilities_response(request),
            request @ Request::SubmitProfile { .. } => {
                self.handle_submit_profile(request, make_context).await
            }
            request @ Request::GetProfileResult { .. } => {
                self.handle_get_profile_result(request).await
            }
            request @ Request::GetCapabilities { .. }
            | request @ Request::SubmitTask { .. }
            | request @ Request::GetTask { .. }
            | request @ Request::CancelTask { .. } => {
                if request.protocol_version() == PROFILE_PROTOCOL_VERSION {
                    error_response_for(
                        PROFILE_PROTOCOL_VERSION,
                        request.request_id().to_owned(),
                        ErrorCode::InvalidRequest,
                        "protocol v2 only supports submitProfile and getProfileResult",
                    )
                } else {
                    self.handle_request(request, make_context).await
                }
            }
        }
    }

    fn profile_capabilities_response(&self, request: Request) -> Response {
        let mut response = match self.handle_get_capabilities(request) {
            RequestHandling::Handled(response) => response,
            RequestHandling::Unhandled(_) => unreachable!("capabilities request must be handled"),
        };
        if self.profile.is_some()
            && !self.fail_stop.is_fail_stopping()
            && let Response::Capabilities { payload, .. } = &mut response
        {
            payload.protocol_versions.push(PROFILE_PROTOCOL_VERSION);
        }
        response
    }

    pub(crate) async fn handle_submit_profile<F>(
        &self,
        request: Request,
        make_context: F,
    ) -> Response
    where
        F: FnOnce() -> SubmitContext,
    {
        self.handle_submit_profile_with_source(request, make_context, None, None)
            .await
    }

    pub(crate) async fn handle_submit_remote_profile<F>(
        &self,
        request: Request,
        make_context: F,
        source: File,
        principal: String,
    ) -> Response
    where
        F: FnOnce() -> SubmitContext,
    {
        self.handle_submit_profile_with_source(request, make_context, Some(source), Some(principal))
            .await
    }

    async fn handle_submit_profile_with_source<F>(
        &self,
        request: Request,
        make_context: F,
        daemon_source: Option<File>,
        remote_principal: Option<String>,
    ) -> Response
    where
        F: FnOnce() -> SubmitContext,
    {
        let Request::SubmitProfile {
            protocol_version,
            request_id,
            payload,
        } = request
        else {
            unreachable!("profile submit dispatcher must preserve request kind")
        };
        if let Some(response) = validate_profile_envelope(protocol_version, &request_id) {
            return response;
        }
        let Some(runtime) = &self.profile else {
            return error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id,
                ErrorCode::InvalidRequest,
                "protocol v2 local profiles are not enabled",
            );
        };
        let prepared = match runtime.validate(&payload) {
            Ok(prepared) => prepared,
            Err(error) => return profile_error_response(request_id, error),
        };
        if let Err(error) = self.deployment_policy.validate(prepared.budget()) {
            return error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id,
                ErrorCode::LimitExceedsPolicy,
                error.to_string(),
            );
        }
        let core = match &self.state {
            HandlerState::Ready { core, .. } => Arc::clone(core),
            HandlerState::Unavailable { .. } => {
                return error_response_for(
                    PROFILE_PROTOCOL_VERSION,
                    request_id,
                    ErrorCode::EnvironmentUnavailable,
                    "cgroup v2 execution environment is unavailable",
                );
            }
        };
        if let Err(error) = runtime.prune_missing_tasks(|task_id| {
            core.snapshot(task_id)
                .map(|snapshot| snapshot.is_some())
                .map_err(|error| error.to_string())
        }) {
            return profile_error_response(request_id, error);
        }
        let reservation = match runtime.reserve(payload).await {
            Ok(ProfileReservation::Existing(task)) => {
                return existing_profile_response(&core, runtime, request_id, task).await;
            }
            Ok(reservation) => reservation,
            Err(error) => return profile_error_response(request_id, error),
        };
        match core.has_client_request_id(reservation_client_request_id(&reservation)) {
            Ok(true) => {
                runtime.release(reservation);
                return error_response_for(
                    PROFILE_PROTOCOL_VERSION,
                    request_id,
                    ErrorCode::IdempotencyConflict,
                    "clientRequestId was already used for a Raw Command request",
                );
            }
            Ok(false) => {}
            Err(error) => {
                runtime.release(reservation);
                return error_response_for(
                    PROFILE_PROTOCOL_VERSION,
                    request_id,
                    ErrorCode::InternalError,
                    error.to_string(),
                );
            }
        }
        let mut context = make_context();
        let task_id = context.preallocate_task_id();
        let staged = match daemon_source {
            Some(source) => runtime.stage_daemon_input(&task_id, prepared, source),
            None => runtime.stage(&task_id, prepared),
        };
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                runtime.release(reservation);
                return profile_error_response(request_id, error);
            }
        };
        let (profile_request, budget, staged_artifacts, plan, output_slot) = staged.into_plan();
        let profile_task = runtime.new_task(&task_id, &profile_request, budget, output_slot);
        if let Some(principal) = &remote_principal {
            self.register_remote_task(task_id.clone(), principal.clone());
        }
        let (metadata, finished_time) = context.into_parts();
        let outcome = core
            .submit_profile_validated(
                request_id.clone(),
                ValidatedSubmit::from_profile(profile_request, plan),
                metadata,
                finished_time,
                Arc::clone(&profile_task),
                staged_artifacts,
            )
            .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                runtime.release(reservation);
                if submit_error_code(&error) != ErrorCode::InternalError
                    && let Some(principal) = &remote_principal
                {
                    self.remove_remote_task_if_owned(&task_id, principal);
                }
                return error_response_for(
                    PROFILE_PROTOCOL_VERSION,
                    request_id,
                    submit_error_code(&error),
                    error.to_string(),
                );
            }
        };
        let SubmitOutcome {
            task_id,
            effective_limits,
            observation,
            ..
        } = outcome;
        match observation {
            SubmitObservation::Task(TaskPayload::Running { .. }) => {
                let task = match runtime.accept(reservation, Arc::clone(&profile_task)) {
                    Ok(task) => task,
                    Err(error) => {
                        return profile_error_response(request_id, error);
                    }
                };
                let limits = effective_limits
                    .expect("RUNNING profile response requires verified effective limits")
                    .into_protocol();
                Response::ProfileAccepted {
                    protocol_version: PROFILE_PROTOCOL_VERSION,
                    request_id,
                    payload: ProfileAcceptedPayload {
                        task_id,
                        state: TaskState::Running,
                        profile: task.profile().clone(),
                        effective_resources: ProfileEffectiveResources {
                            limits,
                            output: task.budget().protocol_output(),
                        },
                    },
                }
            }
            SubmitObservation::Task(payload @ TaskPayload::Finished { .. }) => {
                let task = match runtime.accept(reservation, Arc::clone(&profile_task)) {
                    Ok(task) => task,
                    Err(error) => {
                        return profile_error_response(request_id, error);
                    }
                };
                Response::ProfileResult {
                    protocol_version: PROFILE_PROTOCOL_VERSION,
                    request_id,
                    payload: task.snapshot(payload).await,
                }
            }
            SubmitObservation::Failed(failure) => {
                runtime.release(reservation);
                if failure.code != ErrorCode::InternalError
                    && let Some(principal) = &remote_principal
                {
                    self.remove_remote_task_if_owned(&task_id, principal);
                }
                error_response_for(
                    PROFILE_PROTOCOL_VERSION,
                    request_id,
                    failure.code,
                    failure.message,
                )
            }
        }
    }

    pub(crate) async fn handle_get_profile_result(&self, request: Request) -> Response {
        let Request::GetProfileResult {
            protocol_version,
            request_id,
            payload,
        } = request
        else {
            unreachable!("profile result dispatcher must preserve request kind")
        };
        if let Some(response) = validate_profile_envelope(protocol_version, &request_id) {
            return response;
        }
        let Some(runtime) = &self.profile else {
            return error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id,
                ErrorCode::TaskNotFound,
                format!("task was not found: {}", payload.task_id),
            );
        };
        let task = match runtime.task(&payload.task_id) {
            Ok(Some(task)) => task,
            Ok(None) => match &self.state {
                HandlerState::Ready { core, .. } => match core.snapshot(&payload.task_id) {
                    Ok(Some(_)) => {
                        return error_response_for(
                            PROFILE_PROTOCOL_VERSION,
                            request_id,
                            ErrorCode::TaskKindMismatch,
                            format!("task is not a Profile Task: {}", payload.task_id),
                        );
                    }
                    Ok(None) => {
                        return error_response_for(
                            PROFILE_PROTOCOL_VERSION,
                            request_id,
                            ErrorCode::TaskNotFound,
                            format!("task was not found: {}", payload.task_id),
                        );
                    }
                    Err(error) => {
                        return error_response_for(
                            PROFILE_PROTOCOL_VERSION,
                            request_id,
                            ErrorCode::InternalError,
                            error.to_string(),
                        );
                    }
                },
                HandlerState::Unavailable { .. } => {
                    return error_response_for(
                        PROFILE_PROTOCOL_VERSION,
                        request_id,
                        ErrorCode::TaskNotFound,
                        format!("task was not found: {}", payload.task_id),
                    );
                }
            },
            Err(error) => return profile_error_response(request_id, error),
        };
        let raw = match &self.state {
            HandlerState::Ready { core, .. } => core.snapshot(&payload.task_id),
            HandlerState::Unavailable { .. } => Ok(None),
        };
        match raw {
            Ok(Some(raw)) => Response::ProfileResult {
                protocol_version: PROFILE_PROTOCOL_VERSION,
                request_id,
                payload: task.snapshot(raw).await,
            },
            Ok(None) => match runtime.discard_task(&payload.task_id) {
                Ok(()) => error_response_for(
                    PROFILE_PROTOCOL_VERSION,
                    request_id,
                    ErrorCode::TaskNotFound,
                    format!("task was not found: {}", payload.task_id),
                ),
                Err(error) => profile_error_response(request_id, error),
            },
            Err(error) => error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id,
                ErrorCode::InternalError,
                error.to_string(),
            ),
        }
    }

    pub(crate) fn register_remote_task(&self, task_id: String, principal: String) {
        self.remote_tasks
            .lock()
            .expect("remote task owner state poisoned")
            .insert(task_id, principal);
    }

    pub(crate) fn prevalidate_remote_profile(
        &self,
        request_id: &str,
        payload: &crate::protocol::ProfileRequestPayload,
    ) -> Option<Response> {
        let Some(runtime) = &self.profile else {
            return Some(error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id.to_owned(),
                ErrorCode::ProfileNotFound,
                "daemon-installed profiles are not enabled",
            ));
        };
        let prepared = match runtime.validate(payload) {
            Ok(prepared) => prepared,
            Err(error) => return Some(profile_error_response(request_id.to_owned(), error)),
        };
        if let Err(error) = self.deployment_policy.validate(prepared.budget()) {
            return Some(error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id.to_owned(),
                ErrorCode::LimitExceedsPolicy,
                error.to_string(),
            ));
        }
        if matches!(self.state, HandlerState::Unavailable { .. }) {
            return Some(error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id.to_owned(),
                ErrorCode::EnvironmentUnavailable,
                "cgroup v2 execution environment is unavailable",
            ));
        }
        None
    }

    pub(crate) fn remote_task_owned_by(&self, task_id: &str, principal: &str) -> bool {
        self.remote_tasks
            .lock()
            .expect("remote task owner state poisoned")
            .get(task_id)
            .is_some_and(|owner| owner == principal)
    }

    pub(crate) fn remove_remote_task_if_owned(&self, task_id: &str, principal: &str) {
        let mut remote_tasks = self
            .remote_tasks
            .lock()
            .expect("remote task owner state poisoned");
        if remote_tasks
            .get(task_id)
            .is_some_and(|owner| owner == principal)
        {
            remote_tasks.remove(task_id);
        }
    }

    pub(crate) fn open_remote_profile_output(&self, path: &str) -> Result<File, String> {
        self.profile
            .as_ref()
            .ok_or_else(|| "daemon-installed profiles are not enabled".to_owned())?
            .open_published_artifact(path)
            .map_err(|error| error.to_string())
    }

    fn is_remote_task(&self, task_id: &str) -> bool {
        self.remote_tasks
            .lock()
            .expect("remote task owner state poisoned")
            .contains_key(task_id)
    }
}

#[cfg(target_os = "linux")]
fn task_id_for_visibility_check(request: &Request) -> Option<&str> {
    match request {
        Request::GetTask { payload, .. }
        | Request::CancelTask { payload, .. }
        | Request::GetProfileResult { payload, .. } => Some(&payload.task_id),
        Request::GetCapabilities { .. }
        | Request::SubmitTask { .. }
        | Request::SubmitProfile { .. } => None,
    }
}

#[cfg(target_os = "linux")]
fn validate_profile_envelope(protocol_version: u32, request_id: &str) -> Option<Response> {
    if protocol_version != PROFILE_PROTOCOL_VERSION {
        let code = if protocol_version == PROTOCOL_VERSION {
            ErrorCode::InvalidRequest
        } else {
            ErrorCode::UnsupportedProtocolVersion
        };
        return Some(error_response_for(
            PROFILE_PROTOCOL_VERSION,
            request_id.to_owned(),
            code,
            format!("unsupported protocolVersion: {protocol_version}"),
        ));
    }
    if !is_uuid(request_id) {
        return Some(error_response_for(
            PROFILE_PROTOCOL_VERSION,
            request_id.to_owned(),
            ErrorCode::InvalidRequest,
            "requestId must be a UUID",
        ));
    }
    None
}

#[cfg(target_os = "linux")]
fn profile_error_response(request_id: String, error: crate::profile::ProfileError) -> Response {
    error_response_for(
        PROFILE_PROTOCOL_VERSION,
        request_id,
        error.code(),
        error.to_string(),
    )
}

#[cfg(target_os = "linux")]
fn error_response_for(
    protocol_version: u32,
    request_id: String,
    code: ErrorCode,
    message: impl Into<String>,
) -> Response {
    Response::Error {
        protocol_version,
        request_id,
        payload: ErrorPayload {
            code,
            message: message.into(),
            retryable: matches!(code, ErrorCode::CapacityExhausted),
        },
    }
}

#[cfg(target_os = "linux")]
fn reservation_client_request_id(reservation: &ProfileReservation) -> &str {
    match reservation {
        ProfileReservation::Owner {
            client_request_id, ..
        } => client_request_id,
        ProfileReservation::Existing(_) => {
            unreachable!("existing profile reservation is handled before staging")
        }
    }
}

#[cfg(target_os = "linux")]
async fn existing_profile_response(
    core: &Arc<SubmitCoordinator>,
    runtime: &LocalProfileRuntime,
    request_id: String,
    task: Arc<ProfileTaskRecord>,
) -> Response {
    let raw = core.snapshot(task.task_id());
    match raw {
        Ok(Some(raw)) => Response::ProfileResult {
            protocol_version: PROFILE_PROTOCOL_VERSION,
            request_id,
            payload: task.snapshot(raw).await,
        },
        Ok(None) => match runtime.discard_task(task.task_id()) {
            Ok(()) => error_response_for(
                PROFILE_PROTOCOL_VERSION,
                request_id,
                ErrorCode::TaskNotFound,
                format!("task was not found: {}", task.task_id()),
            ),
            Err(error) => profile_error_response(request_id, error),
        },
        Err(error) => error_response_for(
            PROFILE_PROTOCOL_VERSION,
            request_id,
            ErrorCode::InternalError,
            error.to_string(),
        ),
    }
}

#[cfg(target_os = "linux")]
fn submit_error_code(error: &SubmitError) -> ErrorCode {
    match error {
        SubmitError::Validation(SubmitValidationError::UnsupportedProtocolVersion(_)) => {
            ErrorCode::UnsupportedProtocolVersion
        }
        SubmitError::Validation(_) => ErrorCode::InvalidRequest,
        SubmitError::Registry(error) => registry_error_code(error),
        SubmitError::CoordinatorStopped => ErrorCode::InternalError,
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
            Request::SubmitProfile { request_id, .. }
            | Request::GetProfileResult { request_id, .. } => {
                RequestHandling::Handled(error_response(
                    request_id,
                    ErrorCode::InvalidRequest,
                    "Profile requests require the daemon v2 dispatcher",
                ))
            }
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
        if let Err(error) = self.deployment_policy.validate(validated.budget()) {
            return RequestHandling::Handled(error_response(
                request_id,
                ErrorCode::LimitExceedsPolicy,
                error.to_string(),
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
                payload: protocol_mapper::task_snapshot(payload),
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
                termination_reason: DomainTerminationReason::Cancelled,
                ..
            }) => Response::TaskCancelled {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                payload: TaskCancelledPayload {
                    task_id,
                    state: TaskState::Finished,
                    termination_reason: crate::protocol::TerminationReason::Cancelled,
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
            payload: protocol_mapper::task_snapshot(payload),
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
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "linux")]
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::Value;
    #[cfg(target_os = "linux")]
    use sha2::{Digest, Sha256};
    use taskcage_core::task::{
        ProcessResult, TaskOutput, TaskTiming, TaskUsage, TerminationReason,
    };
    #[cfg(target_os = "linux")]
    use tokio::time::{Duration as TokioDuration, timeout};

    use super::*;
    #[cfg(target_os = "linux")]
    use crate::cgroup::CgroupCreateFaults;
    #[cfg(target_os = "linux")]
    use crate::preflight::{CapabilityProbe, SystemProbe};
    #[cfg(target_os = "linux")]
    use crate::profile::{
        FFMPEG_PACKAGE_ENTRYPOINT, FFMPEG_PACKAGE_ID, FFMPEG_PROFILE_NAME, FFMPEG_PROFILE_VERSION,
        FILE_COPY_PROFILE_NAME, FILE_COPY_PROFILE_VERSION,
    };
    use crate::protocol::{
        CommandSpec, CpuMax, EmptyPayload, OutputLimits, ResourceLimits, SubmitTaskPayload,
        TaskIdPayload,
    };
    #[cfg(target_os = "linux")]
    use crate::resource_budget::ResourceBudget;
    #[cfg(target_os = "linux")]
    use crate::runtime_package::import_for_service_uid;

    const REQUEST_ID: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER_REQUEST_ID: &str = "77777777-7777-7777-7777-777777777777";
    const CLIENT_REQUEST_ID: &str = "22222222-2222-2222-2222-222222222222";
    #[cfg(target_os = "linux")]
    const INVALID_PROFILE_CLIENT_REQUEST_ID: &str = "33333333-3333-3333-3333-333333333333";
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
            let _ = context.into_parts();
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
        ready_handlers_with_policy(core, maximum, DeploymentResourcePolicy::for_test())
    }

    fn ready_handlers_with_policy(
        core: FakeCore,
        maximum: u32,
        deployment_policy: DeploymentResourcePolicy,
    ) -> ProtocolHandlers<FakeCore> {
        let fail_stop = test_fail_stop();
        ProtocolHandlers::initialize_with(
            Ok(VerifiedEnvironment::for_test()),
            TaskCapacitySettings::new(maximum).unwrap(),
            deployment_policy,
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
            DeploymentResourcePolicy::for_test(),
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

    #[cfg(target_os = "linux")]
    fn profile_request(
        request_id: &str,
        client_request_id: &str,
        digest: String,
        size_bytes: u64,
    ) -> Request {
        Request::SubmitProfile {
            protocol_version: PROFILE_PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            payload: crate::protocol::ProfileRequestPayload {
                client_request_id: client_request_id.to_owned(),
                profile: crate::protocol::ProfileIdentity {
                    name: FILE_COPY_PROFILE_NAME.to_owned(),
                    version: FILE_COPY_PROFILE_VERSION.to_owned(),
                },
                inputs: BTreeMap::from([
                    (
                        "source".to_owned(),
                        crate::protocol::ProfileInputValue::LocalInput {
                            path: "jobs/42/source.txt".to_owned(),
                            digest,
                            size_bytes,
                        },
                    ),
                    (
                        "label".to_owned(),
                        crate::protocol::ProfileInputValue::String {
                            value: "archive".to_owned(),
                        },
                    ),
                    (
                        "retain_metadata".to_owned(),
                        crate::protocol::ProfileInputValue::Boolean { value: true },
                    ),
                    (
                        "priority".to_owned(),
                        crate::protocol::ProfileInputValue::Int64 { value: 3 },
                    ),
                ]),
                resource_overrides: None,
            },
        }
    }

    #[cfg(target_os = "linux")]
    fn ffmpeg_profile_request(
        request_id: &str,
        client_request_id: &str,
        source_path: &str,
        digest: String,
        size_bytes: u64,
        wall_time_limit_ms: Option<u64>,
    ) -> Request {
        Request::SubmitProfile {
            protocol_version: PROFILE_PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            payload: crate::protocol::ProfileRequestPayload {
                client_request_id: client_request_id.to_owned(),
                profile: crate::protocol::ProfileIdentity {
                    name: FFMPEG_PROFILE_NAME.to_owned(),
                    version: FFMPEG_PROFILE_VERSION.to_owned(),
                },
                inputs: BTreeMap::from([
                    (
                        "source".to_owned(),
                        crate::protocol::ProfileInputValue::LocalInput {
                            path: source_path.to_owned(),
                            digest,
                            size_bytes,
                        },
                    ),
                    (
                        "sample_rate_hz".to_owned(),
                        crate::protocol::ProfileInputValue::Int64 { value: 16_000 },
                    ),
                    (
                        "channels".to_owned(),
                        crate::protocol::ProfileInputValue::Int64 { value: 1 },
                    ),
                ]),
                resource_overrides: wall_time_limit_ms.map(|value| {
                    crate::protocol::ProfileResourceOverrides {
                        limits: Some(crate::protocol::PartialResourceLimits {
                            wall_time_limit_ms: Some(value),
                            ..crate::protocol::PartialResourceLimits::default()
                        }),
                        output: None,
                    }
                }),
            },
        }
    }

    #[cfg(target_os = "linux")]
    fn import_ffmpeg_package(
        root: &Path,
        executable: &Path,
    ) -> (PathBuf, crate::digest::Sha256Digest) {
        let executable_bytes = fs::read(executable).expect("FFmpeg package entrypoint 읽기");
        let sbom = br#"{"spdxVersion":"SPDX-2.3"}"#;
        let source = root.join("package-source");
        let cache = root.join("package-cache");
        let entrypoint = source.join("rootfs").join(FFMPEG_PACKAGE_ENTRYPOINT);
        let sbom_path = source.join("rootfs/share/sbom.spdx.json");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::create_dir_all(sbom_path.parent().unwrap()).unwrap();
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&entrypoint, &executable_bytes).unwrap();
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o555)).unwrap();
        fs::write(&sbom_path, sbom).unwrap();
        fs::set_permissions(&sbom_path, fs::Permissions::from_mode(0o444)).unwrap();
        let digest_text = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
        let manifest = serde_json::json!({
            "schemaVersion": "taskcage.runtime-package/v0alpha1",
            "id": FFMPEG_PACKAGE_ID,
            "version": "0.0.0-test.1",
            "platform": {
                "os": "linux",
                "architecture": std::env::consts::ARCH,
                "abi": "gnu",
                "libc": { "family": "glibc", "minimumVersion": "2.17" }
            },
            "entrypoint": FFMPEG_PACKAGE_ENTRYPOINT,
            "libraryPaths": [],
            "files": [
                {
                    "path": FFMPEG_PACKAGE_ENTRYPOINT,
                    "digest": digest_text(&executable_bytes),
                    "sizeBytes": executable_bytes.len(),
                    "mode": "0555"
                },
                {
                    "path": "share/sbom.spdx.json",
                    "digest": digest_text(sbom),
                    "sizeBytes": sbom.len(),
                    "mode": "0444"
                }
            ],
            "licenses": [],
            "sbom": { "format": "SPDX-JSON-2.3", "path": "share/sbom.spdx.json" }
        });
        fs::write(
            source.join("runtime-package.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let report = import_for_service_uid(&cache, &source).expect("service UID package import");
        (cache, report.digest)
    }

    #[cfg(target_os = "linux")]
    fn write_profile_input(
        artifact_root: &Path,
        relative_path: &str,
        contents: &[u8],
    ) -> (String, u64) {
        let path = artifact_root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        (
            crate::digest::Sha256Digest::from_bytes(Sha256::digest(contents).into()).to_string(),
            u64::try_from(contents.len()).unwrap(),
        )
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_profile_result(
        handlers: &ProtocolHandlers<SubmitCoordinator>,
        task_id: &str,
    ) -> crate::protocol::ProfileTaskPayload {
        timeout(TokioDuration::from_secs(10), async {
            loop {
                let response = handlers
                    .handle_daemon_request(
                        Request::GetProfileResult {
                            protocol_version: PROFILE_PROTOCOL_VERSION,
                            request_id: OTHER_REQUEST_ID.to_owned(),
                            payload: TaskIdPayload {
                                task_id: task_id.to_owned(),
                            },
                        },
                        context,
                    )
                    .await;
                if let Response::ProfileResult {
                    payload: payload @ crate::protocol::ProfileTaskPayload::Finished { .. },
                    ..
                } = response
                {
                    return payload;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Profile Task가 cleanup-confirmed 결과로 끝나야 합니다")
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_child_marker(path: &Path) -> u32 {
        timeout(TokioDuration::from_secs(5), async {
            loop {
                if let Ok(contents) = fs::read_to_string(path)
                    && let Some(value) = contents.trim().strip_prefix("child_pid=")
                    && let Ok(pid) = value.parse()
                {
                    return pid;
                }
                tokio::time::sleep(TokioDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake FFmpeg child PID가 준비돼야 합니다")
    }

    #[cfg(target_os = "linux")]
    fn make_test_tree_writable(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(path)? {
                make_test_tree_writable(&entry?.path())?;
            }
        } else {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn profile_budget() -> ResourceBudget {
        ResourceBudget::try_from_protocol(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 50_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 64 * 1024 * 1024,
                pids_max: 8,
                wall_time_limit_ms: 5_000,
            },
            OutputLimits {
                stdout_tail_max_bytes: 1_024,
                stderr_tail_max_bytes: 1_024,
            },
        )
        .expect("file-copy Profile test budget")
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

    #[cfg(target_os = "linux")]
    fn assert_profile_error(response: Response, code: ErrorCode, retryable: bool) {
        assert!(matches!(
            response,
            Response::Error {
                protocol_version: PROFILE_PROTOCOL_VERSION,
                payload: ErrorPayload {
                    code: actual,
                    retryable: actual_retryable,
                    ..
                },
                ..
            } if actual == code && actual_retryable == retryable
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn profile_policy_error_contract_is_not_retryable() {
        let response = error_response_for(
            PROFILE_PROTOCOL_VERSION,
            REQUEST_ID.to_owned(),
            ErrorCode::LimitExceedsPolicy,
            "Bundle resource override exceeds policy",
        );

        assert_profile_error(response, ErrorCode::LimitExceedsPolicy, false);
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
                observation: SubmitObservation::Task(protocol_mapper::task_snapshot_from_protocol(
                    payload,
                )),
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
                    payload: protocol_mapper::task_snapshot(expected),
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

        let registry_capacity = ready_handlers(
            FakeCore::with_submit(Err(SubmitError::Registry(RegistryError::CapacityExhausted))),
            1,
        );
        let registry_capacity_response = handled(
            registry_capacity
                .handle_submit(submit_request(REQUEST_ID, submit_payload()), context)
                .await,
        );
        assert!(matches!(
            registry_capacity_response,
            Response::Error {
                request_id,
                payload: ErrorPayload {
                    code: ErrorCode::CapacityExhausted,
                    message,
                    retryable: true,
                },
                ..
            } if request_id == REQUEST_ID
                && message == "task registry retention capacity is exhausted"
        ));

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
                    termination_reason: crate::protocol::TerminationReason::Cancelled,
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

    #[tokio::test]
    async fn deployment_policy_rejects_before_context_and_core_side_effects() {
        let policy = DeploymentResourcePolicy::try_new(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 1,
                    period_micros: 1,
                },
                memory_max_bytes: 1,
                pids_max: 1,
                wall_time_limit_ms: 1,
            },
            OutputLimits {
                stdout_tail_max_bytes: 1,
                stderr_tail_max_bytes: 1,
            },
        )
        .unwrap();
        let handlers = ready_handlers_with_policy(FakeCore::default(), 1, policy);
        let mut payload = submit_payload();
        payload.limits.memory_max_bytes = 2;
        let context_calls = AtomicUsize::new(0);

        let response = handled(
            handlers
                .handle_submit(submit_request(REQUEST_ID, payload), || {
                    context_calls.fetch_add(1, Ordering::SeqCst);
                    context()
                })
                .await,
        );

        assert_error(response, ErrorCode::LimitExceedsPolicy, false);
        assert_eq!(context_calls.load(Ordering::SeqCst), 0);
        let HandlerState::Ready { core, .. } = &handlers.state else {
            panic!("policy 시험에는 준비된 core가 필요합니다");
        };
        assert_eq!(core.submit_calls.load(Ordering::SeqCst), 0);
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
            TaskRegistrySettings::new(16).unwrap(),
            DeploymentResourcePolicy::for_test(),
            test_fail_stop(),
            None,
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
                    payload: payload @ crate::protocol::TaskPayload::Finished { .. },
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
            crate::protocol::TaskPayload::Finished {
                termination_reason: crate::protocol::TerminationReason::Exited,
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
                payload: crate::protocol::TaskPayload::Finished {
                    termination_reason: crate::protocol::TerminationReason::ExecutionFailed,
                    process: crate::protocol::ProcessResult {
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
    async fn actual_profile_handler_snapshots_runs_and_publishes_after_cleanup() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_PROFILE_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let root =
            std::env::temp_dir().join(format!("taskcage-profile-artifacts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("jobs")).unwrap();
        fs::create_dir(root.join("jobs/42")).unwrap();
        let source_bytes = b"TaskCage profile input\n";
        fs::write(root.join("jobs/42/source.txt"), source_bytes).unwrap();
        let digest = crate::digest::Sha256Digest::from_bytes(Sha256::digest(source_bytes).into());

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let profile_runtime =
            LocalProfileRuntime::open(&root, 1_024 * 1_024, profile_budget(), None, None)
                .expect("safe local Artifact root enables file-copy");
        let handlers = ProtocolHandlers::initialize(
            Ok(environment),
            TaskCapacitySettings::new(1).unwrap(),
            TaskRegistrySettings::new(16).unwrap(),
            DeploymentResourcePolicy::for_test(),
            test_fail_stop(),
            Some(profile_runtime),
        )
        .unwrap();

        let capabilities = handlers
            .handle_daemon_request(
                Request::GetCapabilities {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: REQUEST_ID.to_owned(),
                    payload: EmptyPayload {},
                },
                context,
            )
            .await;
        assert!(matches!(
            capabilities,
            Response::Capabilities { payload, .. }
                if payload.cgroup_v2_ready && payload.protocol_versions == vec![1, 2]
        ));

        let mut invalid = profile_request(
            REQUEST_ID,
            INVALID_PROFILE_CLIENT_REQUEST_ID,
            digest.to_string(),
            source_bytes.len() as u64,
        );
        let Request::SubmitProfile { payload, .. } = &mut invalid else {
            unreachable!("test helper must build a Profile request")
        };
        payload.inputs.insert(
            "priority".to_owned(),
            crate::protocol::ProfileInputValue::String {
                value: "not-an-int64".to_owned(),
            },
        );
        let rejected = handlers
            .handle_daemon_request(invalid, || {
                panic!("invalid Profile input must not allocate a task context")
            })
            .await;
        assert_profile_error(rejected, ErrorCode::InvalidProfileInput, false);
        let HandlerState::Ready { core, .. } = &handlers.state else {
            panic!("Profile integration requires a ready daemon")
        };
        assert!(
            core.snapshot_by_client_request_id(INVALID_PROFILE_CLIENT_REQUEST_ID)
                .unwrap()
                .is_none(),
            "invalid Profile input must not reserve a Raw Task"
        );
        assert_eq!(
            fs::read_dir(&jobs_path)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
                .count(),
            0,
            "invalid Profile input must not create a task cgroup"
        );

        let submitted = handlers
            .handle_daemon_request(
                profile_request(
                    REQUEST_ID,
                    CLIENT_REQUEST_ID,
                    digest.to_string(),
                    source_bytes.len() as u64,
                ),
                || context_for(TASK_ID),
            )
            .await;
        match submitted {
            Response::ProfileAccepted {
                protocol_version: PROFILE_PROTOCOL_VERSION,
                payload:
                    ProfileAcceptedPayload {
                        task_id,
                        state: TaskState::Running,
                        profile,
                        ..
                    },
                ..
            }
            | Response::ProfileResult {
                protocol_version: PROFILE_PROTOCOL_VERSION,
                payload:
                    crate::protocol::ProfileTaskPayload::Finished {
                        task_id, profile, ..
                    },
                ..
            } if task_id == TASK_ID
                && profile.name == FILE_COPY_PROFILE_NAME
                && profile.version == FILE_COPY_PROFILE_VERSION => {}
            response => panic!("unexpected file-copy submit response: {response:#?}"),
        }

        let finished = timeout(TokioDuration::from_secs(5), async {
            loop {
                let response = handlers
                    .handle_daemon_request(
                        Request::GetProfileResult {
                            protocol_version: PROFILE_PROTOCOL_VERSION,
                            request_id: OTHER_REQUEST_ID.to_owned(),
                            payload: TaskIdPayload {
                                task_id: TASK_ID.to_owned(),
                            },
                        },
                        context,
                    )
                    .await;
                if let Response::ProfileResult {
                    payload: payload @ crate::protocol::ProfileTaskPayload::Finished { .. },
                    ..
                } = response
                {
                    break payload;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("profile result must finish after runner cleanup");
        assert!(matches!(
            finished,
            crate::protocol::ProfileTaskPayload::Finished {
                profile_outcome: crate::protocol::ProfileOutcome::Succeeded,
                termination_reason: crate::protocol::TerminationReason::Exited,
                ref artifacts,
                failure: None,
                ..
            } if artifacts.len() == 1
        ));
        assert_eq!(
            fs::read(root.join(format!("tasks/{TASK_ID}/result.txt"))).unwrap(),
            source_bytes
        );
        assert!(
            !root.join(format!(".taskcage/staging/{TASK_ID}")).exists(),
            "published Profile result must remove staging"
        );
        let raw_snapshot = handled(handlers.handle_get_task(Request::GetTask {
            protocol_version: PROTOCOL_VERSION,
            request_id: OTHER_REQUEST_ID.to_owned(),
            payload: TaskIdPayload {
                task_id: TASK_ID.to_owned(),
            },
        }));
        assert!(matches!(
            raw_snapshot,
            Response::Task {
                payload: crate::protocol::TaskPayload::Finished {
                    termination_reason: crate::protocol::TerminationReason::Exited,
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
        assert_eq!(remaining_jobs, 0, "Profile 뒤 작업 cgroup이 남아 있습니다");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_ffmpeg_profile_uses_pinned_package_and_cleans_failure_timeout_and_cancel() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_FFMPEG_PROFILE_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경과 fake FFmpeg fixture가 필요합니다");
            return;
        }

        const SUCCESS_TASK: &str = "41414141-4141-4141-8141-414141414141";
        const FAILURE_TASK: &str = "42424242-4242-4242-8242-424242424242";
        const TIMEOUT_TASK: &str = "43434343-4343-4343-8343-434343434343";
        const CANCEL_TASK: &str = "44444444-4444-4444-8444-444444444444";
        let fake_ffmpeg = PathBuf::from(
            std::env::var_os("TASKCAGE_FAKE_FFMPEG_BIN")
                .expect("TASKCAGE_FAKE_FFMPEG_BIN이 필요합니다"),
        );
        let root = std::env::temp_dir().join(format!(
            "taskcage-ffmpeg-profile-integration-{}",
            std::process::id()
        ));
        let _ = make_test_tree_writable(&root);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let artifacts = root.join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
        let (cache, package_digest) = import_ffmpeg_package(&root, &fake_ffmpeg);

        let (success_digest, success_size) =
            write_profile_input(&artifacts, "jobs/success/source.txt", b"SUCCESS\n");
        let (failure_digest, failure_size) =
            write_profile_input(&artifacts, "jobs/failure/source.txt", b"FAIL\n");
        let (timeout_digest, timeout_size) =
            write_profile_input(&artifacts, "jobs/timeout/source.txt", b"HANG\n");
        let (cancel_digest, cancel_size) =
            write_profile_input(&artifacts, "jobs/cancel/source.txt", b"HANG\n");

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let profile_runtime = LocalProfileRuntime::open(
            &artifacts,
            16 * 1024 * 1024,
            profile_budget(),
            Some((&cache, package_digest)),
            None,
        )
        .expect("service UID가 import한 FFmpeg package를 daemon이 resolve해야 합니다");
        let handlers = ProtocolHandlers::initialize(
            Ok(environment),
            TaskCapacitySettings::new(1).unwrap(),
            TaskRegistrySettings::new(32).unwrap(),
            DeploymentResourcePolicy::for_test(),
            test_fail_stop(),
            Some(profile_runtime),
        )
        .unwrap();

        let success_submit = handlers
            .handle_daemon_request(
                ffmpeg_profile_request(
                    REQUEST_ID,
                    "51515151-5151-4151-8151-515151515151",
                    "jobs/success/source.txt",
                    success_digest,
                    success_size,
                    None,
                ),
                || context_for(SUCCESS_TASK),
            )
            .await;
        assert!(matches!(
            success_submit,
            Response::ProfileAccepted { .. } | Response::ProfileResult { .. }
        ));
        let success = wait_for_profile_result(&handlers, SUCCESS_TASK).await;
        let crate::protocol::ProfileTaskPayload::Finished {
            profile_outcome: crate::protocol::ProfileOutcome::Succeeded,
            termination_reason: crate::protocol::TerminationReason::Exited,
            process,
            artifacts: published,
            failure: None,
            ..
        } = success
        else {
            panic!("FFmpeg success result가 필요합니다")
        };
        assert_eq!(process.exit_code, Some(0));
        assert_eq!(published.len(), 1);
        let audio = published.get("audio").expect("audio output slot");
        assert_eq!(audio.media_type, "audio/wav");
        assert_eq!(audio.path, format!("tasks/{SUCCESS_TASK}/result.wav"));
        assert_eq!(
            fs::read(artifacts.join(&audio.path)).unwrap(),
            b"RIFFtaskcageWAVE"
        );

        let failure_submit = handlers
            .handle_daemon_request(
                ffmpeg_profile_request(
                    REQUEST_ID,
                    "52525252-5252-4252-8252-525252525252",
                    "jobs/failure/source.txt",
                    failure_digest,
                    failure_size,
                    None,
                ),
                || context_for(FAILURE_TASK),
            )
            .await;
        assert!(matches!(
            failure_submit,
            Response::ProfileAccepted { .. } | Response::ProfileResult { .. }
        ));
        let failure = wait_for_profile_result(&handlers, FAILURE_TASK).await;
        assert!(matches!(
            failure,
            crate::protocol::ProfileTaskPayload::Finished {
                profile_outcome: crate::protocol::ProfileOutcome::Failed,
                termination_reason: crate::protocol::TerminationReason::Exited,
                ref artifacts,
                failure: Some(crate::protocol::ProfileFailurePayload { ref code, .. }),
                ..
            } if artifacts.is_empty() && code == "PROCESS_EXITED_NONZERO"
        ));
        assert!(
            !artifacts
                .join(format!("tasks/{FAILURE_TASK}/result.wav"))
                .exists()
        );

        let timeout_submit = handlers
            .handle_daemon_request(
                ffmpeg_profile_request(
                    REQUEST_ID,
                    "53535353-5353-4353-8353-535353535353",
                    "jobs/timeout/source.txt",
                    timeout_digest,
                    timeout_size,
                    Some(100),
                ),
                || context_for(TIMEOUT_TASK),
            )
            .await;
        assert!(matches!(
            timeout_submit,
            Response::ProfileAccepted { .. } | Response::ProfileResult { .. }
        ));
        let timeout_result = wait_for_profile_result(&handlers, TIMEOUT_TASK).await;
        let crate::protocol::ProfileTaskPayload::Finished {
            profile_outcome: crate::protocol::ProfileOutcome::Failed,
            termination_reason: crate::protocol::TerminationReason::TimedOut,
            output,
            artifacts: timeout_artifacts,
            failure: Some(timeout_failure),
            ..
        } = timeout_result
        else {
            panic!("FFmpeg timeout result가 필요합니다")
        };
        assert!(timeout_artifacts.is_empty());
        assert_eq!(timeout_failure.code, "TIMED_OUT");
        let timeout_child = output
            .stdout_tail
            .lines()
            .find_map(|line| line.strip_prefix("child_pid="))
            .and_then(|value| value.parse::<u32>().ok())
            .expect("timeout output에 child PID가 있어야 합니다");
        assert_process_gone(timeout_child).await;

        let cancel_submit = handlers
            .handle_daemon_request(
                ffmpeg_profile_request(
                    REQUEST_ID,
                    "54545454-5454-4454-8454-545454545454",
                    "jobs/cancel/source.txt",
                    cancel_digest,
                    cancel_size,
                    None,
                ),
                || context_for(CANCEL_TASK),
            )
            .await;
        assert!(matches!(cancel_submit, Response::ProfileAccepted { .. }));
        let cancel_marker = artifacts
            .join(".taskcage/staging")
            .join(CANCEL_TASK)
            .join("artifacts/out/result.wav");
        let cancel_child = wait_for_child_marker(&cancel_marker).await;
        let cancelled = handlers
            .handle_daemon_request(
                Request::CancelTask {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: CANCEL_REQUEST_ID.to_owned(),
                    payload: TaskIdPayload {
                        task_id: CANCEL_TASK.to_owned(),
                    },
                },
                context,
            )
            .await;
        assert!(matches!(
            cancelled,
            Response::TaskCancelled {
                payload: TaskCancelledPayload {
                    termination_reason: crate::protocol::TerminationReason::Cancelled,
                    ..
                },
                ..
            }
        ));
        let cancel_result = wait_for_profile_result(&handlers, CANCEL_TASK).await;
        assert!(matches!(
            cancel_result,
            crate::protocol::ProfileTaskPayload::Finished {
                profile_outcome: crate::protocol::ProfileOutcome::Failed,
                termination_reason: crate::protocol::TerminationReason::Cancelled,
                ref artifacts,
                failure: Some(crate::protocol::ProfileFailurePayload { ref code, .. }),
                ..
            } if artifacts.is_empty() && code == "CANCELLED"
        ));
        assert_process_gone(cancel_child).await;

        let remaining_jobs = fs::read_dir(&jobs_path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
            .count();
        assert_eq!(
            remaining_jobs, 0,
            "FFmpeg Profile 뒤 task cgroup이 남아 있습니다"
        );
        assert!(
            !artifacts
                .join(".taskcage/staging")
                .join(TIMEOUT_TASK)
                .exists()
        );
        assert!(
            !artifacts
                .join(".taskcage/staging")
                .join(CANCEL_TASK)
                .exists()
        );

        make_test_tree_writable(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_real_ffmpeg_profile_imports_and_resolves_under_service_uid() {
        if std::env::var_os("TASKCAGE_RUN_REAL_FFMPEG_PROFILE_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 FFmpeg와 service UID cgroup 환경이 필요합니다");
            return;
        }
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "실제 Runtime Package 시험은 daemon service UID로 실행해야 합니다"
        );

        const REAL_TASK: &str = "61616161-6161-4161-8161-616161616161";
        let ffmpeg = PathBuf::from(
            std::env::var_os("TASKCAGE_REAL_FFMPEG_BIN")
                .expect("TASKCAGE_REAL_FFMPEG_BIN이 필요합니다"),
        );
        let source = PathBuf::from(
            std::env::var_os("TASKCAGE_REAL_FFMPEG_INPUT")
                .expect("TASKCAGE_REAL_FFMPEG_INPUT이 필요합니다"),
        );
        let source_bytes = fs::read(source).expect("실제 FFmpeg 입력 읽기");
        let root = std::env::temp_dir().join(format!(
            "taskcage-real-ffmpeg-profile-{}",
            std::process::id()
        ));
        let _ = make_test_tree_writable(&root);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let artifacts = root.join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
        let (cache, package_digest) = import_ffmpeg_package(&root, &ffmpeg);
        let (source_digest, source_size) =
            write_profile_input(&artifacts, "jobs/real/source.wav", &source_bytes);

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let profile_runtime = LocalProfileRuntime::open(
            &artifacts,
            32 * 1024 * 1024,
            profile_budget(),
            Some((&cache, package_digest)),
            None,
        )
        .expect("service UID import와 daemon resolve가 같은 ownership 계약을 사용해야 합니다");
        let handlers = ProtocolHandlers::initialize(
            Ok(environment),
            TaskCapacitySettings::new(1).unwrap(),
            TaskRegistrySettings::new(8).unwrap(),
            DeploymentResourcePolicy::for_test(),
            test_fail_stop(),
            Some(profile_runtime),
        )
        .unwrap();
        let submitted = handlers
            .handle_daemon_request(
                ffmpeg_profile_request(
                    REQUEST_ID,
                    "62626262-6262-4262-8262-626262626262",
                    "jobs/real/source.wav",
                    source_digest,
                    source_size,
                    None,
                ),
                || context_for(REAL_TASK),
            )
            .await;
        assert!(matches!(
            submitted,
            Response::ProfileAccepted { .. } | Response::ProfileResult { .. }
        ));
        let result = wait_for_profile_result(&handlers, REAL_TASK).await;
        let crate::protocol::ProfileTaskPayload::Finished {
            profile_outcome: crate::protocol::ProfileOutcome::Succeeded,
            termination_reason: crate::protocol::TerminationReason::Exited,
            artifacts: published,
            failure: None,
            ..
        } = result
        else {
            panic!("실제 FFmpeg Profile 성공 결과가 필요합니다: {result:#?}")
        };
        let audio = published.get("audio").expect("audio Artifact");
        assert_eq!(audio.media_type, "audio/wav");
        assert_eq!(audio.path, format!("tasks/{REAL_TASK}/result.wav"));
        let wav = fs::read(artifacts.join(&audio.path)).unwrap();
        assert!(wav.len() >= 12);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(
            fs::read_dir(&jobs_path)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
                .count(),
            0,
            "실제 FFmpeg Profile 뒤 task cgroup이 남아 있습니다"
        );

        make_test_tree_writable(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
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
            DeploymentResourcePolicy::for_test(),
            Arc::clone(&fail_stop),
            move |environment, settings| {
                SubmitCoordinator::initialize_with_cgroup_create_faults(
                    environment,
                    settings,
                    TaskRegistrySettings::new(16).unwrap(),
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
            TaskRegistrySettings::new(16).unwrap(),
            DeploymentResourcePolicy::for_test(),
            test_fail_stop(),
            None,
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
                        termination_reason: crate::protocol::TerminationReason::Cancelled,
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
                payload: crate::protocol::TaskPayload::Finished {
                    termination_reason: crate::protocol::TerminationReason::Cancelled,
                    process: crate::protocol::ProcessResult {
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
                payload: crate::protocol::TaskPayload::Finished {
                    termination_reason: crate::protocol::TerminationReason::TimedOut,
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
