use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};
use taskcaged::remote_artifact::{RemoteArtifactError, RemoteArtifactStore};
use taskcaged::remote_config::PrincipalPolicy;
use taskcaged::remote_protocol::{
    ArtifactUploadState, BeginArtifactUploadPayload, UploadArtifactChunkPayload,
};

const CLIENT_ARTIFACT_ID: &str = "44444444-4444-4444-8444-444444444444";
const TASK_ID: &str = "55555555-5555-4555-8555-555555555555";

fn policy(max_bytes: u64, max_count: usize) -> PrincipalPolicy {
    PrincipalPolicy {
        client_id: "document-worker".to_owned(),
        secret_verifier: "$argon2id$v=19$m=19456,t=2,p=1$c2FsdC1mb3ItdGVzdA$QWERTY".to_owned(),
        allowed_profiles: BTreeSet::new(),
        allow_all_installed_capsules: false,
        maximum_resource_overrides: None,
        artifact_upload_allowed: true,
        max_principal_artifact_bytes: NonZeroU64::new(max_bytes).expect("positive"),
        max_principal_artifacts: NonZeroUsize::new(max_count).expect("positive"),
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
        .expect("Remote Artifact store")
}

fn descriptor(bytes: &[u8]) -> BeginArtifactUploadPayload {
    descriptor_with_client_id(bytes, CLIENT_ARTIFACT_ID)
}

fn descriptor_with_client_id(bytes: &[u8], client_artifact_id: &str) -> BeginArtifactUploadPayload {
    BeginArtifactUploadPayload {
        client_artifact_id: client_artifact_id.to_owned(),
        digest: format!("sha256:{:x}", Sha256::digest(bytes)),
        size_bytes: bytes.len() as u64,
        media_type: Some("application/octet-stream".to_owned()),
    }
}

fn upload(store: &RemoteArtifactStore, bytes: &[u8]) -> String {
    upload_with_client_id(store, bytes, CLIENT_ARTIFACT_ID)
}

fn upload_with_client_id(
    store: &RemoteArtifactStore,
    bytes: &[u8],
    client_artifact_id: &str,
) -> String {
    let started = store
        .begin_upload(
            &policy(1_000_000, 4),
            descriptor_with_client_id(bytes, client_artifact_id),
        )
        .expect("begin upload");
    store
        .upload_chunk(
            "document-worker",
            UploadArtifactChunkPayload {
                artifact_id: started.artifact_id.clone(),
                offset: 0,
                data_base64: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                ),
            },
        )
        .expect("upload chunk");
    started.artifact_id
}

#[test]
fn upload_lifecycle_is_resumable_idempotent_and_single_use() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let bytes = b"TaskCage managed input";
    let artifact_id = upload(&store, bytes);

    let retry = store
        .upload_chunk(
            "document-worker",
            UploadArtifactChunkPayload {
                artifact_id: artifact_id.clone(),
                offset: 0,
                data_base64: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                ),
            },
        )
        .expect("identical chunk retry");
    assert_eq!(retry.next_offset, bytes.len() as u64);
    let resumed = store
        .begin_upload(&policy(1_000_000, 4), descriptor(bytes))
        .expect("resume upload");
    assert_eq!(resumed.artifact_id, artifact_id);
    assert_eq!(resumed.state, ArtifactUploadState::Uploading);
    assert_eq!(resumed.next_offset, bytes.len() as u64);

    let completed = store
        .complete_upload("document-worker", &artifact_id)
        .expect("complete upload");
    let repeated = store
        .complete_upload("document-worker", &artifact_id)
        .expect("idempotent complete");
    assert_eq!(repeated, completed);

    let snapshots = store
        .transfer_inputs(
            "document-worker",
            TASK_ID,
            std::slice::from_ref(&artifact_id),
        )
        .expect("transfer input ownership");
    assert_eq!(snapshots[0].artifact_id, artifact_id);
    assert_eq!(
        std::fs::read(&snapshots[0].path).expect("snapshot bytes"),
        bytes
    );
    assert!(matches!(
        store.abort_upload("document-worker", &artifact_id),
        Err(RemoteArtifactError::InUse)
    ));
    store
        .cleanup_task_inputs(TASK_ID)
        .expect("cleanup task inputs");
    assert!(matches!(
        store.abort_upload("document-worker", &artifact_id),
        Err(RemoteArtifactError::NotFound)
    ));

    let reused = store
        .begin_upload(&policy(1_000_000, 4), descriptor(bytes))
        .expect("reuse clientArtifactId after task ownership transfer");
    assert_ne!(reused.artifact_id, artifact_id);
}

