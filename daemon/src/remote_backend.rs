//! Remote Profile 요청을 기존 cgroup/Profile core에 연결한다.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::application::task::SubmitCoordinator;
use crate::fail_stop::CleanupFailureReport;
use crate::handlers::{ProtocolHandlers, RequestHandling};
use crate::protocol as local;
use crate::remote_artifact::{ManagedInputSnapshot, RemoteArtifactError, RemoteArtifactStore};
use crate::remote_config::PrincipalPolicy;
use crate::remote_dispatch::{RemoteBoolFuture, RemoteTaskBackend, RemoteTaskFuture};
use crate::remote_protocol as remote;
use crate::remote_protocol_mapper as mapper;
use crate::remote_server::error_response;

#[derive(Clone)]
pub(crate) struct LocalProfileRemoteBackend {
    handlers: Arc<ProtocolHandlers<SubmitCoordinator>>,
    cleanup_timeout: Duration,
    input_owners: Arc<Mutex<HashMap<String, String>>>,
    finished: Arc<Mutex<HashMap<String, remote::ProfileTaskPayload>>>,
}

impl LocalProfileRemoteBackend {
    pub(crate) fn new(
        handlers: Arc<ProtocolHandlers<SubmitCoordinator>>,
        cleanup_timeout: Duration,
    ) -> Self {
        Self {
            handlers,
            cleanup_timeout,
            input_owners: Arc::new(Mutex::new(HashMap::new())),
            finished: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl RemoteTaskBackend for LocalProfileRemoteBackend {
    fn submit<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request_id: String,
        payload: remote::RemoteProfileRequestPayload,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            let artifact_ids = mapper::managed_input_ids(&payload);
            let completed = match artifacts.inspect_completed(&principal.client_id, &artifact_ids) {
                Ok(completed) => completed,
                Err(error) => return artifact_error(request_id, error),
            };
            let prevalidated = match mapper::local_profile_payload(principal, &payload, &completed)
            {
                Ok(payload) => payload,
                Err(error) => return artifact_error(request_id, error),
            };
            if let Some(response) = self
                .handlers
                .prevalidate_remote_profile(&request_id, &prevalidated)
            {
                let local::Response::Error { payload, .. } = response else {
                    unreachable!("Remote Profile prevalidation only returns errors")
                };
                return local_error(request_id, payload);
            }
            let ownership_token = mapper::namespaced_uuid(
                &principal.client_id,
                &payload.client_request_id,
                b"remote-input-ownership",
            );
            let snapshots = match artifacts.transfer_inputs(
                &principal.client_id,
                &ownership_token,
                &artifact_ids,
            ) {
                Ok(snapshots) => snapshots,
                Err(error) => return artifact_error(request_id, error),
            };
            let local_payload = match mapper::local_profile_payload(principal, &payload, &snapshots)
            {
                Ok(payload) => payload,
                Err(error) => {
                    if artifacts.restore_task_inputs(&ownership_token).is_err() {
                        self.activate_cleanup_failure(
                            &ownership_token,
                            "remote-input-rollback",
                            "task-owned input",
                        );
                    }
                    return artifact_error(request_id, error);
                }
            };
            let Some(source_snapshot) = snapshots.first() else {
                if artifacts.restore_task_inputs(&ownership_token).is_err() {
                    self.activate_cleanup_failure(
                        &ownership_token,
                        "remote-input-rollback",
                        "task-owned input",
                    );
                }
                return error_response(
                    request_id,
                    remote::RemoteErrorCode::InvalidProfileInput,
                    "Remote Profile requires one MANAGED_INPUT source",
                    false,
                );
            };
            let source = match open_managed_input(source_snapshot) {
                Ok(source) => source,
                Err(error) => {
                    if artifacts.restore_task_inputs(&ownership_token).is_err() {
                        self.activate_cleanup_failure(
                            &ownership_token,
                            "remote-input-rollback",
                            "task-owned input",
                        );
                    }
                    return artifact_error(request_id, error);
                }
            };
            let response = self
                .handlers
                .handle_submit_remote_profile(
                    local::Request::SubmitProfile {
                        protocol_version: local::PROFILE_PROTOCOL_VERSION,
                        request_id: request_id.clone(),
                        payload: local_payload,
                    },
                    || crate::server::submit_context(self.cleanup_timeout),
                    source,
                    principal.client_id.clone(),
                )
                .await;
            match response {
                local::Response::ProfileAccepted { payload, .. } => {
                    let task_id = payload.task_id.clone();
                    self.handlers
                        .register_remote_task(payload.task_id.clone(), principal.client_id.clone());
                    self.input_owners
                        .lock()
                        .expect("remote input owner state poisoned")
                        .insert(payload.task_id.clone(), ownership_token);
                    self.spawn_completion_monitor(principal.clone(), task_id, artifacts.clone());
                    mapper::accepted(request_id, payload)
                }
                local::Response::ProfileResult { payload, .. } => {
                    let task_id = mapper::local_profile_task_id(&payload).to_owned();
                    self.handlers
                        .register_remote_task(task_id.clone(), principal.client_id.clone());
                    self.input_owners
                        .lock()
                        .expect("remote input owner state poisoned")
                        .insert(task_id, ownership_token);
                    self.convert_profile_result(principal, request_id, payload, artifacts)
                }
                local::Response::Error { payload, .. } => {
                    if artifacts.restore_task_inputs(&ownership_token).is_err() {
                        self.activate_cleanup_failure(
                            &ownership_token,
                            "remote-input-rollback",
                            "task-owned input",
                        );
                        return error_response(
                            request_id,
                            remote::RemoteErrorCode::InternalError,
                            "input ownership rollback failed",
                            true,
                        );
                    }
                    local_error(request_id, payload)
                }
                _ => {
                    if artifacts.restore_task_inputs(&ownership_token).is_err() {
                        self.activate_cleanup_failure(
                            &ownership_token,
                            "remote-input-rollback",
                            "task-owned input",
                        );
                    }
                    error_response(
                        request_id,
                        remote::RemoteErrorCode::InternalError,
                        "unexpected local Profile response",
                        true,
                    )
                }
            }
        })
    }

