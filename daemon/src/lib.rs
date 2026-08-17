//! TaskCage Rust 데몬의 사전 검사와 작업 실행 생명주기를 제공한다.

pub mod artifact;
#[cfg(any(target_os = "linux", test))]
mod audit;
#[cfg(target_os = "linux")]
pub mod bundle;
#[cfg_attr(
    not(any(target_os = "linux", test)),
    allow(dead_code, reason = "protocol task 취소는 Linux에서만 제공됩니다")
)]
mod cancellation;
pub mod capability;
mod capacity;
pub mod cgroup;
#[cfg(all(target_os = "linux", test))]
mod cleanup_fault;
pub mod codec;
mod deadline;
mod deployment_policy;
pub mod digest;
mod execution_plan;
#[cfg(target_os = "linux")]
mod executor;
mod fail_stop;
#[cfg(any(target_os = "linux", test))]
mod handlers;
#[cfg_attr(
    not(any(target_os = "linux", test)),
    allow(dead_code, reason = "protocol task lifecycle은 Linux에서만 제공됩니다")
)]
mod lifecycle;
pub mod output;
pub mod preflight;
#[cfg(target_os = "linux")]
mod profile;
pub mod protocol;
pub mod remote_artifact;
pub mod remote_auth;
#[cfg(target_os = "linux")]
mod remote_backend;
pub mod remote_config;
pub mod remote_dispatch;
pub mod remote_protocol;
pub mod remote_server;
pub mod resource_budget;
#[cfg(target_os = "linux")]
mod runner;
pub mod runtime_package;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod startup;
#[cfg(target_os = "linux")]
mod startup_cgroup;
#[cfg(target_os = "linux")]
pub mod status;
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "protocol task 실행은 Linux에서만 제공됩니다")
)]
mod submit;

