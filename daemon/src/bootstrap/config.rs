use std::path::PathBuf;
use std::time::Duration;

use crate::deployment_policy::DeploymentResourcePolicy;
use crate::digest::Sha256Digest;
use crate::fail_stop::FailStopSettings;
use crate::protocol::{OutputLimits, ResourceLimits};
use crate::remote_config::RemoteDaemonConfig;
use crate::{Error, Result};

#[derive(Debug, Clone)]
/// 서비스 daemon이 사용할 명시적 socket과 내부 실행 설정이다.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct DaemonConfig {
    pub(super) socket_path: PathBuf,
    pub(super) max_concurrent_tasks: u32,
    pub(super) max_registry_tasks: usize,
    pub(super) max_concurrent_connections: std::num::NonZeroUsize,
    pub(super) cleanup_timeout: Duration,
    pub(super) fail_stop_timeout: Duration,
    pub(super) deployment_policy: DeploymentResourcePolicy,
    pub(super) local_profile: Option<LocalProfileConfig>,
    pub(super) remote: Option<RemoteDaemonConfig>,
}

/// 명시적으로 활성화한 v0.2 test Profile의 daemon-owned Artifact root 설정이다.
#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct LocalProfileConfig {
    pub(super) artifact_root: PathBuf,
    pub(super) maximum_artifact_bytes: u64,
    pub(super) ffmpeg_audio_to_wav: Option<FfmpegRuntimePackageConfig>,
    pub(super) bundle_cache_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) struct FfmpegRuntimePackageConfig {
    pub(super) cache_root: PathBuf,
    pub(super) digest: Sha256Digest,
}

#[derive(Debug, Clone)]
/// 배포가 Task 하나에 허용하는 자원 예산 최대값이다.
pub struct DeploymentResourceMaximum {
    limits: ResourceLimits,
    output: OutputLimits,
}

impl DeploymentResourceMaximum {
    pub fn new(limits: ResourceLimits, output: OutputLimits) -> Self {
        Self { limits, output }
    }
}

impl DaemonConfig {
    pub fn new(
        socket_path: PathBuf,
        max_concurrent_tasks: u32,
        max_registry_tasks: usize,
        max_concurrent_connections: usize,
        cleanup_timeout: Duration,
        fail_stop_timeout: Duration,
        maximum_task_resources: DeploymentResourceMaximum,
    ) -> Result<Self> {
        if !socket_path.is_absolute() {
            return Err(Error::InvalidArgument(
                "daemon socket 경로는 절대 경로여야 합니다".to_owned(),
            ));
        }
        if max_concurrent_tasks == 0 {
            return Err(Error::InvalidArgument(
                "max-concurrent-tasks 값은 0보다 커야 합니다".to_owned(),
            ));
        }
        let max_registry_tasks =
            std::num::NonZeroUsize::new(max_registry_tasks).ok_or_else(|| {
                Error::InvalidArgument("max-registry-tasks 값은 0보다 커야 합니다".to_owned())
            })?;
        let max_concurrent_tasks_usize = usize::try_from(max_concurrent_tasks).map_err(|_| {
            Error::InvalidArgument(
                "max-concurrent-tasks 값을 Registry 작업 수와 비교할 수 없습니다".to_owned(),
            )
        })?;
        if max_registry_tasks.get() < max_concurrent_tasks_usize {
            return Err(Error::InvalidArgument(
                "max-registry-tasks 값은 max-concurrent-tasks 이상이어야 합니다".to_owned(),
            ));
        }
        let max_concurrent_connections = std::num::NonZeroUsize::new(max_concurrent_connections)
            .ok_or_else(|| {
                Error::InvalidArgument(
                    "max-concurrent-connections 값은 0보다 커야 합니다".to_owned(),
                )
            })?;
        if cleanup_timeout.is_zero() {
            return Err(Error::InvalidArgument(
                "cleanup-timeout-ms 값은 0보다 커야 합니다".to_owned(),
            ));
        }
        FailStopSettings::new(fail_stop_timeout)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        let deployment_policy = DeploymentResourcePolicy::try_new(
            maximum_task_resources.limits,
            maximum_task_resources.output,
        )
        .map_err(|error| {
            Error::InvalidArgument(format!(
                "deployment resource policy가 잘못되었습니다: {error}"
            ))
        })?;
        Ok(Self {
            socket_path,
            max_concurrent_tasks,
            max_registry_tasks: max_registry_tasks.get(),
            max_concurrent_connections,
            cleanup_timeout,
            fail_stop_timeout,
            deployment_policy,
            local_profile: None,
            remote: None,
        })
    }