    fn get<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request_id: String,
        payload: remote::TaskIdPayload,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            if !self
                .handlers
                .remote_task_owned_by(&payload.task_id, &principal.client_id)
            {
                return task_not_found(request_id);
            }
            let task_id = payload.task_id.clone();
            let response = self
                .handlers
                .handle_get_profile_result(local::Request::GetProfileResult {
                    protocol_version: local::PROFILE_PROTOCOL_VERSION,
                    request_id: request_id.clone(),
                    payload: local::TaskIdPayload {
                        task_id: payload.task_id,
                    },
                })
                .await;
            match response {
                local::Response::ProfileResult { payload, .. } => {
                    self.convert_profile_result(principal, request_id, payload, artifacts)
                }
                local::Response::Error { payload, .. } => {
                    if payload.code == local::ErrorCode::TaskNotFound {
                        self.forget_task(&principal.client_id, &task_id, artifacts);
                    }
                    local_error(request_id, payload)
                }
                _ => error_response(
                    request_id,
                    remote::RemoteErrorCode::InternalError,
                    "unexpected local Profile result response",
                    true,
                ),
            }
        })
    }

    fn cancel<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request_id: String,
        payload: remote::TaskIdPayload,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            if !self
                .handlers
                .remote_task_owned_by(&payload.task_id, &principal.client_id)
            {
                return task_not_found(request_id);
            }
            let response = self
                .handlers
                .handle_cancel(local::Request::CancelTask {
                    protocol_version: local::PROTOCOL_VERSION,
                    request_id: request_id.clone(),
                    payload: local::TaskIdPayload {
                        task_id: payload.task_id,
                    },
                })
                .await;
            match response {
                RequestHandling::Handled(local::Response::TaskCancelled { payload, .. }) => {
                    let owner = self
                        .input_owners
                        .lock()
                        .expect("remote input owner state poisoned")
                        .remove(&payload.task_id);
                    if owner
                        .as_deref()
                        .is_some_and(|owner| artifacts.cleanup_task_inputs(owner).is_err())
                    {
                        self.activate_cleanup_failure(
                            &payload.task_id,
                            "remote-cancel-input-cleanup",
                            "task-owned input",
                        );
                        return error_response(
                            request_id,
                            remote::RemoteErrorCode::InternalError,
                            "managed input cleanup failed",
                            true,
                        );
                    }
                    remote::RemoteResponse::TaskCancelled {
                        remote_protocol_version: remote::REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload: remote::TaskCancelledPayload {
                            task_id: payload.task_id,
                            state: remote::TaskState::Finished,
                            termination_reason: remote::TerminationReason::Cancelled,
                        },
                    }
                }
                RequestHandling::Handled(local::Response::Error { payload, .. }) => {
                    local_error(request_id, payload)
                }
                _ => error_response(
                    request_id,
                    remote::RemoteErrorCode::InternalError,
                    "unexpected local cancel response",
                    true,
                ),
            }
        })
    }

    fn is_retained<'a>(
        &'a self,
        principal: &'a str,
        task_id: &'a str,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteBoolFuture<'a> {
        Box::pin(async move {
            if !self.handlers.remote_task_owned_by(task_id, principal) {
                return false;
            }
            let request_id = mapper::namespaced_uuid(principal, task_id, b"remote-retention-check");
            let response = self
                .handlers
                .handle_get_profile_result(local::Request::GetProfileResult {
                    protocol_version: local::PROFILE_PROTOCOL_VERSION,
                    request_id,
                    payload: local::TaskIdPayload {
                        task_id: task_id.to_owned(),
                    },
                })
                .await;
            match response {
                local::Response::ProfileResult { .. } => true,
                local::Response::Error { payload, .. }
                    if payload.code == local::ErrorCode::TaskNotFound =>
                {
                    self.forget_task(principal, task_id, artifacts);
                    false
                }
                _ => true,
            }
        })
    }
}

