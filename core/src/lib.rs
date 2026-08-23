//! Backend-independent TaskCage execution contract.
//!
//! `taskcage-core` owns values that describe an immutable Capsule execution. The daemon and the
//! private embedded helper are adapters around the execution implementation that will be moved into
//! this crate incrementally. Transport, host admission policy and process supervision do not belong
//! in this crate.

pub mod artifact;
pub mod capsule;
mod execution;
pub mod policy;
pub mod task;

#[cfg(target_os = "linux")]
pub mod cgroup;
#[cfg(target_os = "linux")]
pub mod cleanup_fault;
#[cfg(target_os = "linux")]
pub mod deadline;
#[cfg(target_os = "linux")]
pub mod executor;
pub mod output;
pub mod preflight;

pub use capsule::{CapsuleIdentity, IdentityError, is_valid_capsule_name};
pub use execution::{ExecutionCommand, ExecutionExecutable};
