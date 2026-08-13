//! v0.2 Local Profile Core의 daemon-owned `file-copy@1.0.0` 실행 계약이다.
//!
//! 이 모듈은 Runtime Package나 Bundle을 만들지 않는다. caller는 Profile identity와 typed input만
//! 보낼 수 있고, 실제 executable/argv/working directory/output path는 여기서 고정한다.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::Notify;

use crate::artifact::{
    ArtifactPath, ArtifactStoreError, ArtifactVerificationError, DeclaredOutputArtifact,
    LocalArtifactStore, LocalInputArtifact, PublishedArtifact, StagedArtifactTask,
};
use crate::digest::Sha256Digest;
use crate::execution_plan::ResolvedExecutionPlan;
use crate::protocol::{
    CommandSpec, ErrorCode, OutputLimits, ProfileFailurePayload, ProfileIdentity,
    ProfileInputValue, ProfileOutcome, ProfileRequestPayload, ProfileResourceOverrides,
    ProfileTaskPayload, PublishedArtifactKind, PublishedArtifactPayload, ResourceLimits,
    TaskPayload, TerminationReason,
};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};
use crate::runtime_package::{ResolvedRuntimePackage, RuntimePackageCache, RuntimePackageError};

pub(crate) const FILE_COPY_PROFILE_NAME: &str = "file-copy";
pub(crate) const FILE_COPY_PROFILE_VERSION: &str = "1.0.0";
pub(crate) const FFMPEG_PROFILE_NAME: &str = "ffmpeg-audio-to-wav";
pub(crate) const FFMPEG_PROFILE_VERSION: &str = "1.0.0";
const FILE_COPY_PROGRAM: &str = "/usr/bin/cp";
const FILE_COPY_OUTPUT_SLOT: &str = "result";
const FILE_COPY_OUTPUT_FILE: &str = "result.txt";
const FILE_COPY_OUTPUT_MEDIA_TYPE: &str = "text/plain";
pub(crate) const FFMPEG_PACKAGE_ID: &str = "org.taskcage.ffmpeg";
pub(crate) const FFMPEG_PACKAGE_ENTRYPOINT: &str = "bin/ffmpeg";
const FFMPEG_OUTPUT_SLOT: &str = "audio";
const FFMPEG_OUTPUT_FILE: &str = "result.wav";
const FFMPEG_OUTPUT_MEDIA_TYPE: &str = "audio/wav";
const FFMPEG_SAMPLE_RATES: &[i64] = &[8_000, 16_000, 22_050, 44_100, 48_000];
const FFMPEG_CHANNELS: &[i64] = &[1, 2];

#[derive(Debug)]
pub(crate) struct LocalProfileRuntime {
    artifacts: Arc<LocalArtifactStore>,
    maximum_artifact_bytes: u64,
    default_budget: ResourceBudget,
    ffmpeg: Option<FfmpegProfileRegistration>,
    requests: Mutex<HashMap<String, ProfileRequestEntry>>,
    tasks: Mutex<HashMap<String, Arc<ProfileTaskRecord>>>,
}

#[derive(Debug)]
struct FfmpegProfileRegistration {
    cache: RuntimePackageCache,
    digest: Sha256Digest,
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
    execution: PreparedProfileExecution,
    output_slot: &'static str,
}

#[derive(Debug)]
enum PreparedProfileExecution {
    FileCopy,
    Ffmpeg {
        entrypoint: File,
        sample_rate_hz: i64,
        channels: i64,
    },
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
    execution: PreparedProfileExecution,
    output_slot: &'static str,
}

#[derive(Debug)]
pub(crate) struct ProfileTaskRecord {
    task_id: String,
    profile: ProfileIdentity,
    budget: ResourceBudget,
    output_slot: &'static str,
    terminal: Mutex<Option<ProfileTerminal>>,
    terminal_ready: Notify,
}

