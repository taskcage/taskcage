//! Remote Artifact upload, abort, download 요청을 outbound store에 연결한다.

use crate::remote_artifact::{RemoteArtifactError, RemoteArtifactStore};
use crate::remote_config::PrincipalPolicy;
use crate::remote_protocol::{
    ArtifactIdPayload, BeginArtifactUploadPayload, REMOTE_PROTOCOL_VERSION,
    ReadArtifactChunkPayload, RemoteErrorCode, RemoteResponse, UploadArtifactChunkPayload,
};

use super::server::error_response;

pub(super) enum ArtifactTransferOperation {
    Begin(BeginArtifactUploadPayload),
    Upload(UploadArtifactChunkPayload),
    Complete(ArtifactIdPayload),
    Abort(ArtifactIdPayload),
    Read(ReadArtifactChunkPayload),
}

pub(super) fn dispatch(
    artifacts: &RemoteArtifactStore,
    principal: &PrincipalPolicy,
    request_id: String,
    operation: ArtifactTransferOperation,
) -> RemoteResponse {
    match operation {
        ArtifactTransferOperation::Begin(payload) => {
            match artifacts.begin_upload(principal, payload) {
                Ok(payload) => RemoteResponse::ArtifactUploadStarted {
                    remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                    request_id,
                    payload,
                },
                Err(error) => artifact_error(request_id, error),
            }
        }
        ArtifactTransferOperation::Upload(payload) => {
            match artifacts.upload_chunk(&principal.client_id, payload) {
                Ok(payload) => RemoteResponse::ArtifactChunkAccepted {
                    remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                    request_id,
                    payload,
                },
                Err(error) => artifact_error(request_id, error),
            }
        }
        ArtifactTransferOperation::Complete(ArtifactIdPayload { artifact_id }) => {
            match artifacts.complete_upload(&principal.client_id, &artifact_id) {
                Ok(payload) => RemoteResponse::ArtifactUploaded {
                    remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                    request_id,
                    payload,
                },
                Err(error) => artifact_error(request_id, error),
            }
        }
        ArtifactTransferOperation::Abort(ArtifactIdPayload { artifact_id }) => {
            match artifacts.abort_upload(&principal.client_id, &artifact_id) {
                Ok(()) => RemoteResponse::ArtifactUploadAborted {
                    remote_protocol_version: REMOTE_PROTOCOL_VERSION,
                    request_id,
                    payload: ArtifactIdPayload { artifact_id },
                },
                Err(error) => artifact_error(request_id, error),
            }
        }
        ArtifactTransferOperation::Read(payload) => match artifacts.read_output_chunk(
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
        },
    }
}

pub(super) fn artifact_error(request_id: String, error: RemoteArtifactError) -> RemoteResponse {
    let (code, retryable) = error.wire_code();
    let message = match code {
        RemoteErrorCode::InternalError => "internal Artifact store error".to_owned(),
        _ => error.to_string(),
    };
    error_response(request_id, code, message, retryable)
}