#[test]
fn descriptor_quota_and_digest_failures_have_no_retained_side_effect() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let bytes = b"abcd";
    assert!(matches!(
        store.begin_upload(&policy(3, 1), descriptor(bytes)),
        Err(RemoteArtifactError::LimitExceeded)
    ));

    let mut wrong = descriptor(bytes);
    wrong.digest = format!("sha256:{}", "a".repeat(64));
    let started = store
        .begin_upload(&policy(10, 1), wrong)
        .expect("begin wrong digest upload");
    store
        .upload_chunk(
            "document-worker",
            UploadArtifactChunkPayload {
                artifact_id: started.artifact_id.clone(),
                offset: 0,
                data_base64: "YWJjZA==".to_owned(),
            },
        )
        .expect("upload bytes");
    assert!(matches!(
        store.complete_upload("document-worker", &started.artifact_id),
        Err(RemoteArtifactError::DigestMismatch)
    ));
    let fresh = store
        .begin_upload(&policy(10, 1), descriptor(bytes))
        .expect("mismatch released quota and key");
    assert_ne!(fresh.artifact_id, started.artifact_id);
}

#[test]
fn begin_upload_resolves_existing_key_before_current_policy() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let bytes = b"lost response recovery";
    let original = store
        .begin_upload(&policy(1_000_000, 4), descriptor(bytes))
        .expect("initial upload");

    let mut reduced = policy(1, 1);
    reduced.artifact_upload_allowed = false;
    let recovered = store
        .begin_upload(&reduced, descriptor(bytes))
        .expect("existing upload must survive policy reduction");
    assert_eq!(recovered, original);

    let mut changed = descriptor(bytes);
    changed.size_bytes += 1;
    assert!(matches!(
        store.begin_upload(&reduced, changed),
        Err(RemoteArtifactError::IdempotencyConflict)
    ));

    let missing = descriptor_with_client_id(
        b"larger than the reduced policy",
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    );
    assert!(matches!(
        store.begin_upload(&reduced, missing),
        Err(RemoteArtifactError::AuthorizationDenied)
    ));
}

#[test]
fn managed_output_is_published_only_after_verification_and_downloaded_in_chunks() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = store(temporary.path());
    let source = temporary.path().join("source.bin");
    let bytes = b"verified managed output";
    std::fs::write(&source, bytes).expect("source output");
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    let output = store
        .publish_output(
            "document-worker",
            &source,
            &digest,
            bytes.len() as u64,
            "application/octet-stream",
        )
        .expect("publish output");
    let first = store
        .read_output_chunk("document-worker", &output.artifact_id, 0, 4)
        .expect("first chunk");
    assert_eq!(first.next_offset, 4);
    assert!(!first.finished);
    let remaining = store
        .read_output_chunk(
            "document-worker",
            &output.artifact_id,
            first.next_offset,
            780_000,
        )
        .expect("remaining chunk");
    assert!(remaining.finished);
    assert_eq!(remaining.next_offset, bytes.len() as u64);
    assert!(matches!(
        store.read_output_chunk("other-principal", &output.artifact_id, 0, 4),
        Err(RemoteArtifactError::NotFound)
    ));
}

#[test]
fn restart_discards_incomplete_bytes_but_restores_retained_completed_artifacts() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let bytes = b"retained across daemon restart";
    let source = temporary.path().join("source.bin");
    std::fs::write(&source, bytes).expect("source output");
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));

    let store_before = store(temporary.path());
    let incomplete = upload(&store_before, b"incomplete upload");
    let completed_client_id = "99999999-9999-4999-8999-999999999999";
    let completed_id = upload_with_client_id(&store_before, bytes, completed_client_id);
    let completed = store_before
        .complete_upload("document-worker", &completed_id)
        .expect("complete retained input");
    let output = store_before
        .publish_output(
            "document-worker",
            &source,
            &digest,
            bytes.len() as u64,
            "application/octet-stream",
        )
        .expect("publish retained output");
    drop(store_before);

    let store_after = store(temporary.path());
    assert!(matches!(
        store_after.upload_chunk(
            "document-worker",
            UploadArtifactChunkPayload {
                artifact_id: incomplete,
                offset: 0,
                data_base64: "YQ==".to_owned(),
            },
        ),
        Err(RemoteArtifactError::NotFound)
    ));
    let fresh_incomplete = store_after
        .begin_upload(&policy(1_000_000, 4), descriptor(b"incomplete upload"))
        .expect("incomplete upload key is discarded on restart");
    assert_ne!(fresh_incomplete.artifact_id, completed.artifact_id);
    let resumed = store_after
        .begin_upload(
            &policy(1_000_000, 4),
            descriptor_with_client_id(bytes, completed_client_id),
        )
        .expect("completed input idempotency survives restart");
    assert_eq!(resumed.artifact_id, completed.artifact_id);
    assert_eq!(resumed.state, ArtifactUploadState::Uploaded);
    assert_eq!(resumed.next_offset, bytes.len() as u64);

    let downloaded = store_after
        .read_output_chunk("document-worker", &output.artifact_id, 0, 780_000)
        .expect("retained output after restart");
    assert!(downloaded.finished);
    assert_eq!(
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            downloaded.data_base64,
        )
        .expect("output base64"),
        bytes
    );
}