impl LocalProfileRemoteBackend {
    fn spawn_completion_monitor(
        &self,
        principal: PrincipalPolicy,
        task_id: String,
        artifacts: RemoteArtifactStore,
    ) {
        let backend = self.clone();
        tokio::spawn(async move {
            let request_id = mapper::namespaced_uuid(
                &principal.client_id,
                &task_id,
                b"remote-completion-monitor",
            );
            loop {
                let response = backend
                    .handlers
                    .handle_get_profile_result(local::Request::GetProfileResult {
                        protocol_version: local::PROFILE_PROTOCOL_VERSION,
                        request_id: request_id.clone(),
                        payload: local::TaskIdPayload {
                            task_id: task_id.clone(),
                        },
                    })
                    .await;
                match response {
                    local::Response::ProfileResult {
                        payload: local::ProfileTaskPayload::Running { .. },
                        ..
                    } => tokio::time::sleep(Duration::from_millis(100)).await,
                    local::Response::ProfileResult { payload, .. } => {
                        let _ = backend
                            .convert_profile_result(&principal, request_id, payload, &artifacts);
                        break;
                    }
                    _ => {
                        if backend.cleanup_remote_inputs(&task_id, &artifacts).is_err() {
                            backend.activate_cleanup_failure(
                                &task_id,
                                "remote-monitor-input-cleanup",
                                "task-owned input",
                            );
                        }
                        break;
                    }
                }
            }
        });
    }

