//! v0.2 Local Profile Core의 daemon-owned `file-copy@1.0.0` 실행 계약이다.
//!
//! 이 모듈은 Runtime Package나 Bundle을 만들지 않는다. caller는 Profile identity와 typed input만
//! 보낼 수 있고, 실제 executable/argv/working directory/output path는 여기서 고정한다.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::Notify;

use crate::artifact::{
    ArtifactPath, ArtifactStoreError, ArtifactVerificationError, DeclaredOutputArtifact,
    LocalArtifactStore, LocalInputArtifact, PublishedArtifact, StagedArtifactTask,
};
use crate::execution_plan::ResolvedExecutionPlan;
use crate::protocol::{
    CommandSpec, ErrorCode, OutputLimits, ProfileFailurePayload, ProfileIdentity,
    ProfileInputValue, ProfileOutcome, ProfileRequestPayload, ProfileResourceOverrides,
    ProfileTaskPayload, PublishedArtifactKind, PublishedArtifactPayload, ResourceLimits,
    TaskPayload, TerminationReason,
};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};

pub(crate) const FILE_COPY_PROFILE_NAME: &str = "file-copy";
pub(crate) const FILE_COPY_PROFILE_VERSION: &str = "1.0.0";
const FILE_COPY_PROGRAM: &str = "/usr/bin/cp";
const FILE_COPY_OUTPUT_SLOT: &str = "result";
const FILE_COPY_OUTPUT_FILE: &str = "result.txt";
const FILE_COPY_OUTPUT_MEDIA_TYPE: &str = "text/plain";

#[derive(Debug)]
pub(crate) struct LocalProfileRuntime {
    artifacts: Arc<LocalArtifactStore>,
    maximum_artifact_bytes: u64,
    default_budget: ResourceBudget,
    requests: Mutex<HashMap<String, ProfileRequestEntry>>,
    tasks: Mutex<HashMap<String, Arc<ProfileTaskRecord>>>,
}

#[derive(Debug)]
enum ProfileRequestEntry {
    Pending {
        request: ProfileRequestPayload,
        ready: Arc<Notify>,
    },
    Accepted {
        request: ProfileRequestPayload,
        task: Arc<ProfileTaskRecord>,
    },
}

#[derive(Debug)]
pub(crate) enum ProfileReservation {
    Owner {
        client_request_id: String,
        request: Box<ProfileRequestPayload>,
        ready: Arc<Notify>,
    },
    Existing(Arc<ProfileTaskRecord>),
}

#[derive(Debug)]
pub(crate) struct PreparedProfile {
    request: ProfileRequestPayload,
    source: LocalInputArtifact,
    output: DeclaredOutputArtifact,
    budget: ResourceBudget,
}

impl PreparedProfile {
    pub(crate) fn budget(&self) -> &ResourceBudget {
        &self.budget
    }
}

#[derive(Debug)]
pub(crate) struct StagedProfile {
    request: ProfileRequestPayload,
    budget: ResourceBudget,
    staged: StagedArtifactTask,
}

#[derive(Debug)]
pub(crate) struct ProfileTaskRecord {
    task_id: String,
    profile: ProfileIdentity,
    budget: ResourceBudget,
    terminal: Mutex<Option<ProfileTerminal>>,
    terminal_ready: Notify,
}

#[derive(Debug, Clone)]
enum ProfileTerminal {
    Succeeded(PublishedArtifact),
    Failed(ProfileFailurePayload),
}

