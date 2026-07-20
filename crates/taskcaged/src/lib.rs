//! TaskCage's Rust system daemon.
//!
//! The daemon owns admission control, the delegated cgroup v2 subtree,
//! atomic target creation, monitoring, classification, cleanup and the local
//! protocol used by the Java SDK.

pub mod cgroup;
pub mod executor;
pub mod monitor;
pub mod protocol;
pub mod scheduler;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("taskcaged implementation is not complete")]
    NotImplemented,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Starts the daemon after platform preflight succeeds.
pub async fn run() -> Result<()> {
    tracing::info!("TaskCage daemon scaffold initialized");
    Ok(())
}
