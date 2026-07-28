//! TaskCage Rust 데몬의 사전 검사와 작업 실행 생명주기를 제공한다.

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
pub mod protocol;
pub mod resource_budget;
#[cfg(target_os = "linux")]
mod runner;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod startup;
#[cfg(target_os = "linux")]
mod startup_cgroup;
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
use protocol::TaskOutput;
#[cfg(target_os = "linux")]
use runner::{ExecutionConfig, execute};
use serde::Serialize;
#[cfg(target_os = "linux")]
use startup::StartupOwnership;
#[cfg(target_os = "linux")]
use startup_cgroup::recover_from_environment;
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
    cleanup_timeout: Duration,
    fail_stop_timeout: Duration,
}

impl DaemonConfig {
    pub fn new(
        socket_path: PathBuf,
        max_concurrent_tasks: u32,
        cleanup_timeout: Duration,
        fail_stop_timeout: Duration,
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
        if cleanup_timeout.is_zero() {
            return Err(Error::InvalidArgument(
                "cleanup-timeout-ms 값은 0보다 커야 합니다".to_owned(),
            ));
        }
        FailStopSettings::new(fail_stop_timeout)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        Ok(Self {
            socket_path,
            max_concurrent_tasks,
            cleanup_timeout,
            fail_stop_timeout,
        })
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
    let environment = run_startup_steps(
        || match recover_from_environment(config.cleanup_timeout) {
            Ok(report) => {
                tracing::info!(
                    removed_jobs = report.removed_jobs,
                    "잔여 TaskCage 작업 cgroup 복구를 완료했습니다"
                );
                Ok(report)
            }
            Err(error) => {
                tracing::error!(
                    stage = error.stage(),
                    remaining_path = %error.remaining_path().display(),
                    error = %error,
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
        root = %environment.report().delegated_root.display(),
        manager = %environment.report().manager_cgroup.display(),
        "cgroup 사전 검사를 통과했습니다"
    );
    let handlers = Arc::new(ProtocolHandlers::initialize(
        Ok(environment),
        capacity_settings,
        fail_stop,
    )?);
    tracing::info!(socket = %config.socket_path.display(), "TaskCage daemon started");
    let result =
        server::serve_protocol_until(startup, config.cleanup_timeout, handlers, shutdown_signal())
            .await
            .map_err(|error| match error {
                server::ServerError::FailStop { task_id, stage } => {
                    Error::FailStop { task_id, stage }
                }
                other => Error::Server(other.to_string()),
            });
    if result.is_ok() {
        tracing::info!(socket = %config.socket_path.display(), "TaskCage daemon stopped");
    } else {
        tracing::error!(socket = %config.socket_path.display(), "TaskCage daemon stopped with an error");
    }
    result
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
    tracing::info!(root = %manager.root().display(), "검증된 cgroup 작업 영역을 준비했습니다");

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
