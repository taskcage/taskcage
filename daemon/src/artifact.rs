//! Local Product Alpha Artifact descriptor의 공통 검증값이다.
//!
//! 실제 root-relative open, snapshot과 publish는 Profile 실행 경로가 추가될 때 이 값만 소비한다.
//! Raw Command Protocol v1은 이 모듈을 사용하지 않는다.

use std::fmt;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::digest::Sha256Digest;

#[cfg(target_os = "linux")]
use std::cell::Cell;
#[cfg(target_os = "linux")]
use std::ffi::{CString, OsStr};
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::path::{Component, Path, PathBuf};

/// Artifact root 기준 상대 path의 최대 UTF-8 byte 길이다.
pub const MAX_ARTIFACT_PATH_BYTES: usize = 4_096;

/// Artifact root 밖을 가리키지 않는 wire path다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactPath(String);

impl ArtifactPath {
    /// Local Artifact path 문법을 side effect 없이 검증한다.
    pub fn parse(value: impl Into<String>) -> Result<Self, ArtifactPathError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_ARTIFACT_PATH_BYTES {
            return Err(ArtifactPathError::Length);
        }

        for segment in value.split('/') {
            if segment.is_empty() || matches!(segment, "." | "..") {
                return Err(ArtifactPathError::Segment);
            }
            if segment
                .bytes()
                .any(|byte| byte == b'\0' || byte == b'\\' || byte.is_ascii_control())
            {
                return Err(ArtifactPathError::UnsafeCharacter);
            }
        }

        if value.split('/').next() == Some(".taskcage") {
            return Err(ArtifactPathError::ReservedPath);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Input Artifact가 선언하는 immutable snapshot identity다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInputArtifact {
    path: ArtifactPath,
    digest: Sha256Digest,
    size_bytes: u64,
}

impl LocalInputArtifact {
    pub fn new(path: ArtifactPath, digest: Sha256Digest, size_bytes: u64) -> Self {
        Self {
            path,
            digest,
            size_bytes,
        }
    }

    pub fn path(&self) -> &ArtifactPath {
        &self.path
    }

    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

/// Source bytes가 descriptor와 배포 상한에 맞는지 target 시작 전에 검증한다.
pub fn verify_input<R>(
    artifact: &LocalInputArtifact,
    maximum_bytes: u64,
    reader: &mut R,
) -> Result<(), ArtifactVerificationError>
where
    R: Read,
{
    if artifact.size_bytes() > maximum_bytes {
        return Err(ArtifactVerificationError::TooLarge {
            actual: artifact.size_bytes(),
            maximum: maximum_bytes,
        });
    }

    let mut actual_size = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(ArtifactVerificationError::Read)?;
        if read == 0 {
            break;
        }
        actual_size = actual_size
            .checked_add(u64::try_from(read).expect("read length fits in u64"))
            .ok_or(ArtifactVerificationError::SizeOverflow)?;
        if actual_size > maximum_bytes {
            return Err(ArtifactVerificationError::TooLarge {
                actual: actual_size,
                maximum: maximum_bytes,
            });
        }
        hasher.update(&buffer[..read]);
    }

    if actual_size != artifact.size_bytes() {
        return Err(ArtifactVerificationError::SizeMismatch {
            expected: artifact.size_bytes(),
            actual: actual_size,
        });
    }
    let actual = Sha256Digest::from_bytes(hasher.finalize().into());
    if actual != artifact.digest() {
        return Err(ArtifactVerificationError::DigestMismatch);
    }

    Ok(())
}

/// Artifact path가 Local Product Alpha 경계를 벗어났다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactPathError {
    #[error("artifact path는 1~4096 UTF-8 bytes여야 합니다")]
    Length,
    #[error("artifact path에 빈, . 또는 .. segment를 넣을 수 없습니다")]
    Segment,
    #[error("artifact path에 NUL, backslash 또는 ASCII control 문자를 넣을 수 없습니다")]
    UnsafeCharacter,
    #[error(".taskcage staging subtree는 caller Artifact path로 사용할 수 없습니다")]
    ReservedPath,
}

/// Input snapshot 검증이 target 시작 전에 실패했다.
#[derive(Debug, Error)]
pub enum ArtifactVerificationError {
    #[error("artifact가 maximum {maximum} bytes를 넘습니다: {actual} bytes")]
    TooLarge { actual: u64, maximum: u64 },
    #[error("artifact size가 descriptor와 다릅니다: expected={expected}, actual={actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("artifact size를 누적하는 중 overflow가 발생했습니다")]
    SizeOverflow,
    #[error("artifact digest가 descriptor와 다릅니다")]
    DigestMismatch,
    #[error("artifact source를 읽지 못했습니다: {0}")]
    Read(#[source] io::Error),
}

/// Profile이 선언한 하나의 immutable output file 계약이다.
///
/// v0.2 Product Alpha는 여러 output을 하나의 transaction으로 publish하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOutputArtifact {
    file_name: String,
    media_type: String,
    maximum_bytes: u64,
}

impl DeclaredOutputArtifact {
    pub fn new(
        file_name: impl Into<String>,
        media_type: impl Into<String>,
        maximum_bytes: u64,
    ) -> Result<Self, DeclaredOutputError> {
        let file_name = file_name.into();
        if file_name.is_empty()
            || file_name.contains('/')
            || file_name.contains('\\')
            || file_name == "."
            || file_name == ".."
            || file_name
                .bytes()
                .any(|byte| byte == b'\0' || byte.is_ascii_control())
        {
            return Err(DeclaredOutputError::FileName);
        }
        let media_type = media_type.into();
        if media_type.is_empty()
            || media_type.len() > 255
            || media_type
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b'\0')
        {
            return Err(DeclaredOutputError::MediaType);
        }
        if maximum_bytes == 0 {
            return Err(DeclaredOutputError::MaximumBytes);
        }