#[derive(Debug, Error)]
pub(crate) enum ProfileFinalizationError {
    #[error(transparent)]
    Artifact(#[from] ArtifactStoreError),
    #[error("profile terminal state is unavailable")]
    StateUnavailable,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct ProfileError {
    code: ErrorCode,
    message: String,
}

impl ProfileError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn code(&self) -> ErrorCode {
        self.code
    }
}

impl LocalProfileRuntime {
    pub(crate) fn open(
        root: &Path,
        maximum_artifact_bytes: u64,
        default_budget: ResourceBudget,
    ) -> Result<Self, ArtifactStoreError> {
        let program = Path::new(FILE_COPY_PROGRAM);
        let metadata = fs::metadata(program).map_err(|source| ArtifactStoreError::Io {
            operation: "file-copy profile program 확인",
            path: program.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ArtifactStoreError::NotRegularFile(program.to_path_buf()));
        }
        Ok(Self {
            artifacts: Arc::new(LocalArtifactStore::open(root, maximum_artifact_bytes)?),
            maximum_artifact_bytes,
            default_budget,
            requests: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn reserve(
        &self,
        request: ProfileRequestPayload,
    ) -> Result<ProfileReservation, ProfileError> {
        let client_request_id = request.client_request_id.clone();
        loop {
            let ready = {
                let mut requests = self.requests.lock().map_err(|_| {
                    ProfileError::new(
                        ErrorCode::InternalError,
                        "profile request state is unavailable",
                    )
                })?;
                match requests.get(&client_request_id) {
                    Some(ProfileRequestEntry::Accepted {
                        request: existing,
                        task,
                    }) => {
                        if *existing != request {
                            return Err(ProfileError::new(
                                ErrorCode::IdempotencyConflict,
                                "clientRequestId was already used for a different request",
                            ));
                        }
                        return Ok(ProfileReservation::Existing(Arc::clone(task)));
                    }
                    Some(ProfileRequestEntry::Pending {
                        request: existing,
                        ready,
                    }) => {
                        if *existing != request {
                            return Err(ProfileError::new(
                                ErrorCode::IdempotencyConflict,
                                "clientRequestId was already used for a different request",
                            ));
                        }
                        Arc::clone(ready)
                    }
                    None => {
                        let ready = Arc::new(Notify::new());
                        requests.insert(
                            client_request_id.clone(),
                            ProfileRequestEntry::Pending {
                                request: request.clone(),
                                ready: Arc::clone(&ready),
                            },
                        );
                        return Ok(ProfileReservation::Owner {
                            client_request_id,
                            request: Box::new(request),
                            ready,
                        });
                    }
                }
            };
            ready.notified().await;
        }
    }

    pub(crate) fn release(&self, reservation: ProfileReservation) {
        let ProfileReservation::Owner {
            client_request_id,
            ready,
            ..
        } = reservation
        else {
            return;
        };
        if let Ok(mut requests) = self.requests.lock()
            && matches!(
                requests.get(&client_request_id),
                Some(ProfileRequestEntry::Pending { .. })
            )
        {
            requests.remove(&client_request_id);
        }
        ready.notify_waiters();
    }

    pub(crate) fn validate(
        &self,
        request: &ProfileRequestPayload,
    ) -> Result<PreparedProfile, ProfileError> {
        validate_uuid("clientRequestId", &request.client_request_id)?;
        validate_profile_identity(&request.profile)?;
        if request.profile.name != FILE_COPY_PROFILE_NAME
            || request.profile.version != FILE_COPY_PROFILE_VERSION
        {
            return Err(ProfileError::new(
                ErrorCode::ProfileNotFound,
                format!(
                    "profile {}@{} is not installed",
                    request.profile.name, request.profile.version
                ),
            ));
        }

        for slot in request.inputs.keys() {
            validate_slot_name(slot)?;
        }
        if request.inputs.len() != 4 {
            return Err(ProfileError::new(
                ErrorCode::InvalidProfileInput,
                "file-copy requires exactly source, label, retainMetadata, and priority inputs",
            ));
        }

        let source = match request.inputs.get("source") {
            Some(ProfileInputValue::LocalInput {
                path,
                digest,
                size_bytes,
            }) => {
                let path = ArtifactPath::parse(path.clone()).map_err(|error| {
                    ProfileError::new(ErrorCode::InvalidArtifactPath, error.to_string())
                })?;
                let digest = crate::digest::Sha256Digest::from_str(digest).map_err(|error| {
                    ProfileError::new(ErrorCode::InvalidProfileInput, error.to_string())
                })?;
                LocalInputArtifact::new(path, digest, *size_bytes)
            }
            Some(_) => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.source must be LOCAL_INPUT",
                ));
            }
            None => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.source is required",
                ));
            }
        };
        match request.inputs.get("label") {
            Some(ProfileInputValue::String { value })
                if !value.is_empty() && value.len() <= 128 => {}
            Some(ProfileInputValue::String { .. }) => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.label must contain 1 to 128 bytes",
                ));
            }
            _ => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.label must be STRING",
                ));
            }
        }
        if !matches!(
            request.inputs.get("retainMetadata"),
            Some(ProfileInputValue::Boolean { .. })
        ) {
            return Err(ProfileError::new(
                ErrorCode::InvalidProfileInput,
                "inputs.retainMetadata must be BOOLEAN",
            ));
        }
        match request.inputs.get("priority") {
            Some(ProfileInputValue::Int64 { value }) if (0..=100).contains(value) => {}
            Some(ProfileInputValue::Int64 { .. }) => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.priority must be INT64 between 0 and 100",
                ));
            }
            _ => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.priority must be INT64",
                ));
            }
        }

        let budget = resolve_budget(&self.default_budget, request.resource_overrides.as_ref())?;
        let output = DeclaredOutputArtifact::new(
            FILE_COPY_OUTPUT_FILE,
            FILE_COPY_OUTPUT_MEDIA_TYPE,
            self.maximum_artifact_bytes,
        )
        .expect("static file-copy output contract must be valid");
        Ok(PreparedProfile {
            request: request.clone(),
            source,
            output,
            budget,
        })
    }

    pub(crate) fn stage(
        &self,
        task_id: &str,
        prepared: PreparedProfile,
    ) -> Result<StagedProfile, ProfileError> {
        let staged = self
            .artifacts
            .stage_input(task_id, &prepared.source, prepared.output)
            .map_err(profile_stage_error)?;
        Ok(StagedProfile {
            request: prepared.request,
            budget: prepared.budget,
            staged,
        })
    }

    pub(crate) fn new_task(
        &self,
        task_id: &str,
        request: &ProfileRequestPayload,
        budget: ResourceBudget,
    ) -> Arc<ProfileTaskRecord> {
        Arc::new(ProfileTaskRecord {
            task_id: task_id.to_owned(),
            profile: request.profile.clone(),
            budget,
            terminal: Mutex::new(None),
            terminal_ready: Notify::new(),
        })
    }

    pub(crate) fn accept(
        &self,
        reservation: ProfileReservation,
        task: Arc<ProfileTaskRecord>,
    ) -> Result<Arc<ProfileTaskRecord>, ProfileError> {
        let ProfileReservation::Owner {
            client_request_id,
            request,
            ready,
        } = reservation
        else {
            return Err(ProfileError::new(
                ErrorCode::InternalError,
                "only a profile request owner may accept a task",
            ));
        };
        let mut requests = self.requests.lock().map_err(|_| {
            ProfileError::new(
                ErrorCode::InternalError,
                "profile request state is unavailable",
            )
        })?;
        requests.insert(
            client_request_id,
            ProfileRequestEntry::Accepted {
                request: *request,
                task: Arc::clone(&task),
            },
        );
        drop(requests);
        self.tasks
            .lock()
            .map_err(|_| {
                ProfileError::new(
                    ErrorCode::InternalError,
                    "profile task state is unavailable",
                )
            })?
            .insert(task.task_id().to_owned(), Arc::clone(&task));
        ready.notify_waiters();
        Ok(task)
    }

    pub(crate) fn task(
        &self,
        task_id: &str,
    ) -> Result<Option<Arc<ProfileTaskRecord>>, ProfileError> {
        Ok(self
            .tasks
            .lock()
            .map_err(|_| {
                ProfileError::new(
                    ErrorCode::InternalError,
                    "profile task state is unavailable",
                )
            })?
            .get(task_id)
            .cloned())
    }

    /// Raw Task snapshot retention이 끝난 Profile mapping은 다음 submit 전에 함께 제거한다.
    /// Profile만 오래 남아 같은 clientRequestId를 영구적으로 막으면 v1 Registry retention 경계와 달라진다.
    pub(crate) fn prune_missing_tasks(
        &self,
        mut raw_task_exists: impl FnMut(&str) -> Result<bool, String>,
    ) -> Result<(), ProfileError> {
        let task_ids = self
            .tasks
            .lock()
            .map_err(|_| {
                ProfileError::new(
                    ErrorCode::InternalError,
                    "profile task state is unavailable",
                )
            })?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for task_id in task_ids {
            if !raw_task_exists(&task_id)
                .map_err(|message| ProfileError::new(ErrorCode::InternalError, message))?
            {
                self.discard_task(&task_id)?;
            }
        }
        Ok(())
    }

    /// Raw Task가 이미 retention에서 사라졌다면 Profile request/task mapping도 함께 버린다.
    pub(crate) fn discard_task(&self, task_id: &str) -> Result<(), ProfileError> {
        self.tasks
            .lock()
            .map_err(|_| {
                ProfileError::new(
                    ErrorCode::InternalError,
                    "profile task state is unavailable",
                )
            })?
            .remove(task_id);
        self.requests
            .lock()
            .map_err(|_| {
                ProfileError::new(
                    ErrorCode::InternalError,
                    "profile request state is unavailable",
                )
            })?
            .retain(|_, entry| match entry {
                ProfileRequestEntry::Pending { .. } => true,
                ProfileRequestEntry::Accepted { task, .. } => task.task_id() != task_id,
            });
        Ok(())
    }
}