use std::collections::BTreeMap;
use std::ffi::OsString;
#[cfg(test)]
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "linux")]
use cancellation::cancellation_channel;
#[cfg(target_os = "linux")]
use capacity::TaskCapacitySettings;
#[cfg(target_os = "linux")]
use cgroup::CgroupManager;
use cgroup::{CgroupError, CgroupLimits, JobStats};
#[cfg(target_os = "linux")]
use executor::{ExecutorError, PreparedCommand};
#[cfg(target_os = "linux")]
use fail_stop::FailStopCoordinator;
use fail_stop::FailStopSettings;
#[cfg(target_os = "linux")]
use handlers::ProtocolHandlers;
use output::CaptureLimits;
use preflight::{CapabilityProbe, CapabilityReport, PreflightError, SystemProbe};
use protocol::{OutputLimits, ResourceLimits, TaskOutput};
#[cfg(target_os = "linux")]
use runner::{ExecutionConfig, execute};
use serde::Serialize;
#[cfg(target_os = "linux")]
use startup::StartupOwnership;
#[cfg(target_os = "linux")]
use startup_cgroup::recover_from_environment;
#[cfg(target_os = "linux")]
use submit::{TaskRegistrySettings, TaskStartTime, TaskStartTimeSource};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Preflight(#[from] PreflightError),
    #[error(transparent)]
    Cgroup(#[from] CgroupError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error("TaskCage 작업 실행에는 Linux cgroup v2가 필요합니다")]
    UnsupportedPlatform,
    #[error("잘못된 인자입니다: {0}")]
    InvalidArgument(String),
    #[error(transparent)]
    RuntimePackage(#[from] runtime_package::RuntimePackageError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Bundle(#[from] bundle::BundleError),
    #[error("실행 결과를 JSON으로 바꾸지 못했습니다")]
    Serialize(#[from] serde_json::Error),
    #[error("운영체제 종료 신호를 처리하지 못했습니다")]
    Signal(#[source] io::Error),
    #[error("작업 {stage} 단계가 실패했습니다: {cause}; 정리 오류={cleanup_errors:?}")]
    RunFailed {
        stage: &'static str,
        cause: String,
        cleanup_errors: Vec<String>,
    },
    #[error("이전 작업의 격리 정리를 확인하지 못해 새 작업을 실행할 수 없습니다")]
    CleanupUncertain,
    #[error("작업 lifecycle 결과를 만들지 못했습니다: {0}")]
    TaskLifecycle(String),
    #[error("UDS 서버가 실패했습니다: {0}")]
    Server(String),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Status(#[from] status::StatusError),
    #[cfg(target_os = "linux")]
    #[error("실행 중인 daemon이 cgroup v2 준비 상태가 아닙니다")]
    DaemonUnready,
    #[cfg(target_os = "linux")]
    #[error("daemon 시작 복구가 실패했습니다: {0}")]
    Startup(String),
    #[error("정리 불확실 fail-stop으로 daemon을 종료합니다: taskId={task_id}, stage={stage}")]
    FailStop { task_id: String, stage: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
/// 작업 하나를 실행하는 데 필요한 제한과 명령이다.
pub struct RunOnceConfig {
    pub cgroup_root: Option<PathBuf>,
    pub job_id: String,
    pub limits: CgroupLimits,
    pub wall_timeout: Duration,
    pub cleanup_timeout: Duration,
    pub capture_limits: CaptureLimits,
    pub working_directory: PathBuf,
    /// target에 명시적으로 전달할 환경이다. daemon process 환경은 자동으로 합치지 않는다.
    pub environment: BTreeMap<OsString, OsString>,
    /// 첫 값은 실행 파일이며 나머지는 셸 해석 없이 그대로 넘길 인자다.
    pub command: Vec<OsString>,
}

#[derive(Debug, Clone)]
/// 서비스 daemon이 사용할 명시적 socket과 내부 실행 설정이다.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct DaemonConfig {
    socket_path: PathBuf,
    max_concurrent_tasks: u32,
    max_registry_tasks: usize,
    max_concurrent_connections: NonZeroUsize,
    cleanup_timeout: Duration,
    fail_stop_timeout: Duration,
    deployment_policy: deployment_policy::DeploymentResourcePolicy,
    local_profile: Option<LocalProfileConfig>,
    remote: Option<remote_config::RemoteDaemonConfig>,
}

/// 명시적으로 활성화한 v0.2 test Profile의 daemon-owned Artifact root 설정이다.
#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct LocalProfileConfig {
    artifact_root: PathBuf,
    maximum_artifact_bytes: u64,
    ffmpeg_audio_to_wav: Option<FfmpegRuntimePackageConfig>,
    bundle_cache_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct FfmpegRuntimePackageConfig {
    cache_root: PathBuf,
    digest: digest::Sha256Digest,
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
        let max_registry_tasks = NonZeroUsize::new(max_registry_tasks).ok_or_else(|| {
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
        let max_concurrent_connections =
            NonZeroUsize::new(max_concurrent_connections).ok_or_else(|| {
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
        let deployment_policy = deployment_policy::DeploymentResourcePolicy::try_new(
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
        digest: digest::Sha256Digest,
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
        let remote = remote_config::RemoteDaemonConfig::load(&path)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        self.remote = Some(remote);
        Ok(self)
    }
}

/// 개발용 `run-once` CLI 진단 결과다. protocol v1의 `task` wire payload가 아니다.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOnceReport {
    pub job_id: String,
    pub pid: i32,
    pub membership_verified: bool,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stats: JobStats,
    pub output: TaskOutput,
    pub cleanup_complete: bool,
}

/// 명시적 UDS 설정으로 protocol v1 daemon을 실행한다.
#[cfg(target_os = "linux")]
pub async fn run(config: DaemonConfig) -> Result<()> {
    let startup = StartupOwnership::acquire(&config.socket_path)
        .map_err(|error| Error::Startup(error.to_string()))?;
    let capacity_settings = TaskCapacitySettings::new(config.max_concurrent_tasks)
        .map_err(|error| Error::InvalidArgument(error.to_string()))?;
    let registry_settings = TaskRegistrySettings::new(config.max_registry_tasks)
        .map_err(|error| Error::InvalidArgument(error.to_string()))?;
    let environment = run_startup_steps(
        || match recover_from_environment(config.cleanup_timeout) {
            Ok(report) => {
                tracing::info!(
                    event = "startup_recovery_completed",
                    removed_jobs = report.removed_jobs,
                    "잔여 TaskCage 작업 cgroup 복구를 완료했습니다"
                );
                Ok(report)
            }
            Err(error) => {
                tracing::error!(
                    event = "startup_recovery_failed",
                    stage = error.stage(),
                    "잔여 TaskCage 작업 cgroup 복구에 실패했습니다"
                );
                Err(Error::Startup(error.to_string()))
            }
        },
        |report| {
            SystemProbe::from_environment()
                .check_after_recovery(report.placement)
                .map_err(Error::from)
        },
    )?;
    let fail_stop = FailStopCoordinator::new(
        FailStopSettings::new(config.fail_stop_timeout)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?,
    );
    tracing::info!(
        event = "preflight_completed",
        cgroup_v2_ready = true,
        "cgroup 사전 검사를 통과했습니다"
    );
    let local_profile = config
        .local_profile
        .as_ref()
        .map(|settings| {
            let ffmpeg_registration = settings
                .ffmpeg_audio_to_wav
                .as_ref()
                .map(|registration| (registration.cache_root.as_path(), registration.digest));
            profile::LocalProfileRuntime::open(
                &settings.artifact_root,
                settings.maximum_artifact_bytes,
                config.deployment_policy.maximum().clone(),
                ffmpeg_registration,
                settings.bundle_cache_root.as_deref(),
            )
            .map_err(|error| {
                Error::InvalidArgument(format!("local profile 설정이 안전하지 않습니다: {error}"))
            })
        })
        .transpose()?;
    let handlers = Arc::new(ProtocolHandlers::initialize(
        Ok(environment),
        capacity_settings,
        registry_settings,
        config.deployment_policy,
        fail_stop,
        local_profile,
    )?);
    tracing::info!(event = "daemon_started", "TaskCage daemon started");
    let result = if let Some(remote) = config.remote {
        if config.local_profile.is_none() {
            return Err(Error::InvalidArgument(
                "Remote listener에는 daemon-installed Profile 설정이 필요합니다".to_owned(),
            ));
        }
        let artifacts = remote_artifact::RemoteArtifactStore::open(
            &remote.artifact_root,
            remote.max_artifact_bytes.get(),
            remote.max_artifact_chunk_bytes.get(),
            remote.artifact_retention,
        )
        .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        let credentials = remote_auth::CredentialStore::new(remote.principals.clone());
        let backend = Arc::new(remote_backend::LocalProfileRemoteBackend::new(
            Arc::clone(&handlers),
            config.cleanup_timeout,
        ));
        let dispatcher = Arc::new(remote_dispatch::RemoteDispatcher::new(artifacts, backend));
        serve_local_and_remote(
            startup,
            config.cleanup_timeout,
            config.max_concurrent_connections,
            handlers,
            Arc::new(remote),
            credentials,
            dispatcher,
        )
        .await
    } else {
        server::serve_protocol_until(
            startup,
            config.cleanup_timeout,
            config.max_concurrent_connections,
            handlers,
            shutdown_signal(),
        )
        .await
        .map_err(map_local_server_error)
    };
    if result.is_ok() {
        tracing::info!(
            event = "daemon_stopped",
            outcome = "CLEAN",
            "TaskCage daemon stopped"
        );
    } else {
        tracing::error!(
            event = "daemon_stopped",
            outcome = "ERROR",
            "TaskCage daemon stopped with an error"
        );
    }
    result
}

#[cfg(target_os = "linux")]
#[allow(
    clippy::too_many_arguments,
    reason = "local과 Remote listener의 명시적 runtime ownership이다"
)]
async fn serve_local_and_remote(
    startup: StartupOwnership,
    cleanup_timeout: Duration,
    max_local_connections: NonZeroUsize,
    handlers: Arc<ProtocolHandlers<submit::SubmitCoordinator>>,
    remote: Arc<remote_config::RemoteDaemonConfig>,
    credentials: remote_auth::CredentialStore,
    dispatcher: Arc<remote_dispatch::RemoteDispatcher<remote_backend::LocalProfileRemoteBackend>>,
) -> Result<()> {
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let local_shutdown_receiver = shutdown_receiver.clone();
    let local_shutdown = async move {
        wait_for_listener_shutdown(local_shutdown_receiver).await;
        Ok(())
    };
    let remote_shutdown = wait_for_listener_shutdown(shutdown_receiver.clone());
    let local = server::serve_protocol_until(
        startup,
        cleanup_timeout,
        max_local_connections,
        Arc::clone(&handlers),
        local_shutdown,
    );
    let reload = reload_remote_credentials(
        remote.source_path.clone(),
        credentials.clone(),
        shutdown_receiver,
    );
    let remote_listener =
        remote_server::serve_remote_until(remote, credentials, dispatcher, remote_shutdown);
    tokio::pin!(local);
    tokio::pin!(remote_listener);
    tokio::pin!(reload);
    enum First {
        Local(std::result::Result<(), server::ServerError>),
        Remote(std::result::Result<(), remote_server::RemoteServerError>),
        Reload(std::result::Result<(), io::Error>),
        Signal(std::result::Result<(), io::Error>),
    }
    let first = tokio::select! {
        result = &mut local => First::Local(result),
        result = &mut remote_listener => First::Remote(result),
        result = &mut reload => First::Reload(result),
        result = shutdown_signal() => First::Signal(result),
    };
    let _ = shutdown_sender.send(true);
    let (local_result, remote_result) = match first {
        First::Local(local_result) => (local_result, remote_listener.await),
        First::Remote(remote_result) => (local.await, remote_result),
        First::Reload(reload_result) => {
            reload_result.map_err(Error::Signal)?;
            tokio::join!(&mut local, &mut remote_listener)
        }
        First::Signal(signal) => {
            signal.map_err(Error::Signal)?;
            tokio::join!(&mut local, &mut remote_listener)
        }
    };
    local_result.map_err(map_local_server_error)?;
    remote_result.map_err(|error| Error::Server(error.to_string()))?;
    Ok(())
}

#[cfg(target_os = "linux")]
async fn reload_remote_credentials(
    path: PathBuf,
    credentials: remote_auth::CredentialStore,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = hangup.recv() => {
                match remote_config::RemoteDaemonConfig::load(&path) {
                    Ok(config) => {
                        credentials.replace_all(config.principals);
                        tracing::info!(event = "remote_credentials_reloaded", "Remote principal 설정을 다시 읽었습니다");
                    }
                    Err(error) => {
                        tracing::error!(event = "remote_credentials_reload_failed", cause = %error, "Remote principal 설정 reload에 실패했습니다");
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
async fn wait_for_listener_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(target_os = "linux")]
fn map_local_server_error(error: server::ServerError) -> Error {
    match error {
        server::ServerError::FailStop { task_id, stage } => Error::FailStop { task_id, stage },
        other => Error::Server(other.to_string()),
    }
}

#[cfg(any(target_os = "linux", test))]
fn run_startup_steps<R, T>(
    recover: impl FnOnce() -> Result<R>,
    preflight: impl FnOnce(R) -> Result<T>,
) -> Result<T> {
    let recovered = recover()?;
    preflight(recovered)
}

#[cfg(not(target_os = "linux"))]
pub async fn run(_config: DaemonConfig) -> Result<()> {
    Err(Error::UnsupportedPlatform)
}

/// 서비스 시작 전 cgroup 기능과 권한을 한 번에 확인한다.
pub fn check_environment() -> std::result::Result<CapabilityReport, PreflightError> {
    Ok(SystemProbe::from_environment().check()?.report().clone())
}

#[cfg(target_os = "linux")]
pub async fn run_once(config: RunOnceConfig) -> Result<RunOnceReport> {
    let RunOnceConfig {
        cgroup_root,
        job_id,
        limits,
        wall_timeout,
        cleanup_timeout,
        capture_limits,
        working_directory,
        environment,
        command,
    } = config;
    // 요청 검증은 preflight의 manager cgroup 생성이나 target side effect보다 먼저 끝낸다.
    let prepared = PreparedCommand::new(command, &working_directory, environment)?;
    let probe = match &cgroup_root {
        Some(root) => SystemProbe::with_root(root),
        None => SystemProbe::from_environment(),
    };
    let environment = probe.check()?;
    let manager = CgroupManager::initialize(environment)?;
    tracing::info!(
        event = "run_once_preflight_completed",
        cgroup_v2_ready = true,
        "검증된 cgroup 작업 영역을 준비했습니다"
    );

    let (cancellation, _unused_cancel_handle) = cancellation_channel();
    let cleaned = execute(
        &manager,
        ExecutionConfig {
            job_id,
            limits,
            wall_timeout,
            cleanup_timeout,
            capture_limits,
            prepared,
        },
        cancellation,
        None,
        TaskStartTimeSource::new(|| TaskStartTime::new(String::new(), std::time::Instant::now())),
        |_, _| {},
    )
    .await
    .map_err(|failure| failure.into_error())?;
    let diagnostic = cleaned.into_diagnostic_parts();
    if let Some(errno) = diagnostic.exec_errno {
        return Err(Error::RunFailed {
            stage: "프로세스 생성",
            cause: ExecutorError::Exec(errno).to_string(),
            cleanup_errors: Vec::new(),
        });
    }
    let output = diagnostic.output.into_task_output();
    Ok(RunOnceReport {
        job_id: diagnostic.job_id,
        pid: diagnostic.pid,
        membership_verified: diagnostic.membership_verified,
        timed_out: diagnostic.timed_out,
        exit_code: diagnostic.exit_code,
        signal: diagnostic.signal,
        stats: diagnostic.stats,
        output,
        cleanup_complete: true,
    })
}

#[cfg(not(target_os = "linux"))]
pub async fn run_once(_config: RunOnceConfig) -> Result<RunOnceReport> {
    Err(Error::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(test)]
async fn run_until<F>(shutdown: F) -> io::Result<()>
where
    F: Future<Output = io::Result<()>>,
{
    tracing::info!("TaskCage daemon started");
    shutdown.await?;
    tracing::info!("TaskCage daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[tokio::test]
    async fn stops_after_shutdown_signal() {
        run_until(async { Ok(()) }).await.unwrap();
    }

    #[tokio::test]
    async fn returns_shutdown_signal_errors() {
        let error = run_until(async { Err(io::Error::other("signal failed")) })
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn startup_recovery_must_finish_before_preflight() {
        let order = RefCell::new(Vec::new());
        let result = run_startup_steps(
            || {
                order.borrow_mut().push("recovery");
                Ok(7)
            },
            |recovered| {
                assert_eq!(recovered, 7);
                order.borrow_mut().push("preflight");
                Ok(42)
            },
        )
        .unwrap();

        assert_eq!(result, 42);
        assert_eq!(order.into_inner(), ["recovery", "preflight"]);
    }

    #[test]
    fn injected_startup_recovery_failures_block_preflight_and_listener_preparation() {
        for stage in ["cgroup.kill", "populated 0", "cgroup 제거"] {
            let preflight_called = RefCell::new(false);
            let result = run_startup_steps::<(), ()>(
                || {
                    Err(Error::InvalidArgument(format!(
                        "injected startup {stage} failure"
                    )))
                },
                |_| {
                    *preflight_called.borrow_mut() = true;
                    Ok(())
                },
            );

            assert!(matches!(result, Err(Error::InvalidArgument(_))));
            assert!(!preflight_called.into_inner());
        }
    }
}