        Ok(Self {
            file_name,
            media_type,
            maximum_bytes,
        })
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn maximum_bytes(&self) -> u64 {
        self.maximum_bytes
    }
}

/// 고정 output file 선언이 Product Alpha 경계를 벗어났다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeclaredOutputError {
    #[error("output file name은 단일 안전 file name이어야 합니다")]
    FileName,
    #[error("output media type은 비어 있지 않은 control-character 없는 문자열이어야 합니다")]
    MediaType,
    #[error("output maximum bytes는 0보다 커야 합니다")]
    MaximumBytes,
}

/// 성공한 Profile Task가 Artifact root에 공개한 immutable file의 metadata다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedArtifact {
    path: ArtifactPath,
    digest: Sha256Digest,
    size_bytes: u64,
    media_type: String,
}

impl PublishedArtifact {
    pub fn path(&self) -> &ArtifactPath {
        &self.path
    }

    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }
}

/// Linux descriptor-relative Artifact lifecycle store다.
///
/// Root와 모든 child는 openat와 O_NOFOLLOW로 열며, 각 component의 device를 root와 비교한다.
/// 그래서 traversal, symlink, magic-link와 mount crossing을 통한 root escape가 target 시작 전에 막힌다.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct LocalArtifactStore {
    root: ArtifactDirectory,
    maximum_bytes: u64,
    preflight_sequence: AtomicU64,
}

#[cfg(target_os = "linux")]
impl LocalArtifactStore {
    /// Artifact root를 열고 daemon 전용 staging layout을 준비한다.
    pub fn open(root: &Path, maximum_bytes: u64) -> Result<Self, ArtifactStoreError> {
        if maximum_bytes == 0 {
            return Err(ArtifactStoreError::MaximumBytes);
        }
        let root = ArtifactDirectory::open_absolute(root)?;
        root.validate_as_daemon_owned_root()?;
        let store = Self {
            root,
            maximum_bytes,
            preflight_sequence: AtomicU64::new(0),
        };
        let layout = store.open_layout()?;
        layout.sync_all()?;
        Ok(store)
    }

    /// Input을 daemon-owned snapshot으로 복사하고 task staging directory를 만든다.
    ///
    /// 이 메서드는 Registry reservation, cgroup, target보다 먼저 호출되어야 한다. 실패하면 target이
    /// 시작되지 않은 상태에서 preflight staging을 정리한다.
    pub fn stage_input(
        self: &Arc<Self>,
        task_id: &str,
        input: &LocalInputArtifact,
        output: DeclaredOutputArtifact,
    ) -> Result<StagedArtifactTask, ArtifactStoreError> {
        validate_task_id(task_id)?;
        let layout = self.open_layout()?;
        let preflight_name = self.next_preflight_name(task_id);
        let preflight = layout.preflight.create_child(&preflight_name)?;
        let created_task_staging = Cell::new(false);

        let result = (|| {
            let mut source = self.open_input(input.path())?;
            let snapshot = preflight.create_regular_file("source", 0o400)?;
            copy_and_verify_input(input, self.maximum_bytes, &mut source, snapshot)?;
            preflight.sync_all()?;

            let task_directory = layout.staging.create_child(task_id)?;
            created_task_staging.set(true);
            let artifacts = task_directory.create_child("artifacts")?;
            let input_directory = artifacts.create_child("in")?;
            let output_directory = artifacts.create_child("out")?;
            rename_no_replace(&preflight, "source", &input_directory, "source")?;
            input_directory.sync_all()?;
            output_directory.sync_all()?;
            task_directory.sync_all()?;
            layout.staging.sync_all()?;
            preflight.remove_empty_from(&layout.preflight)?;
            layout.preflight.sync_all()?;

            Ok(StagedArtifactTask {
                store: Arc::clone(self),
                task_id: task_id.to_owned(),
                task_directory,
                output_directory,
                output,
                terminal: false,
            })
        })();

        if result.is_err() {
            let _ = preflight.remove_tree();
            if created_task_staging.get() {
                let _ = layout.staging.remove_child_tree(task_id);
            }
        }
        result
    }

    fn next_preflight_name(&self, task_id: &str) -> String {
        let sequence = self.preflight_sequence.fetch_add(1, Ordering::Relaxed);
        format!("{task_id}-{sequence}")
    }

    fn open_layout(&self) -> Result<ArtifactLayout, ArtifactStoreError> {
        let taskcage = self.root.open_or_create_child(".taskcage")?;
        let preflight = taskcage.open_or_create_child("preflight")?;
        let staging = taskcage.open_or_create_child("staging")?;
        let published = self.root.open_or_create_child("tasks")?;
        Ok(ArtifactLayout {
            preflight,
            staging,
            published,
        })
    }

    fn open_input(&self, path: &ArtifactPath) -> Result<File, ArtifactStoreError> {
        let mut directory = self.root.open_child(".")?;
        let mut components = path.as_str().split('/').peekable();
        while let Some(component) = components.next() {
            if components.peek().is_some() {
                directory = directory.open_child(component)?;
            } else {
                return directory.open_regular_file(component);
            }
        }
        unreachable!("ArtifactPath는 빈 path를 허용하지 않습니다")
    }
}

/// Profile 실행 전에 만든 daemon-owned input/output staging directory다.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct StagedArtifactTask {
    store: Arc<LocalArtifactStore>,
    task_id: String,
    task_directory: ArtifactDirectory,
    output_directory: ArtifactDirectory,
    output: DeclaredOutputArtifact,
    terminal: bool,
}