impl StagedProfile {
    pub(crate) fn into_plan(
        self,
    ) -> (
        ProfileRequestPayload,
        ResourceBudget,
        StagedArtifactTask,
        ResolvedExecutionPlan,
    ) {
        let command = CommandSpec {
            program: FILE_COPY_PROGRAM.to_owned(),
            args: vec![
                self.staged.input_path().to_string_lossy().into_owned(),
                self.staged.output_path().to_string_lossy().into_owned(),
            ],
            working_directory: self
                .staged
                .working_directory()
                .to_string_lossy()
                .into_owned(),
            environment: BTreeMap::new(),
        };
        let plan = ResolvedExecutionPlan::from_validated_raw(&command, self.budget.clone());
        (self.request, self.budget, self.staged, plan)
    }
}

impl ProfileTaskRecord {
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) fn profile(&self) -> &ProfileIdentity {
        &self.profile
    }

    pub(crate) fn budget(&self) -> &ResourceBudget {
        &self.budget
    }

    pub(crate) fn finalize(
        &self,
        raw: &TaskPayload,
        staged: StagedArtifactTask,
    ) -> Result<(), ProfileFinalizationError> {
        let terminal = match raw {
            TaskPayload::Finished {
                termination_reason: TerminationReason::Exited,
                process,
                ..
            } if process.exit_code == Some(0) => match staged.publish_for_profile()? {
                Ok(artifact) => ProfileTerminal::Succeeded(artifact),
                Err(error) => ProfileTerminal::Failed(ProfileFailurePayload {
                    code: output_failure_code(&error).to_owned(),
                    message: error.to_string(),
                }),
            },
            TaskPayload::Finished {
                termination_reason, ..
            } => {
                let failure = ProfileFailurePayload {
                    code: process_failure_code(*termination_reason).to_owned(),
                    message: format!("profile process finished with {termination_reason:?}"),
                };
                staged.cleanup()?;
                ProfileTerminal::Failed(failure)
            }
            TaskPayload::Running { .. } => ProfileTerminal::Failed(ProfileFailurePayload {
                code: "INTERNAL_ERROR".to_owned(),
                message: "profile finalization requires a finished task".to_owned(),
            }),
        };
        let mut slot = self
            .terminal
            .lock()
            .map_err(|_| ProfileFinalizationError::StateUnavailable)?;
        *slot = Some(terminal);
        drop(slot);
        self.terminal_ready.notify_waiters();
        Ok(())
    }

    pub(crate) async fn snapshot(&self, raw: TaskPayload) -> ProfileTaskPayload {
        match raw {
            TaskPayload::Running {
                task_id,
                submitted_at,
                started_at,
            } => ProfileTaskPayload::Running {
                task_id,
                profile: self.profile.clone(),
                submitted_at,
                started_at,
            },
            raw @ TaskPayload::Finished { .. } => {
                let terminal = self.wait_for_terminal().await;
                finished_profile_payload(raw, self.profile.clone(), terminal)
            }
        }
    }

    async fn wait_for_terminal(&self) -> ProfileTerminal {
        loop {
            let notified = self.terminal_ready.notified();
            if let Ok(terminal) = self.terminal.lock()
                && let Some(terminal) = terminal.clone()
            {
                return terminal;
            }
            notified.await;
        }
    }
}

