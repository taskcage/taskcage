//! 인증 뒤 Remote operation을 Artifact store와 principal-scoped Task backend에 연결한다.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use crate::remote_artifact::{RemoteArtifactError, RemoteArtifactStore};
use crate::remote_config::PrincipalPolicy;
use crate::remote_protocol::{
    ArtifactIdPayload, REMOTE_PROTOCOL_VERSION, RemoteErrorCode, RemoteProfileRequestPayload,
    RemoteRequest, RemoteResponse, TaskIdPayload,
};
use crate::remote_server::{RemoteOperationHandler, RemoteResponseFuture, error_response};

pub type RemoteTaskFuture<'a> = Pin<Box<dyn Future<Output = RemoteResponse> + Send + 'a>>;

pub trait RemoteTaskBackend: Send + Sync + 'static {
    fn submit<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request_id: String,
        payload: RemoteProfileRequestPayload,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a>;

    fn get<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request_id: String,
        payload: TaskIdPayload,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a>;

    fn cancel<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request_id: String,
        payload: TaskIdPayload,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteTaskFuture<'a>;

    fn is_retained<'a>(
        &'a self,
        principal: &'a str,
        task_id: &'a str,
        artifacts: &'a RemoteArtifactStore,
    ) -> RemoteBoolFuture<'a>;
}

pub type RemoteBoolFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

pub struct RemoteDispatcher<B> {
    artifacts: RemoteArtifactStore,
    backend: Arc<B>,
    artifact_operations: Mutex<()>,
    submissions: Mutex<BTreeMap<(String, String), SubmissionRecord>>,
}

impl<B> RemoteDispatcher<B> {
    pub fn new(artifacts: RemoteArtifactStore, backend: Arc<B>) -> Self {
        Self {
            artifacts,
            backend,
            artifact_operations: Mutex::new(()),
            submissions: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn artifacts(&self) -> &RemoteArtifactStore {
        &self.artifacts
    }
}

#[derive(Clone)]
struct SubmissionRecord {
    canonical_payload: Vec<u8>,
    response: RemoteResponse,
    task_id: String,
    last_checked: Instant,
}

impl<B: RemoteTaskBackend> RemoteOperationHandler for RemoteDispatcher<B> {
    fn handle<'a>(
        &'a self,
        principal: &'a PrincipalPolicy,
        request: RemoteRequest,
    ) -> RemoteResponseFuture<'a> {
        Box::pin(async move { self.dispatch(principal, request).await })
    }
}

impl<B: RemoteTaskBackend> RemoteDispatcher<B> {
    async fn dispatch(
        &self,
        principal: &PrincipalPolicy,
        request: RemoteRequest,
    ) -> RemoteResponse {
        let request_id = request.request_id().to_owned();
        match request {
            RemoteRequest::BeginArtifactUpload { payload, .. } => {
                let _operation = self.artifact_operations.lock().await;
                match self.artifacts.begin_upload(principal, payload) {
                    Ok(payload) => RemoteResponse::ArtifactUploadStarted {
                        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload,
                    },
                    Err(error) => artifact_error(request_id, error),
                }
            }
            RemoteRequest::UploadArtifactChunk { payload, .. } => {
                let _operation = self.artifact_operations.lock().await;
                match self.artifacts.upload_chunk(&principal.client_id, payload) {
                    Ok(payload) => RemoteResponse::ArtifactChunkAccepted {
                        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload,
                    },
                    Err(error) => artifact_error(request_id, error),
                }
            }
            RemoteRequest::CompleteArtifactUpload {
                payload: ArtifactIdPayload { artifact_id },
                ..
            } => {
                let _operation = self.artifact_operations.lock().await;
                match self
                    .artifacts
                    .complete_upload(&principal.client_id, &artifact_id)
                {
                    Ok(payload) => RemoteResponse::ArtifactUploaded {
                        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload,
                    },
                    Err(error) => artifact_error(request_id, error),
                }
            }
            RemoteRequest::AbortArtifactUpload {
                payload: ArtifactIdPayload { artifact_id },
                ..
            } => {
                let _operation = self.artifact_operations.lock().await;
                match self
                    .artifacts
                    .abort_upload(&principal.client_id, &artifact_id)
                {
                    Ok(()) => RemoteResponse::ArtifactUploadAborted {
                        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload: ArtifactIdPayload { artifact_id },
                    },
                    Err(error) => artifact_error(request_id, error),
                }
            }
            RemoteRequest::ReadArtifactChunk { payload, .. } => {
                let _operation = self.artifact_operations.lock().await;
                match self.artifacts.read_output_chunk(
                    &principal.client_id,
                    &payload.artifact_id,
                    payload.offset,
                    payload.max_bytes,
                ) {
                    Ok(payload) => RemoteResponse::ArtifactChunk {
                        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                        request_id,
                        payload,
                    },
                    Err(error) => artifact_error(request_id, error),
                }
            }
            RemoteRequest::SubmitProfile { payload, .. } => {
                let _operation = self.artifact_operations.lock().await;
                self.submit(principal, request_id, payload).await
            }
            RemoteRequest::GetProfileResult { payload, .. } => {
                self.backend
                    .get(principal, request_id, payload, &self.artifacts)
                    .await
            }
            RemoteRequest::CancelTask { payload, .. } => {
                self.backend
                    .cancel(principal, request_id, payload, &self.artifacts)
                    .await
            }
            RemoteRequest::Authenticate { .. }
            | RemoteRequest::GetCapabilities { .. }
            | RemoteRequest::SubmitTask { .. } => error_response(
                request_id,
                RemoteErrorCode::InvalidRequest,
                "operation was dispatched at the wrong protocol layer",
                false,
            ),
        }
    }