#[derive(Debug, Error)]
pub(crate) enum ProfileStartupError {
    #[error(transparent)]
    Artifact(#[from] ArtifactStoreError),
    #[error(transparent)]
    RuntimePackage(#[from] RuntimePackageError),
    #[error("FFmpeg Runtime Package 계약이 잘못되었습니다: {0}")]
    FfmpegPackageContract(String),
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
        ffmpeg_registration: Option<(&Path, Sha256Digest)>,
    ) -> Result<Self, ProfileStartupError> {
        let program = Path::new(FILE_COPY_PROGRAM);
        let metadata = fs::metadata(program).map_err(|source| ArtifactStoreError::Io {
            operation: "file-copy profile program 확인",
            path: program.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ArtifactStoreError::NotRegularFile(program.to_path_buf()).into());
        }
        let ffmpeg = ffmpeg_registration
            .map(|(cache_root, digest)| {
                let cache = RuntimePackageCache::open(cache_root)?;
                let package = cache.resolve(digest)?;
                validate_ffmpeg_package_contract(&package)?;
                Ok::<_, ProfileStartupError>(FfmpegProfileRegistration { cache, digest })
            })
            .transpose()?;
        Ok(Self {
            artifacts: Arc::new(LocalArtifactStore::open(root, maximum_artifact_bytes)?),
            maximum_artifact_bytes,
            default_budget,
            ffmpeg,
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
        for slot in request.inputs.keys() {
            validate_slot_name(slot)?;
        }
        match (
            request.profile.name.as_str(),
            request.profile.version.as_str(),
        ) {
            (FILE_COPY_PROFILE_NAME, FILE_COPY_PROFILE_VERSION) => self.validate_file_copy(request),
            (FFMPEG_PROFILE_NAME, FFMPEG_PROFILE_VERSION) if self.ffmpeg.is_some() => {
                self.validate_ffmpeg(request)
            }
            _ => Err(ProfileError::new(
                ErrorCode::ProfileNotFound,
                format!(
                    "profile {}@{} is not installed",
                    request.profile.name, request.profile.version
                ),
            )),
        }
    }

    fn validate_file_copy(
        &self,
        request: &ProfileRequestPayload,
    ) -> Result<PreparedProfile, ProfileError> {
        if request.inputs.len() != 4 {
            return Err(ProfileError::new(
                ErrorCode::InvalidProfileInput,
                "file-copy requires exactly source, label, retain_metadata, and priority inputs",
            ));
        }
        let source = parse_source(request)?;
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
            request.inputs.get("retain_metadata"),
            Some(ProfileInputValue::Boolean { .. })
        ) {
            return Err(ProfileError::new(
                ErrorCode::InvalidProfileInput,
                "inputs.retain_metadata must be BOOLEAN",
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
            execution: PreparedProfileExecution::FileCopy,
            output_slot: FILE_COPY_OUTPUT_SLOT,
        })
    }

    fn validate_ffmpeg(
        &self,
        request: &ProfileRequestPayload,
    ) -> Result<PreparedProfile, ProfileError> {
        let (source, sample_rate_hz, channels) = validate_ffmpeg_inputs(request)?;
        let budget = resolve_budget(&self.default_budget, request.resource_overrides.as_ref())?;
        let registration = self
            .ffmpeg
            .as_ref()
            .expect("FFmpeg dispatch requires an installed registration");
        let package = registration
            .cache
            .resolve(registration.digest)
            .map_err(|error| {
                ProfileError::new(
                    ErrorCode::EnvironmentUnavailable,
                    format!("registered FFmpeg Runtime Package is unavailable: {error}"),
                )
            })?;
        validate_ffmpeg_package_contract(&package).map_err(|error| {
            ProfileError::new(ErrorCode::EnvironmentUnavailable, error.to_string())
        })?;
        let entrypoint = package.entrypoint().try_clone().map_err(|error| {
            ProfileError::new(
                ErrorCode::EnvironmentUnavailable,
                format!("verified FFmpeg entrypoint descriptor could not be pinned: {error}"),
            )
        })?;
        let output = DeclaredOutputArtifact::new(
            FFMPEG_OUTPUT_FILE,
            FFMPEG_OUTPUT_MEDIA_TYPE,
            self.maximum_artifact_bytes,
        )
        .expect("static FFmpeg output contract must be valid");
        Ok(PreparedProfile {
            request: request.clone(),
            source,
            output,
            budget,
            execution: PreparedProfileExecution::Ffmpeg {
                entrypoint,
                sample_rate_hz,
                channels,
            },
            output_slot: FFMPEG_OUTPUT_SLOT,
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
            execution: prepared.execution,
            output_slot: prepared.output_slot,
        })
    }

    pub(crate) fn new_task(
        &self,
        task_id: &str,
        request: &ProfileRequestPayload,
        budget: ResourceBudget,
        output_slot: &'static str,
    ) -> Arc<ProfileTaskRecord> {
        Arc::new(ProfileTaskRecord {
            task_id: task_id.to_owned(),
            profile: request.profile.clone(),
            budget,
            output_slot,
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
        &'static str,
    ) {
        let input = self.staged.input_path();
        let output = self.staged.output_path();
        let working_directory = self.staged.working_directory();
        let plan = match self.execution {
            PreparedProfileExecution::FileCopy => {
                let command = CommandSpec {
                    program: FILE_COPY_PROGRAM.to_owned(),
                    args: vec![
                        input.to_string_lossy().into_owned(),
                        output.to_string_lossy().into_owned(),
                    ],
                    working_directory: working_directory.to_string_lossy().into_owned(),
                    environment: BTreeMap::new(),
                };
                ResolvedExecutionPlan::from_validated_raw(&command, self.budget.clone())
            }
            PreparedProfileExecution::Ffmpeg {
                entrypoint,
                sample_rate_hz,
                channels,
            } => ResolvedExecutionPlan::from_pinned_entrypoint(
                entrypoint,
                OsString::from("ffmpeg"),
                ffmpeg_arguments(&input, sample_rate_hz, channels, &output),
                working_directory,
                BTreeMap::new(),
                self.budget.clone(),
            ),
        };
        (
            self.request,
            self.budget,
            self.staged,
            plan,
            self.output_slot,
        )
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
                finished_profile_payload(raw, self.profile.clone(), self.output_slot, terminal)
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
    output_slot: &'static str,
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
            artifacts: BTreeMap::from([(output_slot.to_owned(), published_wire(artifact))]),
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

fn parse_source(request: &ProfileRequestPayload) -> Result<LocalInputArtifact, ProfileError> {
    match request.inputs.get("source") {
        Some(ProfileInputValue::LocalInput {
            path,
            digest,
            size_bytes,
        }) => {
            let path = ArtifactPath::parse(path.clone()).map_err(|error| {
                ProfileError::new(ErrorCode::InvalidArtifactPath, error.to_string())
            })?;
            let digest = Sha256Digest::from_str(digest).map_err(|error| {
                ProfileError::new(ErrorCode::InvalidProfileInput, error.to_string())
            })?;
            Ok(LocalInputArtifact::new(path, digest, *size_bytes))
        }
        Some(_) => Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "inputs.source must be LOCAL_INPUT",
        )),
        None => Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "inputs.source is required",
        )),
    }
}

fn validate_ffmpeg_inputs(
    request: &ProfileRequestPayload,
) -> Result<(LocalInputArtifact, i64, i64), ProfileError> {
    if request.inputs.len() != 3 {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "ffmpeg-audio-to-wav requires exactly source, sample_rate_hz, and channels inputs",
        ));
    }
    let source = parse_source(request)?;
    let sample_rate_hz = allowed_int64(
        request,
        "sample_rate_hz",
        FFMPEG_SAMPLE_RATES,
        "8000, 16000, 22050, 44100, or 48000",
    )?;
    let channels = allowed_int64(request, "channels", FFMPEG_CHANNELS, "1 or 2")?;
    Ok((source, sample_rate_hz, channels))
}

fn allowed_int64(
    request: &ProfileRequestPayload,
    slot: &'static str,
    allowed: &[i64],
    allowed_description: &'static str,
) -> Result<i64, ProfileError> {
    match request.inputs.get(slot) {
        Some(ProfileInputValue::Int64 { value }) if allowed.contains(value) => Ok(*value),
        Some(ProfileInputValue::Int64 { .. }) => Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            format!("inputs.{slot} must be {allowed_description}"),
        )),
        _ => Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            format!("inputs.{slot} must be INT64"),
        )),
    }
}