    fn convert_profile_result(
        &self,
        principal: &PrincipalPolicy,
        request_id: String,
        payload: local::ProfileTaskPayload,
        artifacts: &RemoteArtifactStore,
    ) -> remote::RemoteResponse {
        match payload {
            local::ProfileTaskPayload::Running {
                task_id,
                profile,
                submitted_at,
                started_at,
            } => remote::RemoteResponse::ProfileResult {
                remote_protocol_version: remote::REMOTE_PROTOCOL_VERSION,
                request_id,
                payload: remote::ProfileTaskPayload::Running {
                    task_id,
                    profile: mapper::profile_identity(profile),
                    submitted_at,
                    started_at,
                },
            },
            local::ProfileTaskPayload::Finished {
                task_id,
                profile,
                profile_outcome,
                termination_reason,
                process,
                timing,
                usage,
                output,
                artifacts: local_artifacts,
                failure,
            } => {
                let mut finished_cache = self
                    .finished
                    .lock()
                    .expect("remote finished state poisoned");
                if let Some(existing) = finished_cache.get(&task_id).cloned() {
                    return remote::RemoteResponse::ProfileResult {
                        remote_protocol_version: remote::REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload: existing,
                    };
                }
                let mut managed = BTreeMap::new();
                for (slot, artifact) in local_artifacts {
                    let source = match self.handlers.open_remote_profile_output(&artifact.path) {
                        Ok(source) => source,
                        Err(_) => {
                            if self.cleanup_remote_inputs(&task_id, artifacts).is_err() {
                                self.activate_cleanup_failure(
                                    &task_id,
                                    "remote-output-open-input-cleanup",
                                    "task-owned input",
                                );
                            }
                            self.activate_cleanup_failure(
                                &task_id,
                                "remote-output-open",
                                "local published output",
                            );
                            return error_response(
                                request_id,
                                remote::RemoteErrorCode::InternalError,
                                "managed output publication failed",
                                true,
                            );
                        }
                    };
                    let published = match artifacts.publish_output_file(
                        &principal.client_id,
                        source,
                        &artifact.digest,
                        artifact.size_bytes,
                        &artifact.media_type,
                    ) {
                        Ok(published) => published,
                        Err(_) => {
                            if self.cleanup_remote_inputs(&task_id, artifacts).is_err() {
                                self.activate_cleanup_failure(
                                    &task_id,
                                    "remote-output-failure-input-cleanup",
                                    "task-owned input",
                                );
                            }
                            self.activate_cleanup_failure(
                                &task_id,
                                "remote-output-publication",
                                "managed output",
                            );
                            return error_response(
                                request_id,
                                remote::RemoteErrorCode::InternalError,
                                "managed output publication failed",
                                true,
                            );
                        }
                    };
                    managed.insert(slot, published);
                }
                if self.cleanup_remote_inputs(&task_id, artifacts).is_err() {
                    self.activate_cleanup_failure(
                        &task_id,
                        "remote-finished-input-cleanup",
                        "task-owned input",
                    );
                    return error_response(
                        request_id,
                        remote::RemoteErrorCode::InternalError,
                        "managed input cleanup failed",
                        true,
                    );
                }
                let remote_payload = remote::ProfileTaskPayload::Finished {
                    task_id: task_id.clone(),
                    profile: mapper::profile_identity(profile),
                    profile_outcome: mapper::profile_outcome(profile_outcome),
                    termination_reason: mapper::termination_reason(termination_reason),
                    process: mapper::process_result(process),
                    timing: mapper::task_timing(timing),
                    usage: mapper::task_usage(usage),
                    output: mapper::task_output(output),
                    artifacts: managed,
                    failure: failure.map(mapper::profile_failure),
                };
                finished_cache.insert(task_id, remote_payload.clone());
                remote::RemoteResponse::ProfileResult {
                    remote_protocol_version: remote::REMOTE_PROTOCOL_VERSION,
                    request_id,
                    payload: remote_payload,
                }
            }
        }
    }

    fn cleanup_remote_inputs(
        &self,
        task_id: &str,
        artifacts: &RemoteArtifactStore,
    ) -> Result<(), RemoteArtifactError> {
        let owner = self
            .input_owners
            .lock()
            .expect("remote input owner state poisoned")
            .remove(task_id);
        if let Some(owner) = owner {
            artifacts.cleanup_task_inputs(&owner)?;
        }
        Ok(())
    }

    fn forget_task(&self, principal: &str, task_id: &str, artifacts: &RemoteArtifactStore) {
        self.handlers
            .remove_remote_task_if_owned(task_id, principal);
        self.finished
            .lock()
            .expect("remote finished state poisoned")
            .remove(task_id);
        if self.cleanup_remote_inputs(task_id, artifacts).is_err() {
            self.activate_cleanup_failure(
                task_id,
                "remote-retention-input-cleanup",
                "task-owned input",
            );
        }
    }

    fn activate_cleanup_failure(
        &self,
        task_id: &str,
        stage: &'static str,
        uncleaned: &'static str,
    ) {
        self.handlers
            .fail_stop()
            .activate(CleanupFailureReport::new(
                task_id,
                stage,
                vec![uncleaned],
                "remote cleanup retry is not safe",
            ));
    }
}

fn open_managed_input(snapshot: &ManagedInputSnapshot) -> Result<File, RemoteArtifactError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options
        .open(&snapshot.path)
        .map_err(|source| RemoteArtifactError::Io {
            operation: "open task-owned managed input",
            path: snapshot.path.clone(),
            source,
        })
}