fn finished_profile_payload(
    raw: TaskPayload,
    profile: ProfileIdentity,
    terminal: ProfileTerminal,
) -> ProfileTaskPayload {
    let TaskPayload::Finished {
        task_id,
        termination_reason,
        process,
        timing,
        usage,
        output,
    } = raw
    else {
        unreachable!("terminal profile payload에는 finished raw task가 필요합니다")
    };
    match terminal {
        ProfileTerminal::Succeeded(artifact) => ProfileTaskPayload::Finished {
            task_id,
            profile,
            profile_outcome: ProfileOutcome::Succeeded,
            termination_reason,
            process,
            timing,
            usage,
            output,
            artifacts: BTreeMap::from([(
                FILE_COPY_OUTPUT_SLOT.to_owned(),
                published_wire(artifact),
            )]),
            failure: None,
        },
        ProfileTerminal::Failed(failure) => ProfileTaskPayload::Finished {
            task_id,
            profile,
            profile_outcome: ProfileOutcome::Failed,
            termination_reason,
            process,
            timing,
            usage,
            output,
            artifacts: BTreeMap::new(),
            failure: Some(failure),
        },
    }
}

fn published_wire(artifact: PublishedArtifact) -> PublishedArtifactPayload {
    PublishedArtifactPayload {
        kind: PublishedArtifactKind::LocalFile,
        path: artifact.path().as_str().to_owned(),
        digest: artifact.digest().to_string(),
        size_bytes: artifact.size_bytes(),
        media_type: artifact.media_type().to_owned(),
    }
}

