//! Remote managed Artifact 전송과 소유권을 담당하는 outbound filesystem adapter다.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::remote_config::PrincipalPolicy;
use crate::remote_protocol::{
    ArtifactChunkAcceptedPayload, ArtifactChunkPayload, ArtifactUploadStartedPayload,
    ArtifactUploadState, ArtifactUploadedPayload, BeginArtifactUploadPayload,
    ManagedOutputArtifactPayload, ManagedOutputKind, RemoteErrorCode, UploadArtifactChunkPayload,
    is_uuid,
};

static ARTIFACT_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct RemoteArtifactStore {
    staging: PathBuf,
    completed: PathBuf,
    task_inputs: PathBuf,
    outputs: PathBuf,
    records: PathBuf,
    max_artifact_bytes: u64,
    max_chunk_bytes: u32,
    retention: Duration,
    state: Arc<Mutex<ArtifactState>>,
}

impl RemoteArtifactStore {
    pub fn open(
        root: &Path,
        max_artifact_bytes: u64,
        max_chunk_bytes: u32,
        retention: Duration,
    ) -> Result<Self, RemoteArtifactError> {
        if !root.is_absolute() || root.to_str().is_none() {
            return Err(RemoteArtifactError::UnsafeRoot(
                "artifact root는 UTF-8 절대 경로여야 합니다".to_owned(),
            ));
        }
        if max_artifact_bytes == 0 || max_chunk_bytes == 0 || retention.is_zero() {
            return Err(RemoteArtifactError::UnsafeRoot(
                "Artifact 제한과 retention은 0보다 커야 합니다".to_owned(),
            ));
        }
        validate_root(root)?;
        let staging = reset_directory(root, "staging")?;
        let completed = prepare_directory(root, "completed-inputs")?;
        let task_inputs = reset_directory(root, "task-inputs")?;
        let outputs = prepare_directory(root, "outputs")?;
        let records = prepare_directory(root, "records")?;
        let state = load_persisted_records(&records, &completed, &outputs)?;
        Ok(Self {
            staging,
            completed,
            task_inputs,
            outputs,
            records,
            max_artifact_bytes,
            max_chunk_bytes,
            retention,
            state: Arc::new(Mutex::new(state)),
        })
    }