fn local_error(request_id: String, payload: local::ErrorPayload) -> remote::RemoteResponse {
    let code = match payload.code {
        local::ErrorCode::EnvironmentUnavailable => remote::RemoteErrorCode::EnvironmentUnavailable,
        local::ErrorCode::CapacityExhausted => remote::RemoteErrorCode::CapacityExhausted,
        local::ErrorCode::TaskNotFound | local::ErrorCode::TaskKindMismatch => {
            remote::RemoteErrorCode::TaskNotFound
        }
        local::ErrorCode::TaskAlreadyFinished => remote::RemoteErrorCode::TaskAlreadyFinished,
        local::ErrorCode::IdempotencyConflict => remote::RemoteErrorCode::IdempotencyConflict,
        local::ErrorCode::LimitExceedsPolicy => remote::RemoteErrorCode::LimitExceedsPolicy,
        local::ErrorCode::ProfileNotFound => remote::RemoteErrorCode::ProfileNotFound,
        local::ErrorCode::InvalidProfileInput
        | local::ErrorCode::InvalidArtifactPath
        | local::ErrorCode::ArtifactDigestMismatch => remote::RemoteErrorCode::InvalidProfileInput,
        local::ErrorCode::InternalError => remote::RemoteErrorCode::InternalError,
        local::ErrorCode::InvalidRequest
        | local::ErrorCode::UnsupportedProtocolVersion
        | local::ErrorCode::FrameTooLarge => remote::RemoteErrorCode::InvalidRequest,
    };
    error_response(request_id, code, payload.message, payload.retryable)
}