#[cfg(target_os = "linux")]
impl StagedArtifactTask {
    /// Profile target의 relative path 해석을 막기 위한 daemon-owned absolute working directory다.
    pub fn working_directory(&self) -> PathBuf {
        self.task_directory.path.clone()
    }

    /// Target이 읽기 전용 snapshot을 받는 absolute path다.
    pub fn input_path(&self) -> PathBuf {
        self.task_directory
            .path
            .join("artifacts")
            .join("in")
            .join("source")
    }

    /// Target이 output을 쓸 유일한 staging path다.
    pub fn output_path(&self) -> PathBuf {
        self.output_directory.path.join("result.part")
    }

    /// target 성공·whole-task cleanup 뒤 output을 검증하고 no-overwrite atomic rename으로 공개한다.
    pub fn publish(self) -> Result<PublishedArtifact, ArtifactStoreError> {
        match self.publish_for_profile()? {
            Ok(artifact) => Ok(artifact),
            Err(error) => Err(error),
        }
    }

    /// Profile terminal state를 만들기 위해 publish rejection과 cleanup uncertainty를 분리한다.
    ///
    /// outer `Err`는 staging cleanup 자체가 확인되지 않은 경우이며, caller는 FINISHED를 공개하지
    /// 않고 fail-stop 해야 한다. inner `Err`는 cleanup을 확인한 output contract/publish rejection이다.
    pub(crate) fn publish_for_profile(
        mut self,
    ) -> Result<Result<PublishedArtifact, ArtifactStoreError>, ArtifactStoreError> {
        let result = self.publish_inner();
        match result {
            Ok(artifact) => {
                self.terminal = true;
                Ok(Ok(artifact))
            }
            Err(error) => {
                // profile failure도 staging cleanup이 끝난 뒤에만 공개할 수 있다. 오류가 난 지점이
                // rename 전이든 후 rollback 뒤이든, task directory가 남지 않았음을 동기적으로 확인한다.
                if self.task_directory.path.exists() {
                    self.cleanup_staging()?;
                }
                self.terminal = true;
                Ok(Err(error))
            }
        }
    }