    /// immutable `file-copy@1.0.0` test Profile을 enable한다.
    ///
    /// Artifact root의 owner, mode, symlink/mount safety는 daemon startup에서 descriptor-relative로
    /// 다시 검증한다. 이 builder는 Raw Command Protocol v1의 기본 행동을 바꾸지 않는다.
    pub fn with_file_copy_profile(
        mut self,
        artifact_root: PathBuf,
        maximum_artifact_bytes: u64,
    ) -> Result<Self> {
        if !artifact_root.is_absolute() {
            return Err(Error::InvalidArgument(
                "artifact-root 경로는 절대 경로여야 합니다".to_owned(),
            ));
        }
        if artifact_root.to_str().is_none() {
            return Err(Error::InvalidArgument(
                "artifact-root 경로는 UTF-8이어야 합니다".to_owned(),
            ));
        }
        if maximum_artifact_bytes == 0 {
            return Err(Error::InvalidArgument(
                "artifact-max-bytes 값은 0보다 커야 합니다".to_owned(),
            ));
        }
        self.local_profile = Some(LocalProfileConfig {
            artifact_root,
            maximum_artifact_bytes,
            ffmpeg_audio_to_wav: None,
            bundle_cache_root: None,
        });
        Ok(self)
    }

    /// `ffmpeg-audio-to-wav@1.0.0`을 하나의 검증된 Runtime Package digest에 등록한다.
    pub fn with_ffmpeg_audio_to_wav_profile(
        mut self,
        cache_root: PathBuf,
        digest: Sha256Digest,
    ) -> Result<Self> {
        if !cache_root.is_absolute() {
            return Err(Error::InvalidArgument(
                "runtime-package-cache-root 경로는 절대 경로여야 합니다".to_owned(),
            ));
        }
        if cache_root.to_str().is_none() {
            return Err(Error::InvalidArgument(
                "runtime-package-cache-root 경로는 UTF-8이어야 합니다".to_owned(),
            ));
        }
        let local_profile = self.local_profile.as_mut().ok_or_else(|| {
            Error::InvalidArgument(
                "FFmpeg Profile 등록에는 완전한 Profile Artifact 설정이 필요합니다".to_owned(),
            )
        })?;
        if local_profile.ffmpeg_audio_to_wav.is_some() {
            return Err(Error::InvalidArgument(
                "FFmpeg Profile Runtime Package가 이미 등록되었습니다".to_owned(),
            ));
        }
        local_profile.ffmpeg_audio_to_wav = Some(FfmpegRuntimePackageConfig { cache_root, digest });
        Ok(self)
    }

    /// 설치된 Bundle catalog에서 Profile을 resolve할 cache root를 등록한다.
    /// Bundle과 Runtime Package는 같은 immutable cache root를 공유한다.
    pub fn with_bundle_profile_catalog(mut self, cache_root: PathBuf) -> Result<Self> {
        if !cache_root.is_absolute() {
            return Err(Error::InvalidArgument(
                "bundle-cache-root 경로는 절대 경로여야 합니다".to_owned(),
            ));
        }
        if cache_root.to_str().is_none() {
            return Err(Error::InvalidArgument(
                "bundle-cache-root 경로는 UTF-8이어야 합니다".to_owned(),
            ));
        }
        let local_profile = self.local_profile.as_mut().ok_or_else(|| {
            Error::InvalidArgument(
                "Bundle Profile catalog에는 완전한 Profile Artifact 설정이 필요합니다".to_owned(),
            )
        })?;
        if local_profile.bundle_cache_root.is_some() {
            return Err(Error::InvalidArgument(
                "Bundle Profile catalog가 이미 등록되었습니다".to_owned(),
            ));
        }
        local_profile.bundle_cache_root = Some(cache_root);
        Ok(self)
    }

    /// 승인된 Remote Protocol v1 listener deployment 설정을 추가한다.
    pub fn with_remote_config(mut self, path: PathBuf) -> Result<Self> {
        let remote = RemoteDaemonConfig::load(&path)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        self.remote = Some(remote);
        Ok(self)
    }
}
