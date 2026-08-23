//! Filesystem을 사용하는 outbound adapter다.

pub(crate) mod artifact_filesystem;
#[cfg(target_os = "linux")]
pub(crate) mod bundle_catalog;
pub(crate) mod remote_artifact_filesystem;
#[cfg(target_os = "linux")]
pub(crate) mod runtime_package_cache;
