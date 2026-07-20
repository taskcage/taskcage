//! Atomic `clone3(CLONE_INTO_CGROUP)` target creation and `execve` boundary.
//!
//! The child-side path must remain allocation-free and async-signal-safe after
//! clone. Arguments, environment and file-descriptor actions are prepared by
//! the parent before process creation.
