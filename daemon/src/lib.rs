//! TaskCage Rust 데몬의 사전 검사와 작업 실행 생명주기를 제공한다.

pub mod capability;
pub mod cgroup;
pub mod codec;
#[cfg(target_os = "linux")]
mod executor;
pub mod output;
pub mod preflight;
pub mod protocol;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use cgroup::{CgroupError, CgroupLimits, JobStats};
#[cfg(target_os = "linux")]
use cgroup::{CgroupManager, JobCgroup};
#[cfg(target_os = "linux")]
use executor::{ExecutorError, PreparedCommand, SpawnedProcess, WaitOutcome, spawn_in_cgroup};
use output::CaptureLimits;
use preflight::{CapabilityProbe, CapabilityReport, PreflightError, SystemProbe};
use protocol::TaskOutput;
use serde::Serialize;
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

/// 지원 환경을 먼저 확인한 뒤 운영체제의 종료 신호가 올 때까지 데몬을 실행한다.
pub async fn run() -> Result<()> {
    let environment = SystemProbe::from_environment().check()?;
    let report = environment.report();
    tracing::info!(
        root = %report.delegated_root.display(),
        manager = %report.manager_cgroup.display(),
        "cgroup 사전 검사를 통과했습니다"
    );
    // 실제 소켓 서버가 붙으면 이 성공 값을 실행기 상태가 소유한다.
    let _environment = environment;
    run_until(shutdown_signal()).await.map_err(Error::Signal)
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

    let job = manager.create_job(&job_id, limits)?;
    tracing::info!(job_id = job.id(), path = %job.path().display(), "작업 cgroup을 만들었습니다");
    let process = match spawn_in_cgroup(&prepared, job.raw_fd(), capture_limits) {
        Ok(process) => process,
        Err(error) => {
            return Err(
                cleanup_job_after_failure(job, cleanup_timeout, "프로세스 생성", &error).await,
            );
        }
    };
    let pid = process.pid();
    tracing::info!(job_id = %job_id, pid = process.pid(), "target을 작업 cgroup 안에서 시작했습니다");

    let membership_verified = match job.contains_pid(process.pid()) {
        Ok(verified) => verified,
        Err(error) => {
            return Err(cleanup_running_job(
                job,
                process,
                cleanup_timeout,
                "cgroup 소속 재확인",
                &error,
            )
            .await);
        }
    };

    let wait_outcome = match process.wait_for(wall_timeout).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(cleanup_running_job(
                job,
                process,
                cleanup_timeout,
                "target 종료 대기",
                &error,
            )
            .await);
        }
    };
    let (timed_out, exit) = match wait_outcome {
        WaitOutcome::Exited(exit) => (false, exit),
        WaitOutcome::TimedOut => {
            if let Err(error) = job.kill_all() {
                return Err(cleanup_running_job(
                    job,
                    process,
                    cleanup_timeout,
                    "시간 초과 전체 종료",
                    &error,
                )
                .await);
            }
            let exit = match process.reap_after_kill(cleanup_timeout).await {
                Ok(exit) => exit,
                Err(error) => {
                    return Err(cleanup_running_job(
                        job,
                        process,
                        cleanup_timeout,
                        "시간 초과 종료 상태 회수",
                        &error,
                    )
                    .await);
                }
            };
            (true, exit)
        }
    };

    // 대표 프로세스가 정상 종료했어도 남은 자식과 손자를 먼저 cgroup 전체 종료한다.
    // 그래야 후손이 출력 FD를 들고 있어도 EOF를 무기한 기다리지 않는다.
    let finish_result = job.finish(cleanup_timeout).await;
    let output_result = process.finish_output(cleanup_timeout).await;
    let stats = match finish_result {
        Ok(stats) => stats,
        Err(error) => {
            let cleanup_errors = output_result
                .err()
                .map(|output_error| vec![output_error.to_string()])
                .unwrap_or_default();
            return Err(Error::RunFailed {
                stage: "작업 cgroup 정리",
                cause: error.to_string(),
                cleanup_errors,
            });
        }
    };
    let output = output_result?.into_task_output();
    Ok(RunOnceReport {
        job_id,
        pid,
        membership_verified,
        timed_out,
        exit_code: exit.exit_code,
        signal: exit.signal,
        stats,
        output,
        cleanup_complete: true,
    })
}

#[cfg(target_os = "linux")]
async fn cleanup_job_after_failure(
    job: JobCgroup,
    timeout: Duration,
    stage: &'static str,
    cause: &dyn std::fmt::Display,
) -> Error {
    let cleanup_errors = job
        .finish(timeout)
        .await
        .err()
        .map(|error| vec![error.to_string()])
        .unwrap_or_default();
    Error::RunFailed {
        stage,
        cause: cause.to_string(),
        cleanup_errors,
    }
}

#[cfg(target_os = "linux")]
async fn cleanup_running_job(
    job: JobCgroup,
    process: SpawnedProcess,
    timeout: Duration,
    stage: &'static str,
    cause: &dyn std::fmt::Display,
) -> Error {
    let mut cleanup_errors = Vec::new();
    if let Err(error) = job.kill_all() {
        cleanup_errors.push(error.to_string());
    }
    if let Err(error) = process.reap_after_kill(timeout).await {
        cleanup_errors.push(error.to_string());
    }
    if let Err(error) = job.finish(timeout).await {
        cleanup_errors.push(error.to_string());
    }
    if let Err(error) = process.finish_output(timeout).await {
        cleanup_errors.push(error.to_string());
    }
    Error::RunFailed {
        stage,
        cause: cause.to_string(),
        cleanup_errors,
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn run_once(_config: RunOnceConfig) -> Result<RunOnceReport> {
    Err(Error::UnsupportedPlatform)
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}

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
}