fn validate_ffmpeg_package_contract(
    package: &ResolvedRuntimePackage,
) -> Result<(), ProfileStartupError> {
    let manifest = package.manifest();
    if manifest.id != FFMPEG_PACKAGE_ID {
        return Err(ProfileStartupError::FfmpegPackageContract(format!(
            "id must be {FFMPEG_PACKAGE_ID}, actual={}",
            manifest.id
        )));
    }
    if manifest.entrypoint != FFMPEG_PACKAGE_ENTRYPOINT {
        return Err(ProfileStartupError::FfmpegPackageContract(format!(
            "entrypoint must be {FFMPEG_PACKAGE_ENTRYPOINT}, actual={}",
            manifest.entrypoint
        )));
    }
    Ok(())
}

fn ffmpeg_arguments(
    input: &Path,
    sample_rate_hz: i64,
    channels: i64,
    output: &Path,
) -> Vec<OsString> {
    [
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-nostdin"),
        OsString::from("-i"),
        input.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:a:0"),
        OsString::from("-vn"),
        OsString::from("-c:a"),
        OsString::from("pcm_s16le"),
        OsString::from("-ar"),
        OsString::from(sample_rate_hz.to_string()),
        OsString::from("-ac"),
        OsString::from(channels.to_string()),
        output.as_os_str().to_owned(),
    ]
    .into_iter()
    .collect()
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
        || !bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'_' | b'-')
        })
    {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile input slot names must match [a-z][a-z0-9_-]{0,63}",
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use sha2::{Digest, Sha256};

    use crate::protocol::{CpuMax, OutputLimits, ResourceLimits};
    use crate::runtime_package::import_for_service_uid;

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taskcage-ffmpeg-profile-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = make_tree_writable(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn make_tree_writable(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(path)? {
                make_tree_writable(&entry?.path())?;
            }
        } else {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn budget() -> ResourceBudget {
        ResourceBudget::try_from_protocol(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 100_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 512 * 1024 * 1024,
                pids_max: 32,
                wall_time_limit_ms: 120_000,
            },
            OutputLimits {
                stdout_tail_max_bytes: 65_536,
                stderr_tail_max_bytes: 65_536,
            },
        )
        .unwrap()
    }

    fn artifact_root(root: &Path, label: &str) -> PathBuf {
        let path = root.join(format!("artifacts-{label}"));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn cache_root(root: &Path, label: &str) -> PathBuf {
        let path = root.join(format!("cache-{label}"));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn package_source(
        root: &Path,
        label: &str,
        id: &str,
        entrypoint: &str,
        executable: &[u8],
    ) -> PathBuf {
        let source = root.join(format!("source-{label}"));
        let entrypoint_path = source.join("rootfs").join(entrypoint);
        let sbom_path = source.join("rootfs/share/sbom.spdx.json");
        fs::create_dir_all(entrypoint_path.parent().unwrap()).unwrap();
        fs::create_dir_all(sbom_path.parent().unwrap()).unwrap();
        fs::write(&entrypoint_path, executable).unwrap();
        fs::set_permissions(&entrypoint_path, fs::Permissions::from_mode(0o555)).unwrap();
        let sbom = br#"{"spdxVersion":"SPDX-2.3"}"#;
        fs::write(&sbom_path, sbom).unwrap();
        fs::set_permissions(&sbom_path, fs::Permissions::from_mode(0o444)).unwrap();
        let manifest = json!({
            "schemaVersion": "taskcage.runtime-package/v0alpha1",
            "id": id,
            "version": "0.0.0-test.1",
            "platform": {
                "os": "linux",
                "architecture": std::env::consts::ARCH,
                "abi": "gnu",
                "libc": { "family": "glibc", "minimumVersion": "2.17" }
            },
            "entrypoint": entrypoint,
            "libraryPaths": [],
            "files": [
                {
                    "path": entrypoint,
                    "digest": sha256(executable),
                    "sizeBytes": executable.len(),
                    "mode": "0555"
                },
                {
                    "path": "share/sbom.spdx.json",
                    "digest": sha256(sbom),
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
        source
    }

    fn import_package(
        root: &Path,
        label: &str,
        id: &str,
        entrypoint: &str,
        executable: &[u8],
    ) -> (PathBuf, Sha256Digest) {
        let cache = cache_root(root, label);
        let source = package_source(root, label, id, entrypoint, executable);
        let report = import_for_service_uid(&cache, &source).unwrap();
        (cache, report.digest)
    }

    fn request() -> ProfileRequestPayload {
        ProfileRequestPayload {
            client_request_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            profile: ProfileIdentity {
                name: FFMPEG_PROFILE_NAME.to_owned(),
                version: FFMPEG_PROFILE_VERSION.to_owned(),
            },
            inputs: BTreeMap::from([
                (
                    "source".to_owned(),
                    ProfileInputValue::LocalInput {
                        path: "jobs/42/source.mp3".to_owned(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        size_bytes: 128,
                    },
                ),
                (
                    "sample_rate_hz".to_owned(),
                    ProfileInputValue::Int64 { value: 16_000 },
                ),
                ("channels".to_owned(), ProfileInputValue::Int64 { value: 1 }),
            ]),
            resource_overrides: None,
        }
    }

    #[test]
    fn profile_slot_names_follow_the_lowercase_wire_contract() {
        assert!(validate_slot_name("retain_metadata").is_ok());
        assert!(validate_slot_name("output-2").is_ok());
        assert!(validate_slot_name("retainMetadata").is_err());
        assert!(validate_slot_name("Output").is_err());
    }

    #[test]
    fn ffmpeg_inputs_reject_unsupported_values_and_wrong_slot_sets() {
        let mut unsupported_rate = request();
        unsupported_rate.inputs.insert(
            "sample_rate_hz".to_owned(),
            ProfileInputValue::Int64 { value: 96_000 },
        );
        assert_eq!(
            validate_ffmpeg_inputs(&unsupported_rate)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );

        let mut unsupported_channels = request();
        unsupported_channels
            .inputs
            .insert("channels".to_owned(), ProfileInputValue::Int64 { value: 6 });
        assert_eq!(
            validate_ffmpeg_inputs(&unsupported_channels)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );

        let mut missing = request();
        missing.inputs.remove("source");
        assert_eq!(
            validate_ffmpeg_inputs(&missing).unwrap_err().code(),
            ErrorCode::InvalidProfileInput
        );

        let mut unexpected = request();
        unexpected.inputs.insert(
            "output".to_owned(),
            ProfileInputValue::String {
                value: "caller.wav".to_owned(),
            },
        );
        assert_eq!(
            validate_ffmpeg_inputs(&unexpected).unwrap_err().code(),
            ErrorCode::InvalidProfileInput
        );
    }

    #[test]
    fn ffmpeg_argv_and_output_contract_are_daemon_owned_and_deterministic() {
        let input =
            Path::new("/var/lib/taskcage/artifacts/.taskcage/staging/task/artifacts/in/source");
        let output = Path::new(
            "/var/lib/taskcage/artifacts/.taskcage/staging/task/artifacts/out/result.wav",
        );
        assert_eq!(
            ffmpeg_arguments(input, 16_000, 1, output),
            vec![
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                input.to_str().unwrap(),
                "-map",
                "0:a:0",
                "-vn",
                "-c:a",
                "pcm_s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
                output.to_str().unwrap(),
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        let declared =
            DeclaredOutputArtifact::new(FFMPEG_OUTPUT_FILE, FFMPEG_OUTPUT_MEDIA_TYPE, 1024)
                .unwrap();
        assert_eq!(FFMPEG_OUTPUT_SLOT, "audio");
        assert_eq!(declared.file_name(), "result.wav");
        assert_eq!(declared.media_type(), "audio/wav");
    }

    #[test]
    fn ffmpeg_registration_rejects_missing_corrupt_and_wrong_contract_packages() {
        let fixture = TestDirectory::new("registration");
        let artifacts = artifact_root(fixture.path(), "registration");

        let missing_cache = cache_root(fixture.path(), "missing");
        let missing_digest = Sha256Digest::from_bytes([7; 32]);
        assert!(matches!(
            LocalProfileRuntime::open(
                &artifacts,
                1024,
                budget(),
                Some((&missing_cache, missing_digest))
            ),
            Err(ProfileStartupError::RuntimePackage(_))
        ));

        let (wrong_id_cache, wrong_id_digest) = import_package(
            fixture.path(),
            "wrong-id",
            "org.taskcage.not-ffmpeg",
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"wrong-id-package",
        );
        assert!(matches!(
            LocalProfileRuntime::open(
                &artifacts,
                1024,
                budget(),
                Some((&wrong_id_cache, wrong_id_digest))
            ),
            Err(ProfileStartupError::FfmpegPackageContract(_))
        ));

        let (wrong_entry_cache, wrong_entry_digest) = import_package(
            fixture.path(),
            "wrong-entry",
            FFMPEG_PACKAGE_ID,
            "bin/not-ffmpeg",
            b"wrong-entry-package",
        );
        assert!(matches!(
            LocalProfileRuntime::open(
                &artifacts,
                1024,
                budget(),
                Some((&wrong_entry_cache, wrong_entry_digest))
            ),
            Err(ProfileStartupError::FfmpegPackageContract(_))
        ));

        let (corrupt_cache, corrupt_digest) = import_package(
            fixture.path(),
            "corrupt",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"verified-package",
        );
        let cached_entrypoint = corrupt_cache
            .join("packages/sha256")
            .join(corrupt_digest.hex())
            .join("rootfs")
            .join(FFMPEG_PACKAGE_ENTRYPOINT);
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&cached_entrypoint, b"corrupted-package").unwrap();
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(matches!(
            LocalProfileRuntime::open(
                &artifacts,
                1024,
                budget(),
                Some((&corrupt_cache, corrupt_digest))
            ),
            Err(ProfileStartupError::RuntimePackage(_))
        ));
    }

    #[test]
    fn ffmpeg_package_is_reverified_for_each_new_task() {
        let fixture = TestDirectory::new("reresolve");
        let artifacts = artifact_root(fixture.path(), "reresolve");
        let (cache, digest) = import_package(
            fixture.path(),
            "reresolve",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"verified-package",
        );
        let runtime =
            LocalProfileRuntime::open(&artifacts, 1024, budget(), Some((&cache, digest))).unwrap();
        assert!(runtime.validate(&request()).is_ok());

        let cached_entrypoint = cache
            .join("packages/sha256")
            .join(digest.hex())
            .join("rootfs")
            .join(FFMPEG_PACKAGE_ENTRYPOINT);
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&cached_entrypoint, b"corrupted-package").unwrap();
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o555)).unwrap();
        let error = runtime.validate(&request()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::EnvironmentUnavailable);
    }
}
