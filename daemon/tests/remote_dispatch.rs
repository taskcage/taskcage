use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use taskcaged::remote_artifact::RemoteArtifactStore;
use taskcaged::remote_config::PrincipalPolicy;
use taskcaged::remote_dispatch::{
    RemoteBoolFuture, RemoteDispatcher, RemoteTaskBackend, RemoteTaskFuture,
};
use taskcaged::remote_protocol::{
    BeginArtifactUploadPayload, CpuMax, OutputLimits, ProfileEffectiveResources, ProfileIdentity,
    RemoteErrorCode, RemoteProfileRequestPayload, RemoteRequest, RemoteResponse, ResourceLimits,
    TaskIdPayload, TaskState,
};
use taskcaged::remote_server::{RemoteOperationHandler, error_response};
use tokio::sync::Notify;
use tokio::time::timeout;

const TASK_ID: &str = "55555555-5555-4555-8555-555555555555";

struct MockBackend {
    submits: AtomicUsize,
    retained: AtomicBool,
    block_submit: AtomicBool,
    submit_started: Notify,
    submit_release: Notify,
}

impl RemoteTaskBackend for MockBackend {
    fn submit<'a>(
        &'a self,
        _principal: &'a PrincipalPolicy,
        request_id: String,
        payload: RemoteProfileRequestPayload,
        _artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.block_submit.load(Ordering::SeqCst) {
                self.submit_started.notify_one();
                self.submit_release.notified().await;
            }
            RemoteResponse::ProfileAccepted {
                remote_protocol_version: 1,
                request_id,
                payload: taskcaged::remote_protocol::ProfileAcceptedPayload {
                    task_id: TASK_ID.to_owned(),
                    state: TaskState::Running,
                    profile: payload.profile,
                    effective_resources: ProfileEffectiveResources {
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
                    },
                },
            }
        })
    }

    fn get<'a>(
        &'a self,
        _principal: &'a PrincipalPolicy,
        request_id: String,
        _payload: TaskIdPayload,
        _artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            error_response(
                request_id,
                RemoteErrorCode::TaskNotFound,
                "task was not found",
                false,
            )
        })
    }

    fn cancel<'a>(
        &'a self,
        _principal: &'a PrincipalPolicy,
        request_id: String,
        _payload: TaskIdPayload,
        _artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a> {
        Box::pin(async move {
            error_response(
                request_id,
                RemoteErrorCode::TaskNotFound,
                "task was not found",
                false,
            )
        })
    }

    fn is_retained<'a>(
        &'a self,
        _principal: &'a str,
        _task_id: &'a str,
        _artifacts: &'a RemoteArtifactStore,
    ) -> RemoteBoolFuture<'a> {
        Box::pin(async move { self.retained.load(Ordering::SeqCst) })
    }
}

fn principal(client_id: &str) -> PrincipalPolicy {
    PrincipalPolicy {
        client_id: client_id.to_owned(),
        secret_verifier: "redacted-test-verifier".to_owned(),
        allowed_profiles: BTreeSet::new(),
        allow_all_installed_capsules: false,
        maximum_resource_overrides: None,
        artifact_upload_allowed: true,
        max_principal_artifact_bytes: NonZeroU64::new(1_000_000).expect("positive"),
        max_principal_artifacts: NonZeroUsize::new(4).expect("positive"),
    }
}

fn store(root: &Path) -> RemoteArtifactStore {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .expect("protect test root");
    }
    RemoteArtifactStore::open(root, 1_000_000, 780_000, Duration::from_secs(600))
        .expect("artifact store")
}

