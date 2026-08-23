//! TaskCage Rust 데몬의 사전 검사와 작업 실행 생명주기를 제공한다.

pub mod artifact;
mod bootstrap;
pub use bootstrap::{DaemonConfig, DeploymentResourceMaximum, LocalProfileConfig, run};
#[cfg(target_os = "linux")]
pub use taskcage_linux_runtime::cleanup_fault;
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
#[cfg(target_os = "linux")]
pub mod capsule;
pub use taskcage_linux_runtime::cgroup;
pub mod codec;
pub(crate) use taskcage_linux_runtime::deadline;
mod deployment_policy;
pub mod digest;
mod execution_plan;
#[cfg(target_os = "linux")]
pub(crate) use taskcage_linux_runtime::executor;
mod fail_stop;
#[cfg(any(target_os = "linux", test))]
mod handlers;
#[cfg_attr(
    not(any(target_os = "linux", test)),
    allow(dead_code, reason = "protocol task lifecycle은 Linux에서만 제공됩니다")
)]
mod lifecycle;
pub mod output {
    pub use taskcage_core::output::{CaptureLimits, CapturedOutput, CapturedStream};

    use taskcage_core::task::TaskOutput;

    pub(crate) fn into_task_output(captured: CapturedOutput) -> TaskOutput {
        let CapturedOutput { stdout, stderr } = captured;
        TaskOutput {
            stdout_tail: String::from_utf8_lossy(stdout.raw_tail()).into_owned(),
            stderr_tail: String::from_utf8_lossy(stderr.raw_tail()).into_owned(),
            stdout_truncated: stdout.truncated(),
            stderr_truncated: stderr.truncated(),
        }
    }
}
pub use taskcage_linux_runtime::preflight;
#[cfg(target_os = "linux")]
mod profile;
#[cfg(target_os = "linux")]
pub mod profile_invocation;
#[cfg(target_os = "linux")]
pub mod profile_mapper;
#[cfg(target_os = "linux")]
mod profile_registry;
pub mod protocol;
mod protocol_mapper;
pub mod remote_artifact;
pub mod remote_auth;
#[cfg(target_os = "linux")]
mod remote_backend;
pub mod remote_config;
pub mod remote_dispatch;
pub mod remote_protocol;
#[cfg(target_os = "linux")]
mod remote_protocol_mapper;
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
use taskcage_linux_runtime::cgroup::recovery as startup_cgroup;
#[cfg(target_os = "linux")]
pub mod status;
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "protocol task 실행은 Linux에서만 제공됩니다")
)]
mod submit;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(target_os = "linux")]
use cancellation::cancellation_channel;
#[cfg(target_os = "linux")]
use cgroup::CgroupManager;
use cgroup::{CgroupError, CgroupLimits, JobStats};
#[cfg(target_os = "linux")]
use executor::{ExecutorError, PreparedCommand};
use output::CaptureLimits;
use preflight::{CapabilityProbe, CapabilityReport, PreflightError, SystemProbe};
use protocol::TaskOutput;
#[cfg(target_os = "linux")]
use runner::{ExecutionConfig, execute};
use serde::Serialize;
#[cfg(target_os = "linux")]
use submit::{TaskStartTime, TaskStartTimeSource};
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
    let output = protocol_mapper::task_output(output::into_task_output(diagnostic.output));
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
