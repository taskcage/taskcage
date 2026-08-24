//! Backend-independent TaskCage execution contract.
//!
//! `taskcage-core` owns values that describe an immutable Capsule execution. Transport, Linux host
//! admission, cgroup access and process supervision belong to adapters and `taskcage-linux-runtime`,
//! not this crate.

pub mod artifact;
pub mod capsule;
pub mod policy;
pub mod task;

pub mod output;

pub use capsule::{CapsuleIdentity, IdentityError, is_valid_capsule_name};