#[cfg(test)]
#[allow(
    clippy::items_after_test_module,
    reason = "Linux 통합 test는 Remote backend 구현 가까이에 둔다"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::Duration;

    use base64::Engine;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::application::task::TaskRegistrySettings;
    use crate::capacity::TaskCapacitySettings;
    use crate::deployment_policy::DeploymentResourcePolicy;
    use crate::fail_stop::{FailStopCoordinator, FailStopSettings};
    use crate::preflight::{CapabilityProbe, SystemProbe};
    use crate::profile::{FILE_COPY_PROFILE_NAME, FILE_COPY_PROFILE_VERSION, LocalProfileRuntime};
    use crate::remote_config::ProfileIdentityKey;
    use crate::resource_budget::ResourceBudget;

    fn profile_budget() -> ResourceBudget {
        ResourceBudget::try_from_protocol(
            local::ResourceLimits {
                cpu_max: local::CpuMax {
                    quota_micros: 50_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 64 * 1024 * 1024,
                pids_max: 8,
                wall_time_limit_ms: 5_000,
            },
            local::OutputLimits {
                stdout_tail_max_bytes: 1_024,
                stderr_tail_max_bytes: 1_024,
            },
        )
        .expect("Remote Profile test budget")
    }

    fn principal() -> PrincipalPolicy {
        PrincipalPolicy {
            client_id: "document-worker".to_owned(),
            secret_verifier: "test-only-verifier".to_owned(),
            allowed_profiles: BTreeSet::from([ProfileIdentityKey {
                name: FILE_COPY_PROFILE_NAME.to_owned(),
                version: FILE_COPY_PROFILE_VERSION.to_owned(),
            }]),
            maximum_resource_overrides: None,
            artifact_upload_allowed: true,
            max_principal_artifact_bytes: NonZeroU64::new(1_000_000).unwrap(),
            max_principal_artifacts: NonZeroUsize::new(4).unwrap(),
        }
    }

    #[tokio::test]
    async fn actual_remote_managed_input_runs_and_publishes_after_cgroup_cleanup() {
        if std::env::var_os("TASKCAGE_RUN_LINUX_PROFILE_INTEGRATION").is_none() {
            eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
            return;
        }

        let suffix = format!("{}", std::process::id());
        let local_root = std::env::temp_dir().join(format!("taskcage-remote-local-{suffix}"));
        let remote_root = std::env::temp_dir().join(format!("taskcage-remote-store-{suffix}"));
        let _ = fs::remove_dir_all(&local_root);
        let _ = fs::remove_dir_all(&remote_root);
        fs::create_dir(&local_root).unwrap();
        fs::create_dir(&remote_root).unwrap();
        fs::set_permissions(&local_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&remote_root, fs::Permissions::from_mode(0o700)).unwrap();

        let environment = SystemProbe::from_environment().check().unwrap();
        let jobs_path = environment.report().delegated_root.join("jobs");
        let runtime =
            LocalProfileRuntime::open(&local_root, 1_000_000, profile_budget(), None, None)
                .expect("Local Profile runtime");
        let handlers = Arc::new(
            ProtocolHandlers::initialize(
                Ok(environment),
                TaskCapacitySettings::new(1).unwrap(),
                TaskRegistrySettings::new(16).unwrap(),
                DeploymentResourcePolicy::for_test(),
                FailStopCoordinator::new(FailStopSettings::new(Duration::from_secs(5)).unwrap()),
                Some(runtime),
            )
            .unwrap(),
        );
        let artifacts =
            RemoteArtifactStore::open(&remote_root, 1_000_000, 780_000, Duration::from_secs(600))
                .unwrap();
        let bytes = b"Remote MANAGED_INPUT through cgroup\n";
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        let policy = principal();
        let started = artifacts
            .begin_upload(
                &policy,
                remote::BeginArtifactUploadPayload {
                    client_artifact_id: "41414141-4141-4141-8141-414141414141".to_owned(),
                    digest: digest.clone(),
                    size_bytes: bytes.len() as u64,
                    media_type: Some("text/plain".to_owned()),
                },
            )
            .unwrap();
        artifacts
            .upload_chunk(
                &policy.client_id,
                remote::UploadArtifactChunkPayload {
                    artifact_id: started.artifact_id.clone(),
                    offset: 0,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                },
            )
            .unwrap();
        artifacts
            .complete_upload(&policy.client_id, &started.artifact_id)
            .unwrap();

        let backend = LocalProfileRemoteBackend::new(Arc::clone(&handlers), Duration::from_secs(5));
        let payload = remote::RemoteProfileRequestPayload {
            client_request_id: "42424242-4242-4242-8242-424242424242".to_owned(),
            profile: remote::ProfileIdentity {
                name: FILE_COPY_PROFILE_NAME.to_owned(),
                version: FILE_COPY_PROFILE_VERSION.to_owned(),
            },
            inputs: BTreeMap::from([
                (
                    "source".to_owned(),
                    remote::RemoteProfileInputValue::ManagedInput {
                        artifact_id: started.artifact_id.clone(),
                    },
                ),
                (
                    "label".to_owned(),
                    remote::RemoteProfileInputValue::String {
                        value: "remote-e2e".to_owned(),
                    },
                ),
                (
                    "retain_metadata".to_owned(),
                    remote::RemoteProfileInputValue::Boolean { value: false },
                ),
                (
                    "priority".to_owned(),
                    remote::RemoteProfileInputValue::Int64 { value: 50 },
                ),
            ]),
            resource_overrides: None,
        };
        let submitted = backend
            .submit(
                &policy,
                "43434343-4343-4343-8343-434343434343".to_owned(),
                payload,
                &artifacts,
            )
            .await;
        let task_id = match submitted {
            remote::RemoteResponse::ProfileAccepted { payload, .. } => payload.task_id,
            other => panic!("Remote Profile submit must create a running task: {other:#?}"),
        };
        let finished = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response = backend
                    .get(
                        &policy,
                        "44444444-4444-4444-8444-444444444444".to_owned(),
                        remote::TaskIdPayload {
                            task_id: task_id.clone(),
                        },
                        &artifacts,
                    )
                    .await;
                if matches!(
                    response,
                    remote::RemoteResponse::ProfileResult {
                        payload: remote::ProfileTaskPayload::Finished { .. },
                        ..
                    }
                ) {
                    break response;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Remote Profile must finish after cgroup cleanup");
        let output = match finished {
            remote::RemoteResponse::ProfileResult {
                payload:
                    remote::ProfileTaskPayload::Finished {
                        profile_outcome: remote::ProfileOutcome::Succeeded,
                        termination_reason: remote::TerminationReason::Exited,
                        artifacts,
                        failure: None,
                        ..
                    },
                ..
            } => artifacts.into_values().next().expect("managed output"),
            other => panic!("unexpected Remote Profile result: {other:#?}"),
        };
        let downloaded = artifacts
            .read_output_chunk(&policy.client_id, &output.artifact_id, 0, 780_000)
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(downloaded.data_base64)
                .unwrap(),
            bytes
        );
        assert!(matches!(
            artifacts.abort_upload(&policy.client_id, &started.artifact_id),
            Err(RemoteArtifactError::NotFound)
        ));
        assert_eq!(
            fs::read_dir(&jobs_path)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
                .count(),
            0
        );

        drop(backend);
        drop(artifacts);
        drop(handlers);
        fs::remove_dir_all(local_root).unwrap();
        fs::remove_dir_all(remote_root).unwrap();
    }
}

fn artifact_error(request_id: String, error: RemoteArtifactError) -> remote::RemoteResponse {
    let (code, retryable) = error.wire_code();
    error_response(request_id, code, error.to_string(), retryable)
}

fn task_not_found(request_id: String) -> remote::RemoteResponse {
    error_response(
        request_id,
        remote::RemoteErrorCode::TaskNotFound,
        "task was not found",
        false,
    )
}
