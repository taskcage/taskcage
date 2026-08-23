//! Local Profile artifact staging, idempotency, task record, finalization lifecycle을 소유한다.
//!
//! Profile identity와 실행 계약 해석은 `profile_registry`에 위임하고, staging 경로가 생긴 뒤
//! Registry가 반환한 검증된 계약으로 실행 plan을 만든다.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

use taskcage_core::task::{TaskSnapshot as TaskPayload, TerminationReason};
use thiserror::Error;
use tokio::sync::Notify;

use crate::application::capsule::{ProfileRegistry, ResolvedProfile, StagedProfile};
use crate::artifact::{
    ArtifactPath, ArtifactStoreError, ArtifactVerificationError, LocalArtifactStore,
    PublishedArtifact, StagedArtifactTask,
};
use crate::digest::Sha256Digest;
use crate::protocol::{
    ErrorCode, ProfileFailurePayload, ProfileIdentity, ProfileOutcome, ProfileRequestPayload,
    ProfileTaskPayload, PublishedArtifactKind, PublishedArtifactPayload,
};
use crate::protocol_mapper;
use crate::resource_budget::ResourceBudget;

#[cfg(test)]
pub(crate) use crate::application::capsule::{
    FFMPEG_PACKAGE_ENTRYPOINT, FFMPEG_PACKAGE_ID, FFMPEG_PROFILE_NAME, FFMPEG_PROFILE_VERSION,
    FILE_COPY_PROFILE_NAME, FILE_COPY_PROFILE_VERSION,
};
pub(crate) use crate::application::capsule::{ProfileError, ProfileStartupError};

#[derive(Debug)]
pub(crate) struct LocalProfileRuntime {
    artifacts: Arc<LocalArtifactStore>,
    registry: ProfileRegistry,
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
pub(crate) struct ProfileTaskRecord {
    task_id: String,
    profile: ProfileIdentity,
    budget: ResourceBudget,
    output_slot: String,
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

impl LocalProfileRuntime {
    pub(crate) fn open(
        root: &Path,
        maximum_artifact_bytes: u64,
        default_budget: ResourceBudget,
        ffmpeg_registration: Option<(&Path, Sha256Digest)>,
        bundle_cache_root: Option<&Path>,
    ) -> Result<Self, ProfileStartupError> {
        let registry = ProfileRegistry::open(
            maximum_artifact_bytes,
            default_budget,
            ffmpeg_registration,
            bundle_cache_root,
        )?;
        Ok(Self {
            artifacts: Arc::new(LocalArtifactStore::open(root, maximum_artifact_bytes)?),
            registry,
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
    ) -> Result<ResolvedProfile, ProfileError> {
        self.registry.resolve(request)
    }

    pub(crate) fn stage(
        &self,
        task_id: &str,
        prepared: ResolvedProfile,
    ) -> Result<StagedProfile, ProfileError> {
        let staged = self
            .artifacts
            .stage_input(task_id, prepared.source(), prepared.output())
            .map_err(profile_stage_error)?;
        Ok(prepared.into_staged(staged))
    }

    pub(crate) fn stage_daemon_input(
        &self,
        task_id: &str,
        prepared: ResolvedProfile,
        source: File,
    ) -> Result<StagedProfile, ProfileError> {
        let staged = self
            .artifacts
            .stage_daemon_input(task_id, prepared.source(), prepared.output(), source)
            .map_err(profile_stage_error)?;
        Ok(prepared.into_staged(staged))
    }

    pub(crate) fn open_published_artifact(&self, path: &str) -> Result<File, ProfileError> {
        let path = ArtifactPath::parse(path.to_owned())
            .map_err(|error| ProfileError::new(ErrorCode::InternalError, error.to_string()))?;
        self.artifacts
            .open_published_artifact(&path)
            .map_err(profile_stage_error)
    }

    pub(crate) fn new_task(
        &self,
        task_id: &str,
        request: &ProfileRequestPayload,
        budget: ResourceBudget,
        output_slot: String,
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
                finished_profile_payload(
                    raw,
                    self.profile.clone(),
                    self.output_slot.clone(),
                    terminal,
                )
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
    output_slot: String,
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
            termination_reason: protocol_mapper::termination_reason_to_protocol(termination_reason),
            process: protocol_mapper::process_result(process),
            timing: protocol_mapper::task_timing(timing),
            usage: protocol_mapper::task_usage(usage),
            output: protocol_mapper::task_output(output),
            artifacts: BTreeMap::from([(output_slot, published_wire(artifact))]),
            failure: None,
        },
        ProfileTerminal::Failed(failure) => ProfileTaskPayload::Finished {
            task_id,
            profile,
            profile_outcome: ProfileOutcome::Failed,
            termination_reason: protocol_mapper::termination_reason_to_protocol(termination_reason),
            process: protocol_mapper::process_result(process),
            timing: protocol_mapper::task_timing(timing),
            usage: protocol_mapper::task_usage(usage),
            output: protocol_mapper::task_output(output),
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