    pub fn begin_upload(
        &self,
        principal: &PrincipalPolicy,
        descriptor: BeginArtifactUploadPayload,
    ) -> Result<ArtifactUploadStartedPayload, RemoteArtifactError> {
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        let key = (
            principal.client_id.clone(),
            descriptor.client_artifact_id.clone(),
        );
        if let Some(artifact_id) = state.upload_keys.get(&key)
            && let Some(record) = state.artifacts.get(artifact_id)
        {
            if record.descriptor.as_ref() != Some(&descriptor) {
                return Err(RemoteArtifactError::IdempotencyConflict);
            }
            return record.upload_started(artifact_id);
        }
        if !principal.artifact_upload_allowed {
            return Err(RemoteArtifactError::AuthorizationDenied);
        }
        validate_descriptor(&descriptor, self.max_artifact_bytes, principal)?;
        let quota = state.quotas.entry(principal.client_id.clone()).or_default();
        let next_count = quota
            .count
            .checked_add(1)
            .ok_or(RemoteArtifactError::QuotaExhausted)?;
        let next_bytes = quota
            .bytes
            .checked_add(descriptor.size_bytes)
            .ok_or(RemoteArtifactError::QuotaExhausted)?;
        if next_count > principal.max_principal_artifacts.get()
            || next_bytes > principal.max_principal_artifact_bytes.get()
        {
            return Err(RemoteArtifactError::QuotaExhausted);
        }

        let artifact_id = new_uuid();
        let path = self.staging.join(&artifact_id);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| RemoteArtifactError::Io {
                operation: "create upload staging",
                path: path.clone(),
                source,
            })?;
        quota.count = next_count;
        quota.bytes = next_bytes;
        state.upload_keys.insert(key, artifact_id.clone());
        state.artifacts.insert(
            artifact_id.clone(),
            ArtifactRecord {
                principal: principal.client_id.clone(),
                descriptor: Some(descriptor),
                state: StoredArtifactState::Uploading {
                    path,
                    next_offset: 0,
                },
            },
        );
        Ok(ArtifactUploadStartedPayload {
            artifact_id,
            state: ArtifactUploadState::Uploading,
            next_offset: 0,
        })
    }

    pub fn upload_chunk(
        &self,
        principal: &str,
        payload: UploadArtifactChunkPayload,
    ) -> Result<ArtifactChunkAcceptedPayload, RemoteArtifactError> {
        let bytes = STANDARD
            .decode(payload.data_base64.as_bytes())
            .map_err(|_| RemoteArtifactError::InvalidUpload("chunk base64가 잘못되었습니다"))?;
        if bytes.is_empty() || bytes.len() > self.max_chunk_bytes as usize {
            return Err(RemoteArtifactError::InvalidUpload(
                "chunk 크기가 허용 범위를 벗어났습니다",
            ));
        }
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        let record = state
            .artifacts
            .get_mut(&payload.artifact_id)
            .filter(|record| record.principal == principal)
            .ok_or(RemoteArtifactError::NotFound)?;
        let descriptor = record
            .descriptor
            .as_ref()
            .ok_or(RemoteArtifactError::NotFound)?;
        let StoredArtifactState::Uploading { path, next_offset } = &mut record.state else {
            return Err(RemoteArtifactError::InvalidUpload(
                "complete 뒤에는 chunk를 쓸 수 없습니다",
            ));
        };
        let chunk_size = u64::try_from(bytes.len())
            .map_err(|_| RemoteArtifactError::InvalidUpload("chunk가 너무 큽니다"))?;
        let end =
            payload
                .offset
                .checked_add(chunk_size)
                .ok_or(RemoteArtifactError::InvalidUpload(
                    "chunk offset이 너무 큽니다",
                ))?;
        if end > descriptor.size_bytes || payload.offset > *next_offset {
            return Err(RemoteArtifactError::InvalidUpload(
                "chunk offset 또는 크기가 descriptor와 맞지 않습니다",
            ));
        }
        if payload.offset < *next_offset {
            if end > *next_offset || !file_range_matches(path, payload.offset, &bytes)? {
                return Err(RemoteArtifactError::InvalidUpload(
                    "재시도 chunk가 저장된 bytes와 다릅니다",
                ));
            }
            return Ok(ArtifactChunkAcceptedPayload {
                artifact_id: payload.artifact_id,
                next_offset: *next_offset,
            });
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(path.as_path())
            .map_err(|source| RemoteArtifactError::Io {
                operation: "open upload staging",
                path: path.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.flush())
            .map_err(|source| RemoteArtifactError::Io {
                operation: "append upload chunk",
                path: path.clone(),
                source,
            })?;
        *next_offset = end;
        Ok(ArtifactChunkAcceptedPayload {
            artifact_id: payload.artifact_id,
            next_offset: end,
        })
    }

    pub fn complete_upload(
        &self,
        principal: &str,
        artifact_id: &str,
    ) -> Result<ArtifactUploadedPayload, RemoteArtifactError> {
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        let record = state
            .artifacts
            .get(artifact_id)
            .filter(|record| record.principal == principal)
            .ok_or(RemoteArtifactError::NotFound)?;
        if let StoredArtifactState::Completed { uploaded, .. } = &record.state {
            return Ok(uploaded.clone());
        }
        let descriptor = record
            .descriptor
            .clone()
            .ok_or(RemoteArtifactError::NotFound)?;
        let (path, next_offset) = match &record.state {
            StoredArtifactState::Uploading { path, next_offset } => (path.clone(), *next_offset),
            StoredArtifactState::TaskOwned { .. } => return Err(RemoteArtifactError::InUse),
            StoredArtifactState::Output { .. } | StoredArtifactState::Completed { .. } => {
                return Err(RemoteArtifactError::NotFound);
            }
        };
        if next_offset != descriptor.size_bytes {
            return Err(RemoteArtifactError::InvalidUpload(
                "declared size만큼 upload되지 않았습니다",
            ));
        }
        let actual = digest_file(&path)?;
        if actual != descriptor.digest {
            self.remove_upload_locked(&mut state, artifact_id)?;
            return Err(RemoteArtifactError::DigestMismatch);
        }
        let completed_path = self.completed.join(artifact_id);
        fs::rename(&path, &completed_path).map_err(|source| RemoteArtifactError::Io {
            operation: "publish completed input",
            path: completed_path.clone(),
            source,
        })?;
        let expires = SystemTime::now()
            .checked_add(self.retention)
            .ok_or(RemoteArtifactError::Clock)?;
        let uploaded = ArtifactUploadedPayload {
            artifact_id: artifact_id.to_owned(),
            digest: descriptor.digest.clone(),
            size_bytes: descriptor.size_bytes,
            expires_at: format_timestamp(expires)?,
        };
        let persisted = PersistedArtifactRecord::CompletedInput {
            principal: record.principal.clone(),
            descriptor: descriptor.clone(),
            expires_at_epoch_millis: system_time_to_epoch_millis(expires)?,
            uploaded: uploaded.clone(),
        };
        if let Err(error) = write_persisted_record(&self.records, artifact_id, &persisted) {
            let _ = fs::rename(&completed_path, &path);
            return Err(error);
        }
        state
            .artifacts
            .get_mut(artifact_id)
            .expect("artifact remains reserved")
            .state = StoredArtifactState::Completed {
            path: completed_path,
            expires,
            uploaded: uploaded.clone(),
        };
        Ok(uploaded)
    }

    pub fn abort_upload(
        &self,
        principal: &str,
        artifact_id: &str,
    ) -> Result<(), RemoteArtifactError> {
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        let record = state
            .artifacts
            .get(artifact_id)
            .filter(|record| record.principal == principal)
            .ok_or(RemoteArtifactError::NotFound)?;
        match record.state {
            StoredArtifactState::TaskOwned { .. } => return Err(RemoteArtifactError::InUse),
            StoredArtifactState::Output { .. } => return Err(RemoteArtifactError::NotFound),
            StoredArtifactState::Uploading { .. } | StoredArtifactState::Completed { .. } => {}
        }
        self.remove_upload_locked(&mut state, artifact_id)
    }

    pub fn transfer_inputs(
        &self,
        principal: &str,
        task_id: &str,
        artifact_ids: &[String],
    ) -> Result<Vec<ManagedInputSnapshot>, RemoteArtifactError> {
        if !is_uuid(task_id) {
            return Err(RemoteArtifactError::InvalidUpload(
                "taskId가 UUID가 아닙니다",
            ));
        }
        let unique = artifact_ids.iter().collect::<BTreeSet<_>>();
        if unique.len() != artifact_ids.len() {
            return Err(RemoteArtifactError::InvalidUpload(
                "같은 input Artifact를 두 번 참조할 수 없습니다",
            ));
        }
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        let mut moves = Vec::with_capacity(artifact_ids.len());
        for artifact_id in artifact_ids {
            let record = state
                .artifacts
                .get(artifact_id)
                .filter(|record| record.principal == principal)
                .ok_or(RemoteArtifactError::NotFound)?;
            let descriptor = record
                .descriptor
                .as_ref()
                .ok_or(RemoteArtifactError::NotFound)?;
            match &record.state {
                StoredArtifactState::Completed {
                    path,
                    expires,
                    uploaded,
                } => moves.push((
                    artifact_id.clone(),
                    path.clone(),
                    self.task_inputs.join(format!("{task_id}-{artifact_id}")),
                    descriptor.clone(),
                    *expires,
                    uploaded.clone(),
                    record.persisted()?,
                )),
                StoredArtifactState::TaskOwned { .. } => {
                    return Err(RemoteArtifactError::InUse);
                }
                StoredArtifactState::Uploading { .. } | StoredArtifactState::Output { .. } => {
                    return Err(RemoteArtifactError::NotFound);
                }
            }
        }
        let mut moved = Vec::new();
        for (_, source, destination, _, _, _, _) in &moves {
            if let Err(source_error) = fs::rename(source, destination) {
                for (original, published) in moved.into_iter().rev() {
                    let _ = fs::rename(published, original);
                }
                return Err(RemoteArtifactError::Io {
                    operation: "transfer task input ownership",
                    path: destination.clone(),
                    source: source_error,
                });
            }
            moved.push((source.clone(), destination.clone()));
        }
        let mut removed_records: Vec<(String, PersistedArtifactRecord)> = Vec::new();
        for (artifact_id, _, _, _, _, _, persisted) in &moves {
            if let Err(error) = remove_persisted_record(&self.records, artifact_id) {
                for (restored_id, restored) in &removed_records {
                    let _ = write_persisted_record(&self.records, restored_id, restored);
                }
                for (original, published) in moved.into_iter().rev() {
                    let _ = fs::rename(published, original);
                }
                return Err(error);
            }
            removed_records.push((artifact_id.clone(), persisted.clone()));
        }
        let mut snapshots = Vec::with_capacity(moves.len());
        for (artifact_id, _, path, descriptor, expires, uploaded, _) in moves {
            let key = (principal.to_owned(), descriptor.client_artifact_id.clone());
            state.upload_keys.remove(&key);
            release_quota(&mut state, principal, descriptor.size_bytes);
            state
                .artifacts
                .get_mut(&artifact_id)
                .expect("validated artifact remains")
                .state = StoredArtifactState::TaskOwned {
                path: path.clone(),
                task_id: task_id.to_owned(),
                expires,
                uploaded,
            };
            snapshots.push(ManagedInputSnapshot {
                artifact_id,
                path,
                digest: descriptor.digest,
                size_bytes: descriptor.size_bytes,
                media_type: descriptor.media_type,
            });
        }
        Ok(snapshots)
    }

    pub fn inspect_completed(
        &self,
        principal: &str,
        artifact_ids: &[String],
    ) -> Result<Vec<ManagedInputSnapshot>, RemoteArtifactError> {
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        artifact_ids
            .iter()
            .map(|artifact_id| {
                let record = state
                    .artifacts
                    .get(artifact_id)
                    .filter(|record| record.principal == principal)
                    .ok_or(RemoteArtifactError::NotFound)?;
                let descriptor = record
                    .descriptor
                    .as_ref()
                    .ok_or(RemoteArtifactError::NotFound)?;
                match &record.state {
                    StoredArtifactState::Completed { path, .. } => Ok(ManagedInputSnapshot {
                        artifact_id: artifact_id.clone(),
                        path: path.clone(),
                        digest: descriptor.digest.clone(),
                        size_bytes: descriptor.size_bytes,
                        media_type: descriptor.media_type.clone(),
                    }),
                    StoredArtifactState::TaskOwned { .. } => Err(RemoteArtifactError::InUse),
                    StoredArtifactState::Uploading { .. } | StoredArtifactState::Output { .. } => {
                        Err(RemoteArtifactError::NotFound)
                    }
                }
            })
            .collect()
    }

    pub fn restore_task_inputs(&self, task_id: &str) -> Result<(), RemoteArtifactError> {
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        let ids = state
            .artifacts
            .iter()
            .filter_map(|(artifact_id, record)| match &record.state {
                StoredArtifactState::TaskOwned { task_id: owner, .. } if owner == task_id => {
                    Some(artifact_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for artifact_id in ids {
            let record = state
                .artifacts
                .get(&artifact_id)
                .expect("task input remains");
            let descriptor = record.descriptor.clone().expect("task input descriptor");
            let principal = record.principal.clone();
            let (path, expires, uploaded) = match &record.state {
                StoredArtifactState::TaskOwned {
                    path,
                    expires,
                    uploaded,
                    ..
                } => (path.clone(), *expires, uploaded.clone()),
                _ => unreachable!("selected task input state"),
            };
            let completed_path = self.completed.join(&artifact_id);
            fs::rename(&path, &completed_path).map_err(|source| RemoteArtifactError::Io {
                operation: "restore completed input",
                path: completed_path.clone(),
                source,
            })?;
            let persisted = PersistedArtifactRecord::CompletedInput {
                principal: principal.clone(),
                descriptor: descriptor.clone(),
                expires_at_epoch_millis: system_time_to_epoch_millis(expires)?,
                uploaded: uploaded.clone(),
            };
            if let Err(error) = write_persisted_record(&self.records, &artifact_id, &persisted) {
                let _ = fs::rename(&completed_path, &path);
                return Err(error);
            }
            let quota = state.quotas.entry(principal.clone()).or_default();
            quota.count = quota
                .count
                .checked_add(1)
                .ok_or(RemoteArtifactError::QuotaExhausted)?;
            quota.bytes = quota
                .bytes
                .checked_add(descriptor.size_bytes)
                .ok_or(RemoteArtifactError::QuotaExhausted)?;
            state.upload_keys.insert(
                (principal, descriptor.client_artifact_id.clone()),
                artifact_id.clone(),
            );
            state
                .artifacts
                .get_mut(&artifact_id)
                .expect("task input remains")
                .state = StoredArtifactState::Completed {
                path: completed_path,
                expires,
                uploaded,
            };
        }
        Ok(())
    }

    pub fn cleanup_task_inputs(&self, task_id: &str) -> Result<(), RemoteArtifactError> {
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        let ids = state
            .artifacts
            .iter()
            .filter_map(|(artifact_id, record)| match &record.state {
                StoredArtifactState::TaskOwned { task_id: owner, .. } if owner == task_id => {
                    Some(artifact_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for artifact_id in ids {
            let record = state
                .artifacts
                .remove(&artifact_id)
                .expect("task input remains");
            if let StoredArtifactState::TaskOwned { path, .. } = record.state {
                remove_file_if_present(&path)?;
            }
        }
        Ok(())
    }

    pub fn publish_output(
        &self,
        principal: &str,
        source: &Path,
        digest: &str,
        size_bytes: u64,
        media_type: &str,
    ) -> Result<ManagedOutputArtifactPayload, RemoteArtifactError> {
        let source_file = File::open(source).map_err(|source_error| RemoteArtifactError::Io {
            operation: "open managed output source",
            path: source.to_path_buf(),
            source: source_error,
        })?;
        self.publish_output_file(principal, source_file, digest, size_bytes, media_type)
    }

    pub(crate) fn publish_output_file(
        &self,
        principal: &str,
        mut source: File,
        digest: &str,
        size_bytes: u64,
        media_type: &str,
    ) -> Result<ManagedOutputArtifactPayload, RemoteArtifactError> {
        if !valid_digest(digest) || size_bytes == 0 || media_type.is_empty() {
            return Err(RemoteArtifactError::InvalidUpload(
                "output descriptor가 잘못되었습니다",
            ));
        }
        let artifact_id = new_uuid();
        let temporary = self.outputs.join(format!(".{artifact_id}.tmp"));
        let published = self.outputs.join(&artifact_id);
        let copy_result = (|| {
            let mut destination = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|source_error| RemoteArtifactError::Io {
                    operation: "create managed output staging",
                    path: temporary.clone(),
                    source: source_error,
                })?;
            std::io::copy(&mut source, &mut destination)
                .and_then(|_| destination.sync_all())
                .map_err(|source_error| RemoteArtifactError::Io {
                    operation: "copy managed output",
                    path: temporary.clone(),
                    source: source_error,
                })
        })();
        if let Err(error) = copy_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let metadata =
            fs::metadata(&temporary).map_err(|source_error| RemoteArtifactError::Io {
                operation: "stat managed output",
                path: temporary.clone(),
                source: source_error,
            })?;
        let actual_digest = digest_file(&temporary)?;
        if metadata.len() != size_bytes || actual_digest != digest {
            let _ = fs::remove_file(&temporary);
            return Err(RemoteArtifactError::DigestMismatch);
        }
        fs::rename(&temporary, &published).map_err(|source_error| RemoteArtifactError::Io {
            operation: "publish managed output",
            path: published.clone(),
            source: source_error,
        })?;
        let expires = SystemTime::now()
            .checked_add(self.retention)
            .ok_or(RemoteArtifactError::Clock)?;
        let payload = ManagedOutputArtifactPayload {
            kind: ManagedOutputKind::ManagedOutput,
            artifact_id: artifact_id.clone(),
            digest: digest.to_owned(),
            size_bytes,
            media_type: media_type.to_owned(),
            expires_at: format_timestamp(expires)?,
        };
        let persisted = PersistedArtifactRecord::ManagedOutput {
            principal: principal.to_owned(),
            expires_at_epoch_millis: system_time_to_epoch_millis(expires)?,
            payload: payload.clone(),
        };
        if let Err(error) = write_persisted_record(&self.records, &artifact_id, &persisted) {
            let _ = fs::remove_file(&published);
            return Err(error);
        }
        self.state
            .lock()
            .expect("remote artifact state poisoned")
            .artifacts
            .insert(
                artifact_id,
                ArtifactRecord {
                    principal: principal.to_owned(),
                    descriptor: None,
                    state: StoredArtifactState::Output {
                        path: published,
                        expires,
                        payload: payload.clone(),
                    },
                },
            );
        Ok(payload)
    }

    pub fn read_output_chunk(
        &self,
        principal: &str,
        artifact_id: &str,
        offset: u64,
        max_bytes: u32,
    ) -> Result<ArtifactChunkPayload, RemoteArtifactError> {
        if max_bytes == 0 || max_bytes > self.max_chunk_bytes {
            return Err(RemoteArtifactError::InvalidUpload(
                "maxBytes가 허용 범위를 벗어났습니다",
            ));
        }
        let mut state = self.state.lock().expect("remote artifact state poisoned");
        self.purge_expired_locked(&mut state)?;
        let record = state
            .artifacts
            .get(artifact_id)
            .filter(|record| record.principal == principal)
            .ok_or(RemoteArtifactError::NotFound)?;
        let (path, size_bytes) = match &record.state {
            StoredArtifactState::Output { path, payload, .. } => (path, payload.size_bytes),
            _ => return Err(RemoteArtifactError::NotFound),
        };
        if offset > size_bytes {
            return Err(RemoteArtifactError::InvalidUpload(
                "output offset이 Artifact 크기를 초과합니다",
            ));
        }
        let remaining = size_bytes - offset;
        let read_size = remaining.min(u64::from(max_bytes)) as usize;
        let mut bytes = vec![0_u8; read_size];
        let mut file = File::open(path).map_err(|source| RemoteArtifactError::Io {
            operation: "open managed output",
            path: path.clone(),
            source,
        })?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut bytes))
            .map_err(|source| RemoteArtifactError::Io {
                operation: "read managed output",
                path: path.clone(),
                source,
            })?;
        let next_offset = offset + read_size as u64;
        Ok(ArtifactChunkPayload {
            artifact_id: artifact_id.to_owned(),
            offset,
            data_base64: STANDARD.encode(bytes),
            next_offset,
            finished: next_offset == size_bytes,
        })
    }

    fn purge_expired_locked(&self, state: &mut ArtifactState) -> Result<(), RemoteArtifactError> {
        let now = SystemTime::now();
        let expired = state
            .artifacts
            .iter()
            .filter_map(|(artifact_id, record)| match &record.state {
                StoredArtifactState::Completed { expires, .. }
                | StoredArtifactState::Output { expires, .. }
                    if *expires <= now =>
                {
                    Some(artifact_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for artifact_id in expired {
            let record = state
                .artifacts
                .get(&artifact_id)
                .expect("expired artifact remains");
            let persisted = record.persisted()?;
            remove_persisted_record(&self.records, &artifact_id)?;
            if let Err(error) = remove_file_if_present(record.path()) {
                let _ = write_persisted_record(&self.records, &artifact_id, &persisted);
                return Err(error);
            }
            let record = state
                .artifacts
                .remove(&artifact_id)
                .expect("expired artifact remains");
            if let Some(descriptor) = record.descriptor {
                state
                    .upload_keys
                    .remove(&(record.principal.clone(), descriptor.client_artifact_id));
                release_quota(state, &record.principal, descriptor.size_bytes);
            }
        }
        Ok(())
    }

    fn remove_upload_locked(
        &self,
        state: &mut ArtifactState,
        artifact_id: &str,
    ) -> Result<(), RemoteArtifactError> {
        let record = state
            .artifacts
            .get(artifact_id)
            .expect("validated artifact remains");
        let persisted = matches!(&record.state, StoredArtifactState::Completed { .. })
            .then(|| record.persisted())
            .transpose()?;
        if persisted.is_some() {
            remove_persisted_record(&self.records, artifact_id)?;
        }
        if let Err(error) = remove_file_if_present(record.path()) {
            if let Some(persisted) = persisted {
                let _ = write_persisted_record(&self.records, artifact_id, &persisted);
            }
            return Err(error);
        }
        let record = state
            .artifacts
            .remove(artifact_id)
            .expect("validated artifact remains");
        let descriptor = record
            .descriptor
            .expect("upload artifacts carry a descriptor");
        state
            .upload_keys
            .remove(&(record.principal.clone(), descriptor.client_artifact_id));
        release_quota(state, &record.principal, descriptor.size_bytes);
        Ok(())
    }
}

#[derive(Default)]
struct ArtifactState {
    artifacts: BTreeMap<String, ArtifactRecord>,
    upload_keys: BTreeMap<(String, String), String>,
    quotas: BTreeMap<String, PrincipalQuota>,
}

#[derive(Default)]
struct PrincipalQuota {
    bytes: u64,
    count: usize,
}

#[derive(Clone)]
struct ArtifactRecord {
    principal: String,
    descriptor: Option<BeginArtifactUploadPayload>,
    state: StoredArtifactState,
}

impl ArtifactRecord {
    fn upload_started(
        &self,
        artifact_id: &str,
    ) -> Result<ArtifactUploadStartedPayload, RemoteArtifactError> {
        match &self.state {
            StoredArtifactState::Uploading { next_offset, .. } => {
                Ok(ArtifactUploadStartedPayload {
                    artifact_id: artifact_id.to_owned(),
                    state: ArtifactUploadState::Uploading,
                    next_offset: *next_offset,
                })
            }
            StoredArtifactState::Completed { uploaded, .. } => Ok(ArtifactUploadStartedPayload {
                artifact_id: uploaded.artifact_id.clone(),
                state: ArtifactUploadState::Uploaded,
                next_offset: uploaded.size_bytes,
            }),
            StoredArtifactState::TaskOwned { .. } => Err(RemoteArtifactError::InUse),
            StoredArtifactState::Output { .. } => Err(RemoteArtifactError::NotFound),
        }
    }

    fn path(&self) -> &Path {
        match &self.state {
            StoredArtifactState::Uploading { path, .. }
            | StoredArtifactState::Completed { path, .. }
            | StoredArtifactState::TaskOwned { path, .. }
            | StoredArtifactState::Output { path, .. } => path,
        }
    }

    fn persisted(&self) -> Result<PersistedArtifactRecord, RemoteArtifactError> {
        match &self.state {
            StoredArtifactState::Completed {
                expires, uploaded, ..
            } => Ok(PersistedArtifactRecord::CompletedInput {
                principal: self.principal.clone(),
                descriptor: self
                    .descriptor
                    .clone()
                    .expect("completed input carries a descriptor"),
                expires_at_epoch_millis: system_time_to_epoch_millis(*expires)?,
                uploaded: uploaded.clone(),
            }),
            StoredArtifactState::Output {
                expires, payload, ..
            } => Ok(PersistedArtifactRecord::ManagedOutput {
                principal: self.principal.clone(),
                expires_at_epoch_millis: system_time_to_epoch_millis(*expires)?,
                payload: payload.clone(),
            }),
            StoredArtifactState::Uploading { .. } | StoredArtifactState::TaskOwned { .. } => Err(
                RemoteArtifactError::UnsafeRoot("persist할 수 없는 Artifact 상태입니다".to_owned()),
            ),
        }
    }
}

#[derive(Clone)]
enum StoredArtifactState {
    Uploading {
        path: PathBuf,
        next_offset: u64,
    },
    Completed {
        path: PathBuf,
        expires: SystemTime,
        uploaded: ArtifactUploadedPayload,
    },
    TaskOwned {
        path: PathBuf,
        task_id: String,
        expires: SystemTime,
        uploaded: ArtifactUploadedPayload,
    },
    Output {
        path: PathBuf,
        expires: SystemTime,
        payload: ManagedOutputArtifactPayload,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "recordType",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
enum PersistedArtifactRecord {
    CompletedInput {
        principal: String,
        descriptor: BeginArtifactUploadPayload,
        expires_at_epoch_millis: u64,
        uploaded: ArtifactUploadedPayload,
    },
    ManagedOutput {
        principal: String,
        expires_at_epoch_millis: u64,
        payload: ManagedOutputArtifactPayload,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedInputSnapshot {
    pub artifact_id: String,
    pub path: PathBuf,
    pub digest: String,
    pub size_bytes: u64,
    pub media_type: Option<String>,
}

fn validate_descriptor(
    descriptor: &BeginArtifactUploadPayload,
    max_artifact_bytes: u64,
    principal: &PrincipalPolicy,
) -> Result<(), RemoteArtifactError> {
    if !is_uuid(&descriptor.client_artifact_id)
        || !valid_digest(&descriptor.digest)
        || descriptor.size_bytes == 0
        || descriptor
            .media_type
            .as_ref()
            .is_some_and(|value| value.is_empty())
    {
        return Err(RemoteArtifactError::InvalidUpload(
            "upload descriptor가 잘못되었습니다",
        ));
    }
    if descriptor.size_bytes > max_artifact_bytes
        || descriptor.size_bytes > principal.max_principal_artifact_bytes.get()
    {
        return Err(RemoteArtifactError::LimitExceeded);
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn file_range_matches(
    path: &Path,
    offset: u64,
    expected: &[u8],
) -> Result<bool, RemoteArtifactError> {
    let mut file = File::open(path).map_err(|source| RemoteArtifactError::Io {
        operation: "open upload retry range",
        path: path.to_path_buf(),
        source,
    })?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| RemoteArtifactError::Io {
            operation: "seek upload retry range",
            path: path.to_path_buf(),
            source,
        })?;
    let mut actual = vec![0_u8; expected.len()];
    file.read_exact(&mut actual)
        .map_err(|source| RemoteArtifactError::Io {
            operation: "read upload retry range",
            path: path.to_path_buf(),
            source,
        })?;
    Ok(actual == expected)
}

fn digest_file(path: &Path) -> Result<String, RemoteArtifactError> {
    let mut file = File::open(path).map_err(|source| RemoteArtifactError::Io {
        operation: "open Artifact for digest",
        path: path.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| RemoteArtifactError::Io {
                operation: "read Artifact for digest",
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn release_quota(state: &mut ArtifactState, principal: &str, bytes: u64) {
    if let Some(quota) = state.quotas.get_mut(principal) {
        quota.count = quota.count.saturating_sub(1);
        quota.bytes = quota.bytes.saturating_sub(bytes);
        if quota.count == 0 {
            state.quotas.remove(principal);
        }
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), RemoteArtifactError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RemoteArtifactError::Io {
            operation: "remove Artifact",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn validate_root(root: &Path) -> Result<(), RemoteArtifactError> {
    crate::remote_config::validate_path_ancestors(root, "remote artifact root")
        .map_err(|error| RemoteArtifactError::UnsafeRoot(error.to_string()))?;
    let metadata = fs::symlink_metadata(root).map_err(|source| RemoteArtifactError::Io {
        operation: "inspect artifact root",
        path: root.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RemoteArtifactError::UnsafeRoot(
            "artifact root는 실제 directory여야 합니다".to_owned(),
        ));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(RemoteArtifactError::UnsafeRoot(
                "artifact root는 service UID 소유이며 group/other 권한이 없어야 합니다".to_owned(),
            ));
        }
    }
    Ok(())
}

fn reset_directory(root: &Path, name: &str) -> Result<PathBuf, RemoteArtifactError> {
    let path = root.join(name);
    if path.exists() {
        let metadata = fs::symlink_metadata(&path).map_err(|source| RemoteArtifactError::Io {
            operation: "inspect remote artifact directory",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RemoteArtifactError::UnsafeRoot(format!(
                "remote artifact child가 안전한 directory가 아닙니다: {name}"
            )));
        }
        fs::remove_dir_all(&path).map_err(|source| RemoteArtifactError::Io {
            operation: "discard previous remote artifacts",
            path: path.clone(),
            source,
        })?;
    }
    fs::create_dir(&path).map_err(|source| RemoteArtifactError::Io {
        operation: "create remote artifact directory",
        path: path.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            RemoteArtifactError::Io {
                operation: "protect remote artifact directory",
                path: path.clone(),
                source,
            }
        })?;
    }
    Ok(path)
}

fn prepare_directory(root: &Path, name: &str) -> Result<PathBuf, RemoteArtifactError> {
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RemoteArtifactError::UnsafeRoot(format!(
                    "remote artifact child가 안전한 directory가 아닙니다: {name}"
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path).map_err(|source| RemoteArtifactError::Io {
                operation: "create remote artifact directory",
                path: path.clone(),
                source,
            })?;
        }
        Err(source) => {
            return Err(RemoteArtifactError::Io {
                operation: "inspect remote artifact directory",
                path: path.clone(),
                source,
            });
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            RemoteArtifactError::Io {
                operation: "protect remote artifact directory",
                path: path.clone(),
                source,
            }
        })?;
    }
    Ok(path)
}

fn load_persisted_records(
    records: &Path,
    completed: &Path,
    outputs: &Path,
) -> Result<ArtifactState, RemoteArtifactError> {
    let mut state = ArtifactState::default();
    let mut retained_completed = BTreeSet::new();
    let mut retained_outputs = BTreeSet::new();
    let now = SystemTime::now();

    for entry in fs::read_dir(records).map_err(|source| RemoteArtifactError::Io {
        operation: "read Artifact record directory",
        path: records.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| RemoteArtifactError::Io {
            operation: "read Artifact record entry",
            path: records.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| RemoteArtifactError::Io {
            operation: "inspect Artifact record",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RemoteArtifactError::UnsafeRoot(
                "Artifact record는 실제 regular file이어야 합니다".to_owned(),
            ));
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            return Err(RemoteArtifactError::UnsafeRoot(
                "Artifact record 이름은 UTF-8이어야 합니다".to_owned(),
            ));
        };
        if file_name.starts_with('.') && file_name.ends_with(".tmp") {
            remove_file_if_present(&path)?;
            continue;
        }
        let Some(artifact_id) = file_name.strip_suffix(".json") else {
            return Err(RemoteArtifactError::UnsafeRoot(
                "알 수 없는 Artifact record file이 있습니다".to_owned(),
            ));
        };
        if !is_uuid(artifact_id) {
            return Err(RemoteArtifactError::UnsafeRoot(
                "Artifact record 이름이 UUID가 아닙니다".to_owned(),
            ));
        }
        let bytes = fs::read(&path).map_err(|source| RemoteArtifactError::Io {
            operation: "read Artifact record",
            path: path.clone(),
            source,
        })?;
        let persisted: PersistedArtifactRecord = serde_json::from_slice(&bytes).map_err(|_| {
            RemoteArtifactError::UnsafeRoot("Artifact record JSON이 잘못되었습니다".to_owned())
        })?;
        let (record, expires, data_path) =
            restore_persisted_record(artifact_id, persisted, completed, outputs)?;
        if expires <= now {
            remove_file_if_present(&data_path)?;
            remove_file_if_present(&path)?;
            continue;
        }
        validate_persisted_data(&data_path, &record)?;
        match &record.state {
            StoredArtifactState::Completed { .. } => {
                retained_completed.insert(artifact_id.to_owned());
                let descriptor = record
                    .descriptor
                    .as_ref()
                    .expect("completed record carries descriptor");
                let key = (
                    record.principal.clone(),
                    descriptor.client_artifact_id.clone(),
                );
                if state
                    .upload_keys
                    .insert(key, artifact_id.to_owned())
                    .is_some()
                {
                    return Err(RemoteArtifactError::UnsafeRoot(
                        "중복 Artifact upload idempotency record가 있습니다".to_owned(),
                    ));
                }
                let quota = state.quotas.entry(record.principal.clone()).or_default();
                quota.count = quota.count.checked_add(1).ok_or_else(|| {
                    RemoteArtifactError::UnsafeRoot("Artifact quota가 너무 큽니다".to_owned())
                })?;
                quota.bytes = quota
                    .bytes
                    .checked_add(descriptor.size_bytes)
                    .ok_or_else(|| {
                        RemoteArtifactError::UnsafeRoot("Artifact quota가 너무 큽니다".to_owned())
                    })?;
            }
            StoredArtifactState::Output { .. } => {
                retained_outputs.insert(artifact_id.to_owned());
            }
            StoredArtifactState::Uploading { .. } | StoredArtifactState::TaskOwned { .. } => {
                unreachable!("only completed records are persisted")
            }
        }
        if state
            .artifacts
            .insert(artifact_id.to_owned(), record)
            .is_some()
        {
            return Err(RemoteArtifactError::UnsafeRoot(
                "중복 Artifact record가 있습니다".to_owned(),
            ));
        }
    }

    remove_orphan_artifacts(completed, &retained_completed)?;
    remove_orphan_artifacts(outputs, &retained_outputs)?;
    Ok(state)
}

fn restore_persisted_record(
    artifact_id: &str,
    persisted: PersistedArtifactRecord,
    completed: &Path,
    outputs: &Path,
) -> Result<(ArtifactRecord, SystemTime, PathBuf), RemoteArtifactError> {
    match persisted {
        PersistedArtifactRecord::CompletedInput {
            principal,
            descriptor,
            expires_at_epoch_millis,
            uploaded,
        } => {
            if principal.is_empty()
                || !is_uuid(&descriptor.client_artifact_id)
                || !valid_digest(&descriptor.digest)
                || descriptor.size_bytes == 0
                || uploaded.artifact_id != artifact_id
                || uploaded.digest != descriptor.digest
                || uploaded.size_bytes != descriptor.size_bytes
            {
                return Err(RemoteArtifactError::UnsafeRoot(
                    "completed input record가 잘못되었습니다".to_owned(),
                ));
            }
            let expires = epoch_millis_to_system_time(expires_at_epoch_millis)?;
            let path = completed.join(artifact_id);
            Ok((
                ArtifactRecord {
                    principal,
                    descriptor: Some(descriptor),
                    state: StoredArtifactState::Completed {
                        path: path.clone(),
                        expires,
                        uploaded,
                    },
                },
                expires,
                path,
            ))
        }
        PersistedArtifactRecord::ManagedOutput {
            principal,
            expires_at_epoch_millis,
            payload,
        } => {
            if principal.is_empty()
                || payload.kind != ManagedOutputKind::ManagedOutput
                || payload.artifact_id != artifact_id
                || !valid_digest(&payload.digest)
                || payload.size_bytes == 0
                || payload.media_type.is_empty()
            {
                return Err(RemoteArtifactError::UnsafeRoot(
                    "managed output record가 잘못되었습니다".to_owned(),
                ));
            }
            let expires = epoch_millis_to_system_time(expires_at_epoch_millis)?;
            let path = outputs.join(artifact_id);
            Ok((
                ArtifactRecord {
                    principal,
                    descriptor: None,
                    state: StoredArtifactState::Output {
                        path: path.clone(),
                        expires,
                        payload,
                    },
                },
                expires,
                path,
            ))
        }
    }
}

fn validate_persisted_data(
    path: &Path,
    record: &ArtifactRecord,
) -> Result<(), RemoteArtifactError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| RemoteArtifactError::Io {
        operation: "inspect persisted Artifact",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RemoteArtifactError::UnsafeRoot(
            "persisted Artifact는 실제 regular file이어야 합니다".to_owned(),
        ));
    }
    let (expected_size, expected_digest) = match &record.state {
        StoredArtifactState::Completed { uploaded, .. } => {
            (uploaded.size_bytes, uploaded.digest.as_str())
        }
        StoredArtifactState::Output { payload, .. } => {
            (payload.size_bytes, payload.digest.as_str())
        }
        StoredArtifactState::Uploading { .. } | StoredArtifactState::TaskOwned { .. } => {
            unreachable!("only completed records are persisted")
        }
    };
    if metadata.len() != expected_size || digest_file(path)? != expected_digest {
        return Err(RemoteArtifactError::UnsafeRoot(
            "persisted Artifact bytes가 record와 일치하지 않습니다".to_owned(),
        ));
    }
    Ok(())
}

fn remove_orphan_artifacts(
    directory: &Path,
    retained: &BTreeSet<String>,
) -> Result<(), RemoteArtifactError> {
    for entry in fs::read_dir(directory).map_err(|source| RemoteArtifactError::Io {
        operation: "read Artifact data directory",
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| RemoteArtifactError::Io {
            operation: "read Artifact data entry",
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| RemoteArtifactError::Io {
            operation: "inspect Artifact data entry",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RemoteArtifactError::UnsafeRoot(
                "Artifact data entry는 실제 regular file이어야 합니다".to_owned(),
            ));
        }
        let file_name = path.file_name().and_then(|value| value.to_str());
        if !file_name.is_some_and(|value| retained.contains(value)) {
            remove_file_if_present(&path)?;
        }
    }
    Ok(())
}

fn write_persisted_record(
    records: &Path,
    artifact_id: &str,
    record: &PersistedArtifactRecord,
) -> Result<(), RemoteArtifactError> {
    let target = records.join(format!("{artifact_id}.json"));
    let temporary = records.join(format!(".{artifact_id}.json.tmp"));
    remove_file_if_present(&temporary)?;
    let bytes = serde_json::to_vec(record).map_err(|_| {
        RemoteArtifactError::UnsafeRoot("Artifact record를 encode할 수 없습니다".to_owned())
    })?;
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| RemoteArtifactError::Io {
                operation: "create Artifact record",
                path: temporary.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| RemoteArtifactError::Io {
                operation: "write Artifact record",
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, &target).map_err(|source| RemoteArtifactError::Io {
            operation: "publish Artifact record",
            path: target,
            source,
        })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn remove_persisted_record(records: &Path, artifact_id: &str) -> Result<(), RemoteArtifactError> {
    remove_file_if_present(&records.join(format!("{artifact_id}.json")))
}

fn system_time_to_epoch_millis(value: SystemTime) -> Result<u64, RemoteArtifactError> {
    let millis = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RemoteArtifactError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| RemoteArtifactError::Clock)
}

fn epoch_millis_to_system_time(value: u64) -> Result<SystemTime, RemoteArtifactError> {
    UNIX_EPOCH
        .checked_add(Duration::from_millis(value))
        .ok_or(RemoteArtifactError::Clock)
}

fn new_uuid() -> String {
    let sequence = ARTIFACT_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let process_sequence = (u64::from(std::process::id()) << 32) ^ sequence;
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&time.to_be_bytes());
    bytes[8..].copy_from_slice(&process_sequence.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn format_timestamp(value: SystemTime) -> Result<String, RemoteArtifactError> {
    time::OffsetDateTime::from(value)
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| RemoteArtifactError::Clock)
}

#[derive(Debug, Error)]
pub enum RemoteArtifactError {
    #[error("Remote Artifact root가 안전하지 않습니다: {0}")]
    UnsafeRoot(String),
    #[error("principal은 Artifact upload 권한이 없습니다")]
    AuthorizationDenied,
    #[error("Artifact upload descriptor가 정책을 초과합니다")]
    LimitExceeded,
    #[error("principal Artifact quota가 소진되었습니다")]
    QuotaExhausted,
    #[error("Artifact upload idempotency key가 다른 descriptor에 사용되었습니다")]
    IdempotencyConflict,
    #[error("Artifact upload가 잘못되었습니다: {0}")]
    InvalidUpload(&'static str),
    #[error("Artifact digest가 일치하지 않습니다")]
    DigestMismatch,
    #[error("Artifact를 찾을 수 없습니다")]
    NotFound,
    #[error("Artifact가 accepted Task에서 사용 중입니다")]
    InUse,
    #[error("Artifact 시각을 표현할 수 없습니다")]
    Clock,
    #[error("Artifact filesystem 작업에 실패했습니다: {operation}: {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl RemoteArtifactError {
    pub fn wire_code(&self) -> (RemoteErrorCode, bool) {
        match self {
            Self::AuthorizationDenied => (RemoteErrorCode::AuthorizationDenied, false),
            Self::LimitExceeded => (RemoteErrorCode::ArtifactUploadLimitExceeded, false),
            Self::QuotaExhausted => (RemoteErrorCode::ArtifactUploadQuotaExhausted, true),
            Self::IdempotencyConflict => (RemoteErrorCode::IdempotencyConflict, false),
            Self::InvalidUpload(_) => (RemoteErrorCode::InvalidArtifactUpload, false),
            Self::DigestMismatch => (RemoteErrorCode::ArtifactDigestMismatch, false),
            Self::NotFound => (RemoteErrorCode::ArtifactNotFound, false),
            Self::InUse => (RemoteErrorCode::ArtifactInUse, false),
            Self::UnsafeRoot(_) | Self::Clock | Self::Io { .. } => {
                (RemoteErrorCode::InternalError, true)
            }
        }
    }
}