    fn publish_inner(&self) -> Result<PublishedArtifact, ArtifactStoreError> {
        self.validate_declared_staging()?;
        let output = self.output_directory.open_regular_file("result.part")?;
        let (size_bytes, digest) = verify_output(
            &output,
            self.output.maximum_bytes().min(self.store.maximum_bytes),
        )?;
        output.sync_all().map_err(|source| ArtifactStoreError::Io {
            operation: "output fsync",
            path: self.output_path(),
            source,
        })?;

        let layout = self.store.open_layout()?;
        let destination = match layout.published.create_child(&self.task_id) {
            Ok(destination) => destination,
            Err(ArtifactStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                return Err(ArtifactStoreError::DestinationExists(
                    layout.published.path.join(&self.task_id),
                ));
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = rename_no_replace(
            &self.output_directory,
            "result.part",
            &destination,
            self.output.file_name(),
        ) {
            let _ = destination.remove_empty_from(&layout.published);
            return Err(error);
        }
        if let Err(error) = (|| {
            destination.sync_all()?;
            layout.published.sync_all()?;
            self.cleanup_staging()
        })() {
            let _ = destination.remove_regular_file(self.output.file_name());
            let _ = destination.remove_empty_from(&layout.published);
            let _ = layout.published.sync_all();
            return Err(error);
        }

        let relative = ArtifactPath::parse(format!(
            "tasks/{}/{}",
            self.task_id,
            self.output.file_name()
        ))
        .expect("Task id와 output file name은 이미 검증됐습니다");
        Ok(PublishedArtifact {
            path: relative,
            digest,
            size_bytes,
            media_type: self.output.media_type().to_owned(),
        })
    }

    /// timeout, cancellation, non-zero exit 또는 publish failure 뒤 staging을 확인하며 제거한다.
    pub fn cleanup(mut self) -> Result<(), ArtifactStoreError> {
        self.cleanup_staging()?;
        self.terminal = true;
        Ok(())
    }

    fn cleanup_staging(&self) -> Result<(), ArtifactStoreError> {
        self.task_directory.remove_tree()?;
        let layout = self.store.open_layout()?;
        layout.staging.sync_all()
    }

    fn validate_declared_staging(&self) -> Result<(), ArtifactStoreError> {
        require_exact_children(&self.task_directory.path, &["artifacts"])?;
        let artifacts = self.task_directory.path.join("artifacts");
        require_directory(&artifacts)?;
        require_exact_children(&artifacts, &["in", "out"])?;
        let input = artifacts.join("in");
        let output = artifacts.join("out");
        require_directory(&input)?;
        require_directory(&output)?;
        require_exact_children(&input, &["source"])?;
        require_exact_children(&output, &["result.part"])?;
        require_regular_file(&input.join("source"))?;
        require_regular_file(&output.join("result.part"))
    }
}

#[cfg(target_os = "linux")]
impl Drop for StagedArtifactTask {
    fn drop(&mut self) {
        if !self.terminal {
            let _ = self.cleanup_staging();
        }
    }
}

/// Local Artifact store 초기화·staging·publish 오류다.
#[cfg(target_os = "linux")]
#[derive(Debug, Error)]
pub enum ArtifactStoreError {
    #[error("Artifact root는 symlink가 아닌 absolute directory여야 합니다: {0}")]
    InvalidRoot(PathBuf),
    #[error("Artifact root 또는 child가 다른 filesystem으로 넘어갔습니다: {0}")]
    MountCrossing(PathBuf),
    #[error("Artifact root owner가 daemon effective uid와 다릅니다: {path}, owner={owner}")]
    RootOwner { path: PathBuf, owner: libc::uid_t },
    #[error("Artifact root는 group 또는 other writable일 수 없습니다: {path}, mode={mode:o}")]
    RootMode { path: PathBuf, mode: libc::mode_t },
    #[error("Artifact root 경로에 symlink 또는 안전하지 않은 component가 있습니다: {0}")]
    UnsafePath(PathBuf),
    #[error("Artifact size maximum은 0보다 커야 합니다")]
    MaximumBytes,
    #[error("task id가 canonical UUID가 아닙니다: {0}")]
    InvalidTaskId(String),
    #[error("Artifact path가 regular file이 아닙니다: {0}")]
    NotRegularFile(PathBuf),
    #[error("published output destination이 이미 존재합니다: {0}")]
    DestinationExists(PathBuf),
    #[error("filesystem이 no-overwrite atomic rename을 지원하지 않습니다: {0}")]
    AtomicRenameUnavailable(PathBuf),
    #[error("Profile이 선언하지 않은 Artifact staging entry가 있습니다: {0}")]
    UndeclaredOutput(PathBuf),
    #[error("Artifact filesystem 작업 {operation}에 실패했습니다: {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Verification(#[from] ArtifactVerificationError),
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ArtifactLayout {
    preflight: ArtifactDirectory,
    staging: ArtifactDirectory,
    published: ArtifactDirectory,
}

#[cfg(target_os = "linux")]
impl ArtifactLayout {
    fn sync_all(&self) -> Result<(), ArtifactStoreError> {
        self.preflight.sync_all()?;
        self.staging.sync_all()?;
        self.published.sync_all()
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ArtifactDirectory {
    descriptor: OwnedFd,
    path: PathBuf,
    device: libc::dev_t,
}

#[cfg(target_os = "linux")]
impl ArtifactDirectory {
    fn open_absolute(path: &Path) -> Result<Self, ArtifactStoreError> {
        if !path.is_absolute() {
            return Err(ArtifactStoreError::InvalidRoot(path.to_path_buf()));
        }
        let root_name = CString::new("/").expect("고정 path에는 NUL이 없습니다");
        let descriptor = unsafe {
            libc::open(
                root_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor == -1 {
            return Err(io_error("Artifact root 열기", Path::new("/")));
        }
        let mut current = Self::from_raw(descriptor, PathBuf::from("/"), None)?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name = name
                        .to_str()
                        .ok_or_else(|| ArtifactStoreError::UnsafePath(path.to_path_buf()))?;
                    current = current.open_child_allowing_mount(name)?;
                }
                Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                    return Err(ArtifactStoreError::InvalidRoot(path.to_path_buf()));
                }
            }
        }
        Ok(current)
    }

    fn from_raw(
        descriptor: libc::c_int,
        path: PathBuf,
        expected_device: Option<libc::dev_t>,
    ) -> Result<Self, ArtifactStoreError> {
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let metadata = stat_for(descriptor.as_raw_fd(), &path)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(ArtifactStoreError::InvalidRoot(path));
        }
        if let Some(expected_device) = expected_device
            && metadata.st_dev != expected_device
        {
            return Err(ArtifactStoreError::MountCrossing(path));
        }
        Ok(Self {
            descriptor,
            path,
            device: metadata.st_dev,
        })
    }

    fn open_child(&self, name: &str) -> Result<Self, ArtifactStoreError> {
        self.open_child_with_device(name, Some(self.device))
    }

    fn open_child_allowing_mount(&self, name: &str) -> Result<Self, ArtifactStoreError> {
        self.open_child_with_device(name, None)
    }

    fn open_child_with_device(
        &self,
        name: &str,
        expected_device: Option<libc::dev_t>,
    ) -> Result<Self, ArtifactStoreError> {
        let path = self.path.join(name);
        let name = c_name(name, &path)?;
        let descriptor = unsafe {
            libc::openat(
                self.descriptor.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor == -1 {
            return Err(io_error("Artifact directory 열기", &path));
        }
        Self::from_raw(descriptor, path, expected_device)
    }

    fn open_or_create_child(&self, name: &str) -> Result<Self, ArtifactStoreError> {
        let path = self.path.join(name);
        let name_c = c_name(name, &path)?;
        let result = unsafe { libc::mkdirat(self.descriptor.as_raw_fd(), name_c.as_ptr(), 0o700) };
        if result == -1 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io_error("Artifact directory 생성", &path));
        }
        self.open_child(name)
    }

    fn create_child(&self, name: &str) -> Result<Self, ArtifactStoreError> {
        let path = self.path.join(name);
        let name_c = c_name(name, &path)?;
        if unsafe { libc::mkdirat(self.descriptor.as_raw_fd(), name_c.as_ptr(), 0o700) } == -1 {
            return Err(io_error("Artifact staging directory 생성", &path));
        }
        self.open_child(name)
    }

    fn create_regular_file(
        &self,
        name: &str,
        mode: libc::mode_t,
    ) -> Result<File, ArtifactStoreError> {
        let path = self.path.join(name);
        let name = c_name(name, &path)?;
        let descriptor = unsafe {
            libc::openat(
                self.descriptor.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode,
            )
        };
        if descriptor == -1 {
            return Err(io_error("Artifact staging file 생성", &path));
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    fn open_regular_file(&self, name: &str) -> Result<File, ArtifactStoreError> {
        let path = self.path.join(name);
        let name = c_name(name, &path)?;
        let descriptor = unsafe {
            libc::openat(
                self.descriptor.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor == -1 {
            return Err(io_error("Artifact file 열기", &path));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = stat_for(file.as_raw_fd(), &path)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(ArtifactStoreError::NotRegularFile(path));
        }
        if metadata.st_dev != self.device {
            return Err(ArtifactStoreError::MountCrossing(path));
        }
        Ok(file)
    }

    fn remove_empty_from(&self, parent: &ArtifactDirectory) -> Result<(), ArtifactStoreError> {
        let name = self
            .path
            .file_name()
            .ok_or_else(|| ArtifactStoreError::UnsafePath(self.path.clone()))?;
        let name = c_os_name(name, &self.path)?;
        if unsafe {
            libc::unlinkat(
                parent.descriptor.as_raw_fd(),
                name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } == -1
        {
            return Err(io_error("빈 Artifact directory 제거", &self.path));
        }
        parent.sync_all()
    }

    fn remove_regular_file(&self, name: &str) -> Result<(), ArtifactStoreError> {
        let path = self.path.join(name);
        let name = c_name(name, &path)?;
        if unsafe { libc::unlinkat(self.descriptor.as_raw_fd(), name.as_ptr(), 0) } == -1 {
            return Err(io_error("published Artifact file rollback", &path));
        }
        self.sync_all()
    }

    fn remove_tree(&self) -> Result<(), ArtifactStoreError> {
        fs::remove_dir_all(&self.path).map_err(|source| ArtifactStoreError::Io {
            operation: "Artifact staging 제거",
            path: self.path.clone(),
            source,
        })
    }

    fn remove_child_tree(&self, name: &str) -> Result<(), ArtifactStoreError> {
        let path = self.path.join(name);
        match fs::remove_dir_all(&path) {
            Ok(()) => self.sync_all(),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ArtifactStoreError::Io {
                operation: "Artifact staging rollback",
                path,
                source,
            }),
        }
    }

    fn sync_all(&self) -> Result<(), ArtifactStoreError> {
        let duplicated =
            unsafe { libc::fcntl(self.descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated == -1 {
            return Err(io_error("Artifact directory fsync 준비", &self.path));
        }
        let directory = unsafe { File::from_raw_fd(duplicated) };
        directory
            .sync_all()
            .map_err(|source| ArtifactStoreError::Io {
                operation: "Artifact directory fsync",
                path: self.path.clone(),
                source,
            })
    }

    fn validate_as_daemon_owned_root(&self) -> Result<(), ArtifactStoreError> {
        let metadata = stat_for(self.descriptor.as_raw_fd(), &self.path)?;
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.st_uid != effective_uid {
            return Err(ArtifactStoreError::RootOwner {
                path: self.path.clone(),
                owner: metadata.st_uid,
            });
        }
        let mode = metadata.st_mode & 0o777;
        if mode & 0o022 != 0 {
            return Err(ArtifactStoreError::RootMode {
                path: self.path.clone(),
                mode,
            });
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn validate_task_id(task_id: &str) -> Result<(), ArtifactStoreError> {
    let bytes = task_id.as_bytes();
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && *byte == b'-'
                || !matches!(index, 8 | 13 | 18 | 23)
                    && (byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        })
    {
        return Err(ArtifactStoreError::InvalidTaskId(task_id.to_owned()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn copy_and_verify_input(
    artifact: &LocalInputArtifact,
    maximum_bytes: u64,
    source: &mut File,
    mut destination: File,
) -> Result<(), ArtifactStoreError> {
    if artifact.size_bytes() > maximum_bytes {
        return Err(ArtifactVerificationError::TooLarge {
            actual: artifact.size_bytes(),
            maximum: maximum_bytes,
        }
        .into());
    }

    let mut actual_size = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(ArtifactVerificationError::Read)?;
        if read == 0 {
            break;
        }
        actual_size = actual_size
            .checked_add(u64::try_from(read).expect("read length fits in u64"))
            .ok_or(ArtifactVerificationError::SizeOverflow)?;
        if actual_size > maximum_bytes {
            return Err(ArtifactVerificationError::TooLarge {
                actual: actual_size,
                maximum: maximum_bytes,
            }
            .into());
        }
        std::io::Write::write_all(&mut destination, &buffer[..read]).map_err(|source| {
            ArtifactStoreError::Io {
                operation: "Artifact snapshot 쓰기",
                path: PathBuf::from("<staging input>"),
                source,
            }
        })?;
        hasher.update(&buffer[..read]);
    }
    if actual_size != artifact.size_bytes() {
        return Err(ArtifactVerificationError::SizeMismatch {
            expected: artifact.size_bytes(),
            actual: actual_size,
        }
        .into());
    }
    let actual = Sha256Digest::from_bytes(hasher.finalize().into());
    if actual != artifact.digest() {
        return Err(ArtifactVerificationError::DigestMismatch.into());
    }
    destination
        .sync_all()
        .map_err(|source| ArtifactStoreError::Io {
            operation: "Artifact snapshot fsync",
            path: PathBuf::from("<staging input>"),
            source,
        })
}

#[cfg(target_os = "linux")]
fn verify_output(
    file: &File,
    maximum_bytes: u64,
) -> Result<(u64, Sha256Digest), ArtifactStoreError> {
    let mut reader = file.try_clone().map_err(|source| ArtifactStoreError::Io {
        operation: "Artifact output 복제",
        path: PathBuf::from("<staging output>"),
        source,
    })?;
    let mut actual_size = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(ArtifactVerificationError::Read)?;
        if read == 0 {
            break;
        }
        actual_size = actual_size
            .checked_add(u64::try_from(read).expect("read length fits in u64"))
            .ok_or(ArtifactVerificationError::SizeOverflow)?;
        if actual_size > maximum_bytes {
            return Err(ArtifactVerificationError::TooLarge {
                actual: actual_size,
                maximum: maximum_bytes,
            }
            .into());
        }
        hasher.update(&buffer[..read]);
    }
    Ok((
        actual_size,
        Sha256Digest::from_bytes(hasher.finalize().into()),
    ))
}

#[cfg(target_os = "linux")]
fn rename_no_replace(
    from_directory: &ArtifactDirectory,
    from: &str,
    to_directory: &ArtifactDirectory,
    to: &str,
) -> Result<(), ArtifactStoreError> {
    let from_path = from_directory.path.join(from);
    let to_path = to_directory.path.join(to);
    let from = c_name(from, &from_path)?;
    let to = c_name(to, &to_path)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from_directory.descriptor.as_raw_fd(),
            from.as_ptr(),
            to_directory.descriptor.as_raw_fd(),
            to.as_ptr(),
            1_u32,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let source = io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::EEXIST) {
        return Err(ArtifactStoreError::DestinationExists(to_path));
    }
    if matches!(source.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) {
        return Err(ArtifactStoreError::AtomicRenameUnavailable(to_path));
    }
    Err(ArtifactStoreError::Io {
        operation: "Artifact no-overwrite rename",
        path: to_path,
        source,
    })
}

#[cfg(target_os = "linux")]
fn c_name(name: &str, path: &Path) -> Result<CString, ArtifactStoreError> {
    c_os_name(OsStr::new(name), path)
}

#[cfg(target_os = "linux")]
fn c_os_name(name: &OsStr, path: &Path) -> Result<CString, ArtifactStoreError> {
    use std::os::unix::ffi::OsStrExt;

    CString::new(name.as_bytes()).map_err(|_| ArtifactStoreError::UnsafePath(path.to_path_buf()))
}

#[cfg(target_os = "linux")]
fn stat_for(descriptor: libc::c_int, path: &Path) -> Result<libc::stat, ArtifactStoreError> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(descriptor, metadata.as_mut_ptr()) } == -1 {
        return Err(io_error("Artifact metadata 확인", path));
    }
    Ok(unsafe { metadata.assume_init() })
}

#[cfg(target_os = "linux")]
fn io_error(operation: &'static str, path: &Path) -> ArtifactStoreError {
    ArtifactStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source: io::Error::last_os_error(),
    }
}

#[cfg(target_os = "linux")]
fn require_exact_children(directory: &Path, expected: &[&str]) -> Result<(), ArtifactStoreError> {
    let entries = fs::read_dir(directory).map_err(|source| ArtifactStoreError::Io {
        operation: "Artifact staging directory 열거",
        path: directory.to_path_buf(),
        source,
    })?;
    let mut actual = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ArtifactStoreError::Io {
            operation: "Artifact staging entry 읽기",
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| ArtifactStoreError::Io {
            operation: "Artifact staging entry metadata",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ArtifactStoreError::UndeclaredOutput(path));
        }
        actual.push(entry.file_name());
    }
    actual.sort();
    let mut expected: Vec<_> = expected.iter().map(OsStr::new).collect();
    expected.sort();
    if actual.iter().map(|name| name.as_os_str()).ne(expected) {
        return Err(ArtifactStoreError::UndeclaredOutput(
            directory.to_path_buf(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_directory(path: &Path) -> Result<(), ArtifactStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ArtifactStoreError::Io {
        operation: "Artifact staging directory metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(ArtifactStoreError::UndeclaredOutput(path.to_path_buf()))
    }
}

#[cfg(target_os = "linux")]
fn require_regular_file(path: &Path) -> Result<(), ArtifactStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ArtifactStoreError::Io {
        operation: "Artifact staging file metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(ArtifactStoreError::UndeclaredOutput(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::io::Cursor;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn parse_descriptor(value: &Value) -> LocalInputArtifact {
        let object = value
            .as_object()
            .expect("Artifact descriptor는 JSON object여야 합니다");
        assert_eq!(
            object.get("kind").and_then(Value::as_str),
            Some("LOCAL_INPUT")
        );
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .expect("path가 필요합니다");
        let digest = object
            .get("digest")
            .and_then(Value::as_str)
            .expect("digest가 필요합니다");
        let size_bytes = object
            .get("sizeBytes")
            .and_then(Value::as_u64)
            .expect("sizeBytes는 unsigned JSON integer여야 합니다");

        LocalInputArtifact::new(
            ArtifactPath::parse(path).expect("Artifact path가 유효해야 합니다"),
            digest
                .parse()
                .expect("digest가 canonical SHA-256이어야 합니다"),
            size_bytes,
        )
    }

    #[test]
    fn accepts_a_canonical_root_relative_input_path() {
        let path = ArtifactPath::parse("jobs/42/source.mov").expect("valid Artifact path");

        assert_eq!(path.as_str(), "jobs/42/source.mov");
    }

    #[test]
    fn keeps_unicode_and_percent_sequences_as_literal_path_bytes() {
        let path = ArtifactPath::parse("입력/%2e%2e/source.mov").expect("valid literal path");

        assert_eq!(path.to_string(), "입력/%2e%2e/source.mov");
    }

    #[test]
    fn rejects_traversal_reserved_and_ambiguous_paths() {
        for path in [
            "",
            "/absolute/file",
            "jobs/../source.mov",
            "jobs//source.mov",
            "jobs/./source.mov",
            ".taskcage/staging/input",
            "jobs\\source.mov",
            "jobs/line\\nbreak",
        ] {
            assert!(ArtifactPath::parse(path).is_err(), "path={path:?}");
        }
    }

    #[test]
    fn binds_path_digest_and_size_without_a_mutable_source_reference() {
        let artifact = LocalInputArtifact::new(
            ArtifactPath::parse("jobs/42/source.mov").unwrap(),
            DIGEST.parse().unwrap(),
            1_048_576,
        );

        assert_eq!(artifact.path().as_str(), "jobs/42/source.mov");
        assert_eq!(artifact.digest().to_string(), DIGEST);
        assert_eq!(artifact.size_bytes(), 1_048_576);
    }

    fn descriptor_for(bytes: &[u8]) -> LocalInputArtifact {
        let digest = Sha256Digest::from_bytes(Sha256::digest(bytes).into());
        LocalInputArtifact::new(
            ArtifactPath::parse("jobs/42/source.mov").unwrap(),
            digest,
            u64::try_from(bytes.len()).unwrap(),
        )
    }

    #[test]
    fn verifies_size_and_digest_before_execution() {
        let bytes = b"TaskCage input snapshot";
        let artifact = descriptor_for(bytes);

        verify_input(&artifact, 1_024, &mut Cursor::new(bytes)).expect("matching input");
    }

    #[test]
    fn rejects_changed_source_bytes_before_execution() {
        let artifact = descriptor_for(b"original source");

        let error = verify_input(&artifact, 1_024, &mut Cursor::new(b"changed! source"))
            .expect_err("changed bytes must not be accepted");

        assert!(matches!(error, ArtifactVerificationError::DigestMismatch));
    }

    #[test]
    fn rejects_source_larger_than_the_deployment_limit_before_execution() {
        let bytes = b"too large";
        let artifact = descriptor_for(bytes);

        let error = verify_input(&artifact, 8, &mut Cursor::new(bytes))
            .expect_err("deployment maximum must apply before target start");

        assert!(matches!(
            error,
            ArtifactVerificationError::TooLarge {
                actual: 9,
                maximum: 8
            }
        ));
    }

    #[test]
    fn valid_fixture_is_a_canonical_local_input_descriptor() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-input-valid.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");

        let artifact = parse_descriptor(&fixture);

        assert_eq!(artifact.path().as_str(), "jobs/42/source.mov");
        assert_eq!(artifact.digest().to_string(), DIGEST);
        assert_eq!(artifact.size_bytes(), 1_048_576);
    }

    #[test]
    fn rejection_fixtures_require_target_not_to_start() {
        let invalid_path: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-input-invalid-path.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");
        let mismatch: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-input-digest-mismatch.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");

        let descriptor = invalid_path
            .get("descriptor")
            .expect("invalid path fixture에는 descriptor가 필요합니다");
        let path = descriptor
            .get("path")
            .and_then(Value::as_str)
            .expect("path가 필요합니다");
        assert!(ArtifactPath::parse(path).is_err());
        assert_eq!(
            invalid_path.get("expectedError").and_then(Value::as_str),
            Some("INVALID_ARTIFACT_PATH")
        );
        assert_eq!(
            invalid_path.get("targetMustStart").and_then(Value::as_bool),
            Some(false)
        );

        let descriptor = mismatch
            .get("descriptor")
            .expect("digest mismatch fixture에는 descriptor가 필요합니다");
        let artifact = parse_descriptor(descriptor);
        assert_eq!(artifact.path().as_str(), "jobs/42/source.mov");
        assert_eq!(
            mismatch.get("expectedError").and_then(Value::as_str),
            Some("ARTIFACT_DIGEST_MISMATCH")
        );
        assert_eq!(
            mismatch.get("targetMustStart").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn undeclared_output_fixture_requires_no_result_publication() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-output-undeclared.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");

        assert_eq!(
            fixture.get("stagingEntries").and_then(Value::as_array),
            Some(&vec![
                Value::String("result.part".to_owned()),
                Value::String("unexpected.bin".to_owned())
            ])
        );
        assert_eq!(
            fixture.get("expectedFailure").and_then(Value::as_str),
            Some("OUTPUT_CONTRACT_VIOLATION")
        );
        assert_eq!(
            fixture.get("resultMustPublish").and_then(Value::as_bool),
            Some(false)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_store_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    const TASK_ID: &str = "11111111-1111-4111-8111-111111111111";
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryRoot {
        path: PathBuf,
    }

    impl TemporaryRoot {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taskcage-artifact-tests-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("Artifact test root 생성");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TemporaryRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn input_descriptor(root: &Path, relative: &str) -> LocalInputArtifact {
        let bytes = fs::read(root.join(relative)).expect("input bytes 읽기");
        LocalInputArtifact::new(
            ArtifactPath::parse(relative).expect("relative path 검증"),
            Sha256Digest::from_bytes(Sha256::digest(&bytes).into()),
            u64::try_from(bytes.len()).expect("test input size"),
        )
    }

    fn output_contract() -> DeclaredOutputArtifact {
        DeclaredOutputArtifact::new("result.bin", "application/octet-stream", 1_024)
            .expect("output contract")
    }

    #[test]
    fn stages_verified_input_and_publishes_one_output_only_after_explicit_publish() {
        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("inputs/source.bin"), b"source bytes").expect("input 생성");
        let input = input_descriptor(root.path(), "inputs/source.bin");
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        let staged = store
            .stage_input(TASK_ID, &input, output_contract())
            .expect("input snapshot staging");
        assert_eq!(fs::read(staged.input_path()).unwrap(), b"source bytes");
        assert!(
            !root
                .path()
                .join("tasks")
                .join(TASK_ID)
                .join("result.bin")
                .exists(),
            "publish 전에는 caller-visible output이 없어야 합니다"
        );

        fs::write(staged.output_path(), b"result bytes").expect("target output 생성");
        let published = staged.publish().expect("output publish");

        let final_path = root.path().join(published.path().as_str());
        assert_eq!(fs::read(final_path).unwrap(), b"result bytes");
        assert_eq!(published.size_bytes(), 12);
        assert_eq!(published.media_type(), "application/octet-stream");
        assert!(
            !root.path().join(".taskcage/staging").join(TASK_ID).exists(),
            "success result 전에 Artifact staging이 정리되어야 합니다"
        );
        assert_eq!(
            fs::read(root.path().join("inputs/source.bin")).unwrap(),
            b"source bytes",
            "caller input은 daemon이 수정하지 않습니다"
        );
    }

    #[test]
    fn reaped_process_consumes_staged_input_before_output_is_published() {
        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("inputs/source.bin"), b"source bytes").expect("input 생성");
        let input = input_descriptor(root.path(), "inputs/source.bin");
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        let staged = store
            .stage_input(TASK_ID, &input, output_contract())
            .expect("input snapshot staging");
        let mut child = std::process::Command::new("/bin/cp")
            .arg(staged.input_path())
            .arg(staged.output_path())
            .spawn()
            .expect("staged input을 소비하는 child 시작");

        assert!(
            !root
                .path()
                .join("tasks")
                .join(TASK_ID)
                .join("result.bin")
                .exists(),
            "child 종료 전에는 caller-visible output이 없어야 합니다"
        );
        assert!(child.wait().expect("child wait").success());

        let published = staged.publish().expect("reaped child output publish");
        assert_eq!(
            fs::read(root.path().join(published.path().as_str())).unwrap(),
            b"source bytes"
        );
    }

    #[test]
    fn symlink_input_is_rejected_before_any_task_staging_is_created() {
        use std::os::unix::fs::symlink;

        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("outside.bin"), b"outside").expect("outside input 생성");
        symlink(
            root.path().join("outside.bin"),
            root.path().join("inputs/link.bin"),
        )
        .expect("symlink 생성");
        let input = LocalInputArtifact::new(
            ArtifactPath::parse("inputs/link.bin").unwrap(),
            Sha256Digest::from_bytes(Sha256::digest(b"outside").into()),
            7,
        );
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        assert!(
            store
                .stage_input(TASK_ID, &input, output_contract())
                .is_err()
        );
        assert!(
            !root.path().join(".taskcage/staging").join(TASK_ID).exists(),
            "invalid input은 task staging을 만들 수 없습니다"
        );
    }

    #[test]
    fn changed_input_is_rejected_and_preflight_snapshot_is_removed() {
        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("inputs/source.bin"), b"changed source").expect("input 생성");
        let input = LocalInputArtifact::new(
            ArtifactPath::parse("inputs/source.bin").unwrap(),
            Sha256Digest::from_bytes(Sha256::digest(b"original source").into()),
            14,
        );
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        assert!(matches!(
            store.stage_input(TASK_ID, &input, output_contract()),
            Err(ArtifactStoreError::Verification(
                ArtifactVerificationError::DigestMismatch
            ))
        ));
        let preflight = root.path().join(".taskcage/preflight");
        assert_eq!(fs::read_dir(preflight).unwrap().count(), 0);
        assert!(!root.path().join(".taskcage/staging").join(TASK_ID).exists());
    }

    #[test]
    fn cleanup_discards_staged_output_without_publishing_it() {
        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("inputs/source.bin"), b"source bytes").expect("input 생성");
        let input = input_descriptor(root.path(), "inputs/source.bin");
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        let staged = store
            .stage_input(TASK_ID, &input, output_contract())
            .expect("input snapshot staging");
        fs::write(staged.output_path(), b"discard me").expect("output 생성");
        staged.cleanup().expect("failure path cleanup");

        assert!(!root.path().join("tasks").join(TASK_ID).exists());
        assert!(!root.path().join(".taskcage/staging").join(TASK_ID).exists());
    }

    #[test]
    fn existing_published_task_is_never_overwritten() {
        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("inputs/source.bin"), b"source bytes").expect("input 생성");
        fs::create_dir_all(root.path().join("tasks").join(TASK_ID))
            .expect("existing destination 생성");
        fs::write(
            root.path().join("tasks").join(TASK_ID).join("result.bin"),
            b"original result",
        )
        .expect("existing output 생성");
        let input = input_descriptor(root.path(), "inputs/source.bin");
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        let staged = store
            .stage_input(TASK_ID, &input, output_contract())
            .expect("input snapshot staging");
        fs::write(staged.output_path(), b"new result").expect("output 생성");
        assert!(matches!(
            staged.publish(),
            Err(ArtifactStoreError::DestinationExists(_))
        ));
        assert_eq!(
            fs::read(root.path().join("tasks").join(TASK_ID).join("result.bin")).unwrap(),
            b"original result"
        );
        assert!(!root.path().join(".taskcage/staging").join(TASK_ID).exists());
    }

    #[test]
    fn undeclared_output_is_rejected_and_never_published() {
        let root = TemporaryRoot::new();
        fs::create_dir(root.path().join("inputs")).expect("inputs directory 생성");
        fs::write(root.path().join("inputs/source.bin"), b"source bytes").expect("input 생성");
        let input = input_descriptor(root.path(), "inputs/source.bin");
        let store =
            Arc::new(LocalArtifactStore::open(root.path(), 1_024).expect("Artifact store 준비"));

        let staged = store
            .stage_input(TASK_ID, &input, output_contract())
            .expect("input snapshot staging");
        fs::write(staged.output_path(), b"declared output").expect("declared output 생성");
        fs::write(
            staged
                .output_path()
                .parent()
                .unwrap()
                .join("unexpected.bin"),
            b"undeclared output",
        )
        .expect("undeclared output 생성");

        assert!(matches!(
            staged.publish(),
            Err(ArtifactStoreError::UndeclaredOutput(_))
        ));
        assert!(!root.path().join("tasks").join(TASK_ID).exists());
        assert!(!root.path().join(".taskcage/staging").join(TASK_ID).exists());
    }

    #[test]
    fn group_or_other_writable_root_is_rejected_before_staging() {
        use std::os::unix::fs::PermissionsExt;

        let root = TemporaryRoot::new();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777))
            .expect("unsafe mode 설정");

        assert!(matches!(
            LocalArtifactStore::open(root.path(), 1_024),
            Err(ArtifactStoreError::RootMode { .. })
        ));
    }
}