fn submit_request(name: &str, request_id: &str) -> RemoteRequest {
    RemoteRequest::SubmitProfile {
        remote_protocol_version: 1,
        request_id: request_id.to_owned(),
        payload: RemoteProfileRequestPayload {
            client_request_id: "44444444-4444-4444-8444-444444444444".to_owned(),
            profile: ProfileIdentity {
                name: name.to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BTreeMap::new(),
            resource_overrides: None,
        },
    }
}

#[tokio::test]
async fn principal_scoped_idempotency_is_resolved_before_backend_artifact_lookup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let backend = Arc::new(MockBackend {
        submits: AtomicUsize::new(0),
        retained: AtomicBool::new(true),
        block_submit: AtomicBool::new(false),
        submit_started: Notify::new(),
        submit_release: Notify::new(),
    });
    let dispatcher = RemoteDispatcher::new(store(temporary.path()), Arc::clone(&backend));
    let first = dispatcher
        .handle(
            &principal("document-worker"),
            submit_request(
                "ffmpeg-audio-to-wav",
                "33333333-3333-4333-8333-333333333333",
            ),
        )
        .await;
    assert!(matches!(first, RemoteResponse::ProfileAccepted { .. }));

    let retry = dispatcher
        .handle(
            &principal("document-worker"),
            submit_request(
                "ffmpeg-audio-to-wav",
                "99999999-9999-4999-8999-999999999999",
            ),
        )
        .await;
    assert!(matches!(retry, RemoteResponse::ProfileAccepted { .. }));
    assert_eq!(backend.submits.load(Ordering::SeqCst), 1);

    let conflict = dispatcher
        .handle(
            &principal("document-worker"),
            submit_request("file-copy", "88888888-8888-4888-8888-888888888888"),
        )
        .await;
    assert!(matches!(
        conflict,
        RemoteResponse::Error {
            payload: taskcaged::remote_protocol::ErrorPayload {
                code: RemoteErrorCode::IdempotencyConflict,
                ..
            },
            ..
        }
    ));
    assert_eq!(backend.submits.load(Ordering::SeqCst), 1);

    let other_principal = dispatcher
        .handle(
            &principal("other-worker"),
            submit_request(
                "ffmpeg-audio-to-wav",
                "77777777-7777-4777-8777-777777777777",
            ),
        )
        .await;
    assert!(matches!(
        other_principal,
        RemoteResponse::ProfileAccepted { .. }
    ));
    assert_eq!(backend.submits.load(Ordering::SeqCst), 2);

    backend.retained.store(false, Ordering::SeqCst);
    let after_retention = dispatcher
        .handle(
            &principal("document-worker"),
            submit_request("file-copy", "66666666-6666-4666-8666-666666666666"),
        )
        .await;
    assert!(matches!(
        after_retention,
        RemoteResponse::ProfileAccepted { .. }
    ));
    assert_eq!(backend.submits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn unrelated_artifact_operation_does_not_wait_for_slow_submit() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let backend = Arc::new(MockBackend {
        submits: AtomicUsize::new(0),
        retained: AtomicBool::new(true),
        block_submit: AtomicBool::new(true),
        submit_started: Notify::new(),
        submit_release: Notify::new(),
    });
    let dispatcher = Arc::new(RemoteDispatcher::new(
        store(temporary.path()),
        Arc::clone(&backend),
    ));

    let submit_dispatcher = Arc::clone(&dispatcher);
    let submit = tokio::spawn(async move {
        submit_dispatcher
            .handle(
                &principal("document-worker"),
                submit_request(
                    "ffmpeg-audio-to-wav",
                    "33333333-3333-4333-8333-333333333333",
                ),
            )
            .await
    });
    backend.submit_started.notified().await;

    let upload_dispatcher = Arc::clone(&dispatcher);
    let mut upload = tokio::spawn(async move {
        upload_dispatcher
            .handle(
                &principal("other-worker"),
                RemoteRequest::BeginArtifactUpload {
                    remote_protocol_version: 1,
                    request_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
                    payload: BeginArtifactUploadPayload {
                        client_artifact_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
                        digest: format!("sha256:{}", "a".repeat(64)),
                        size_bytes: 1,
                        media_type: None,
                    },
                },
            )
            .await
    });
    assert!(matches!(
        timeout(Duration::from_secs(1), &mut upload)
            .await
            .expect("unrelated upload must not wait for submit")
            .expect("upload task"),
        RemoteResponse::ArtifactUploadStarted { .. }
    ));

    backend.submit_release.notify_one();
    assert!(matches!(
        submit.await.expect("submit task"),
        RemoteResponse::ProfileAccepted { .. }
    ));
}
