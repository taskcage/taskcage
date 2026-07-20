//! TaskCage's Rust system daemon.
//!
//! The first runnable slice is `run_once`: it configures a delegated cgroup,
//! atomically creates one target inside it, enforces a wall timeout and proves
//! whole-cgroup cleanup. UDS admission control is layered on this core later.

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
pub struct RunOnceConfig {
    pub cgroup_root: Option<PathBuf>,
    pub job_id: String,
    pub limits: CgroupLimits,
    pub wall_timeout: Duration,
    pub cleanup_timeout: Duration,
    pub working_directory: Option<PathBuf>,
    pub command: Vec<OsString>,
}

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
    pub cleanup_complete: bool,
}

#[cfg(target_os = "linux")]
pub async fn run_once(config: RunOnceConfig) -> Result<RunOnceReport> {
    let manager = match &config.cgroup_root {
        Some(root) => CgroupManager::initialize(root)?,
        None => CgroupManager::discover_and_initialize()?,
    };
    tracing::info!(root = %manager.root().display(), "initialized delegated cgroup root");

    let job = manager.create_job(&config.job_id, config.limits)?;
    tracing::info!(job_id = job.id(), path = %job.path().display(), "created job cgroup");

    let prepared = PreparedCommand::new(config.command, config.working_directory.as_deref())?;
    let process = match spawn_in_cgroup(&prepared, job.raw_fd()) {
        Ok(process) => process,
        Err(error) => {
            let _ = job.finish(config.cleanup_timeout).await;
            return Err(error.into());
        }
    };
    tracing::info!(job_id = %config.job_id, pid = process.pid(), "started target in job cgroup");
    let membership_verified = match job.contains_pid(process.pid()) {
        Ok(verified) => verified,
        Err(error) => {
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
            let _ = job.kill_all();
            let _ = process.reap_after_kill(config.cleanup_timeout).await;
            let _ = job.finish(config.cleanup_timeout).await;
            return Err(error.into());
        }
    };
    let (timed_out, exit) = match wait_outcome {
        WaitOutcome::Exited(exit) => (false, exit),
        WaitOutcome::TimedOut => {
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
pub async fn run_once(_config: RunOnceConfig) -> Result<RunOnceReport> {
    Err(Error::UnsupportedPlatform)
}

/// Keeps the systemd service alive while the UDS server is being implemented.
pub async fn run() -> Result<()> {
    tracing::info!("TaskCage daemon started; use run-once for the cgroup MVP slice");
    tokio::signal::ctrl_c().await.map_err(Error::Signal)
}