fn resolve_budget(
    default: &ResourceBudget,
    overrides: Option<&ProfileResourceOverrides>,
) -> Result<ResourceBudget, ProfileError> {
    let Some(overrides) = overrides else {
        return Ok(default.clone());
    };
    let mut limits: ResourceLimits = default.protocol_limits();
    let mut output: OutputLimits = default.protocol_output();
    let mut has_value = false;
    if let Some(partial) = &overrides.limits {
        if let Some(cpu_max) = &partial.cpu_max {
            limits.cpu_max = cpu_max.clone();
            has_value = true;
        }
        if let Some(value) = partial.memory_max_bytes {
            limits.memory_max_bytes = value;
            has_value = true;
        }
        if let Some(value) = partial.pids_max {
            limits.pids_max = value;
            has_value = true;
        }
        if let Some(value) = partial.wall_time_limit_ms {
            limits.wall_time_limit_ms = value;
            has_value = true;
        }
    }
    if let Some(partial) = &overrides.output {
        if let Some(value) = partial.stdout_tail_max_bytes {
            output.stdout_tail_max_bytes = value;
            has_value = true;
        }
        if let Some(value) = partial.stderr_tail_max_bytes {
            output.stderr_tail_max_bytes = value;
            has_value = true;
        }
    }
    if !has_value {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "resourceOverrides must contain at least one nested field",
        ));
    }
    ResourceBudget::try_from_protocol(limits, output).map_err(profile_input_budget_error)
}

fn profile_input_budget_error(error: ResourceBudgetError) -> ProfileError {
    ProfileError::new(ErrorCode::InvalidProfileInput, error.to_string())
}

fn profile_stage_error(error: ArtifactStoreError) -> ProfileError {
    let code = match &error {
        ArtifactStoreError::Verification(
            ArtifactVerificationError::DigestMismatch
            | ArtifactVerificationError::SizeMismatch { .. },
        ) => ErrorCode::ArtifactDigestMismatch,
        ArtifactStoreError::Verification(_) | ArtifactStoreError::InvalidTaskId(_) => {
            ErrorCode::InvalidProfileInput
        }
        _ => ErrorCode::InvalidArtifactPath,
    };
    ProfileError::new(code, error.to_string())
}

fn output_failure_code(error: &ArtifactStoreError) -> &'static str {
    match error {
        ArtifactStoreError::UndeclaredOutput(_) | ArtifactStoreError::NotRegularFile(_) => {
            "OUTPUT_CONTRACT_VIOLATION"
        }
        _ => "OUTPUT_PUBLISH_FAILED",
    }
}

fn process_failure_code(reason: TerminationReason) -> &'static str {
    match reason {
        TerminationReason::Exited => "PROCESS_EXITED_NONZERO",
        TerminationReason::ExecutionFailed => "EXECUTION_FAILED",
        TerminationReason::Cancelled => "CANCELLED",
        TerminationReason::TimedOut => "TIMED_OUT",
        TerminationReason::MemoryLimitExceeded => "MEMORY_LIMIT_EXCEEDED",
        TerminationReason::ProcessLimitExceeded => "PROCESS_LIMIT_EXCEEDED",
        TerminationReason::DaemonError => "DAEMON_ERROR",
    }
}

fn validate_profile_identity(profile: &ProfileIdentity) -> Result<(), ProfileError> {
    let name = profile.name.as_bytes();
    if name.is_empty()
        || name.len() > 63
        || !name[0].is_ascii_lowercase()
        || !name[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile.name must match [a-z][a-z0-9-]{0,62}",
        ));
    }
    if !profile.version.split('.').all(valid_version_component)
        || profile.version.split('.').count() != 3
    {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile.version must be strict MAJOR.MINOR.PATCH",
        ));
    }
    Ok(())
}

fn valid_version_component(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn validate_slot_name(value: &str) -> Result<(), ProfileError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_lowercase()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
    {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile input slot names must match [a-z][A-Za-z0-9_-]{0,63}",
        ));
    }
    Ok(())
}

fn validate_uuid(field: &'static str, value: &str) -> Result<(), ProfileError> {
    let bytes = value.as_bytes();
    if bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        Ok(())
    } else {
        Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            format!("{field} must be a UUID"),
        ))
    }
}