    async fn submit(
        &self,
        principal: &PrincipalPolicy,
        request_id: String,
        payload: RemoteProfileRequestPayload,
    ) -> RemoteResponse {
        let canonical_payload = match serde_json_canonicalizer::to_vec(&payload) {
            Ok(bytes) => bytes,
            Err(_) => {
                return error_response(
                    request_id,
                    RemoteErrorCode::InvalidRequest,
                    "profile payload cannot be canonicalized",
                    false,
                );
            }
        };
        let key = (
            principal.client_id.clone(),
            payload.client_request_id.clone(),
        );
        // 같은 key의 concurrent submit도 backend 실행 소유권 하나로 직렬화한다.
        let mut submissions = self.submissions.lock().await;
        if let Some(existing) = submissions.get(&key).cloned() {
            if self
                .backend
                .is_retained(&key.0, &existing.task_id, &self.artifacts)
                .await
            {
                if let Some(current) = submissions.get_mut(&key) {
                    current.last_checked = Instant::now();
                }
                if existing.canonical_payload != canonical_payload {
                    return error_response(
                        request_id,
                        RemoteErrorCode::IdempotencyConflict,
                        "idempotency key was used with a different payload",
                        false,
                    );
                }
                return with_request_id(existing.response, request_id);
            }
            submissions.remove(&key);
        } else if let Some((candidate_key, candidate)) = submissions
            .iter()
            .min_by_key(|(_, record)| record.last_checked)
            .map(|(key, record)| (key.clone(), record.clone()))
        {
            if self
                .backend
                .is_retained(&candidate_key.0, &candidate.task_id, &self.artifacts)
                .await
            {
                if let Some(current) = submissions.get_mut(&candidate_key) {
                    current.last_checked = Instant::now();
                }
            } else {
                submissions.remove(&candidate_key);
            }
        }
        let response = self
            .backend
            .submit(principal, request_id, payload, &self.artifacts)
            .await;
        if matches!(
            response,
            RemoteResponse::ProfileAccepted { .. } | RemoteResponse::ProfileResult { .. }
        ) && let Some(task_id) = response_task_id(&response)
        {
            submissions.insert(
                key,
                SubmissionRecord {
                    canonical_payload,
                    response: response.clone(),
                    task_id: task_id.to_owned(),
                    last_checked: Instant::now(),
                },
            );
        }
        response
    }
}

fn response_task_id(response: &RemoteResponse) -> Option<&str> {
    match response {
        RemoteResponse::ProfileAccepted { payload, .. } => Some(&payload.task_id),
        RemoteResponse::ProfileResult { payload, .. } => match payload {
            crate::remote_protocol::ProfileTaskPayload::Running { task_id, .. }
            | crate::remote_protocol::ProfileTaskPayload::Finished { task_id, .. } => Some(task_id),
        },
        _ => None,
    }
}

fn artifact_error(request_id: String, error: RemoteArtifactError) -> RemoteResponse {
    let (code, retryable) = error.wire_code();
    let message = match code {
        RemoteErrorCode::InternalError => "internal Artifact store error".to_owned(),
        _ => error.to_string(),
    };
    error_response(request_id, code, message, retryable)
}

fn with_request_id(response: RemoteResponse, request_id: String) -> RemoteResponse {
    match response {
        RemoteResponse::ProfileAccepted {
            remote_protocol_version,
            payload,
            ..
        } => RemoteResponse::ProfileAccepted {
            remote_protocol_version,
            request_id,
            payload,
        },
        RemoteResponse::ProfileResult {
            remote_protocol_version,
            payload,
            ..
        } => RemoteResponse::ProfileResult {
            remote_protocol_version,
            request_id,
            payload,
        },
        other => other,
    }
}
