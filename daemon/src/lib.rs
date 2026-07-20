//! TaskCage의 Rust 시스템 데몬이다.
//!
//! 현재 구현된 첫 실행 경로인 `run_once`는 위임받은 cgroup에 제한을 설정하고,
//! 대상 프로세스를 처음부터 그 안에 만든다. 제한 시간을 넘기거나 오류가 생기면
//! 작업 cgroup 전체를 정리한다. 여러 요청을 받는 소켓 통신과 대기열은 이후에 이 위에 붙는다.

pub mod cgroup;
#[cfg(target_os = "linux")]
pub mod executor;
pub mod monitor;
pub mod protocol;
pub mod scheduler;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(target_os = "linux")]
use cgroup::CgroupManager;
use cgroup::{CgroupError, CgroupLimits, JobStats};
#[cfg(target_os = "linux")]
use executor::{ExecutorError, PreparedCommand, WaitOutcome, spawn_in_cgroup};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Cgroup(#[from] CgroupError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error("TaskCage execution requires Linux with delegated cgroup v2")]
    UnsupportedPlatform,
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("serialize run result failed")]
    Serialize(#[from] serde_json::Error),
    #[error("daemon signal handling failed")]
    Signal(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
/// 작업 하나를 실행하는 데 필요한 설정이다.
pub struct RunOnceConfig {
    /// 직접 지정한 cgroup 경로다. 값이 없으면 현재 데몬의 위임 경로를 자동으로 찾는다.
    pub cgroup_root: Option<PathBuf>,
    /// cgroup 디렉터리 이름과 로그에서 작업을 구분할 식별자다.
    pub job_id: String,
    /// 메모리, 프로세스 수, CPU 사용량의 상한이다.
    pub limits: CgroupLimits,
    /// 프로그램 실행을 기다릴 최대 시간이다.
    pub wall_timeout: Duration,
    /// 종료 요청 뒤 cgroup이 완전히 빌 때까지 기다릴 최대 시간이다.
    pub cleanup_timeout: Duration,
    pub working_directory: Option<PathBuf>,
    /// 첫 값은 실행 파일이고 나머지는 그대로 전달할 인자다. 셸 문자열로 해석하지 않는다.
    pub command: Vec<OsString>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// 작업이 끝난 뒤 호출자에게 돌려주는 실행 결과다.
pub struct RunOnceReport {
    pub job_id: String,
    pub pid: i32,
    pub membership_verified: bool,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stats: JobStats,
    pub cleanup_complete: bool,
}

#[cfg(target_os = "linux")]
pub async fn run_once(config: RunOnceConfig) -> Result<RunOnceReport> {
    // 위임 경로와 필요한 제어기를 모두 확인한 뒤에만 작업 생성을 시작한다.
    let manager = match &config.cgroup_root {
        Some(root) => CgroupManager::initialize(root)?,
        None => CgroupManager::discover_and_initialize()?,
    };
    tracing::info!(root = %manager.root().display(), "initialized delegated cgroup root");

    // 자원 제한값을 쓰고 다시 읽어 확인한다. 여기서 실패하면 외부 프로그램은 시작하지 않는다.
    let job = manager.create_job(&config.job_id, config.limits)?;
    tracing::info!(job_id = job.id(), path = %job.path().display(), "created job cgroup");

    let prepared = PreparedCommand::new(config.command, config.working_directory.as_deref())?;
    let process = match spawn_in_cgroup(&prepared, job.raw_fd()) {
        Ok(process) => process,
        Err(error) => {
            // 프로세스를 만들지 못했더라도 앞에서 만든 작업 cgroup은 남기지 않는다.
            let _ = job.finish(config.cleanup_timeout).await;
            return Err(error.into());
        }
    };
    tracing::info!(job_id = %config.job_id, pid = process.pid(), "started target in job cgroup");
    // `clone3`에 넘긴 cgroup이 실제로 적용됐는지 커널의 프로세스 목록으로 확인한다.
    let membership_verified = match job.contains_pid(process.pid()) {
        Ok(verified) => verified,
        Err(error) => {
            // 확인 과정 자체가 실패하면 보호 여부를 알 수 없으므로 작업 전체를 안전하게 끝낸다.
            let _ = job.kill_all();
            let _ = process.reap_after_kill(config.cleanup_timeout).await;
            let _ = job.finish(config.cleanup_timeout).await;
            return Err(error.into());
        }
    };
    if !membership_verified {
        tracing::warn!(
            job_id = %config.job_id,
            pid = process.pid(),
            "target exited before membership could be observed"
        );
    }

    let wait_outcome = match process.wait_for(config.wall_timeout).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // 기다리는 과정에서 오류가 나도 자식·손자 프로세스를 남기지 않는다.
            let _ = job.kill_all();
            let _ = process.reap_after_kill(config.cleanup_timeout).await;
            let _ = job.finish(config.cleanup_timeout).await;
            return Err(error.into());
        }
    };
    let (timed_out, exit) = match wait_outcome {
        WaitOutcome::Exited(exit) => (false, exit),
        WaitOutcome::TimedOut => {
            // 대표 PID만 종료하면 그 프로그램이 만든 자식이 살아남을 수 있으므로 cgroup 전체를 끝낸다.
            tracing::warn!(job_id = %config.job_id, pid = process.pid(), "wall timeout; killing cgroup");
            if let Err(error) = job.kill_all() {
                let _ = job.finish(config.cleanup_timeout).await;
                return Err(error.into());
            }
            let exit = match process.reap_after_kill(config.cleanup_timeout).await {
                Ok(exit) => exit,
                Err(error) => {
                    let _ = job.finish(config.cleanup_timeout).await;
                    return Err(error.into());
                }
            };
            (true, exit)
        }
    };

    // 모든 프로세스가 사라졌는지 확인하고 통계를 읽은 다음 작업 cgroup을 제거한다.
    let stats = job.finish(config.cleanup_timeout).await?;
    tracing::info!(job_id = %config.job_id, "job cgroup is empty and removed");
    Ok(RunOnceReport {
        job_id: config.job_id,
        pid: process.pid(),
        membership_verified,
        timed_out,
        exit_code: exit.exit_code,
        signal: exit.signal,
        stats,
        cleanup_complete: true,
    })
}

#[cfg(not(target_os = "linux"))]
/// cgroup v2를 사용할 수 없는 운영체제에서는 보호되지 않은 실행을 허용하지 않는다.
pub async fn run_once(_config: RunOnceConfig) -> Result<RunOnceReport> {
    Err(Error::UnsupportedPlatform)
}

/// 소켓 서버가 구현되기 전까지 systemd 서비스가 종료되지 않도록 기다린다.
pub async fn run() -> Result<()> {
    tracing::info!("TaskCage daemon started; use run-once for the cgroup MVP slice");
    tokio::signal::ctrl_c().await.map_err(Error::Signal)
}
