//! 검증된 Runtime Package를 digest 기준으로 import하고 다시 여는 로컬 cache다.
//!
//! Import는 manifest와 전체 file set을 먼저 검증한 뒤 같은 filesystem의 staging directory를
//! no-overwrite rename으로 활성화한다. Task 실행 경로는 digest로 다시 검증한 file descriptor만 받는다.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod manifest;

#[cfg(target_os = "linux")]
mod linux_cache;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::digest::Sha256Digest;

pub use manifest::{
    PackageFile, PackageLicense, PackageSbom, RuntimeLibc, RuntimePackageManifest, RuntimePlatform,
};

#[cfg(target_os = "linux")]
pub use linux_cache::RuntimePackageCache;

pub type RuntimePackageResult<T> = std::result::Result<T, RuntimePackageError>;

#[derive(Debug, Error)]
pub enum RuntimePackageError {
    #[error("Runtime Package manifest가 잘못되었습니다: {0}")]
    InvalidManifest(String),
    #[error("Runtime Package source layout이 잘못되었습니다: {0}")]
    InvalidSource(String),
    #[error("Runtime Package가 현재 host와 호환되지 않습니다: {0}")]
    IncompatiblePlatform(String),
    #[error("Runtime Package content 검증에 실패했습니다: {0}")]
    Integrity(String),
    #[error("Runtime Package cache root가 안전하지 않습니다: {0}")]
    UnsafeCacheRoot(PathBuf),
    #[error("현재 platform은 Runtime Package cache를 지원하지 않습니다")]
    UnsupportedPlatform,
    #[error("Runtime Package filesystem 작업 {operation}에 실패했습니다: {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("filesystem이 no-overwrite atomic activation을 지원하지 않습니다: {0}")]
    AtomicActivationUnavailable(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImportOutcome {
    Imported,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub digest: Sha256Digest,
    pub outcome: ImportOutcome,
}

/// 검증된 cache entry와 실행 시점까지 inode를 고정하는 descriptor다.
#[derive(Debug)]
pub struct ResolvedRuntimePackage {
    digest: Sha256Digest,
    manifest: RuntimePackageManifest,
    rootfs: File,
    entrypoint: File,
}

impl ResolvedRuntimePackage {
    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn manifest(&self) -> &RuntimePackageManifest {
        &self.manifest
    }

    pub fn rootfs(&self) -> &File {
        &self.rootfs
    }

    pub fn entrypoint(&self) -> &File {
        &self.entrypoint
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct RuntimePackageCache;

#[cfg(not(target_os = "linux"))]
impl RuntimePackageCache {
    pub fn open(_root: &Path) -> RuntimePackageResult<Self> {
        Err(RuntimePackageError::UnsupportedPlatform)
    }

    pub fn import(&self, _source: &Path) -> RuntimePackageResult<ImportReport> {
        Err(RuntimePackageError::UnsupportedPlatform)
    }

    pub fn resolve(&self, _digest: Sha256Digest) -> RuntimePackageResult<ResolvedRuntimePackage> {
        Err(RuntimePackageError::UnsupportedPlatform)
    }
}

/// daemon과 같은 service UID로 cache에 import하는 CLI 경계다.
pub fn import_for_service_uid(
    cache_root: &Path,
    source: &Path,
) -> RuntimePackageResult<ImportReport> {
    #[cfg(target_os = "linux")]
    {
        RuntimePackageCache::open(cache_root)?.import(source)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cache_root, source);
        Err(RuntimePackageError::UnsupportedPlatform)
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn non_linux_cache_fails_closed() {
        assert!(matches!(
            RuntimePackageCache::open(Path::new("C:\\taskcage")),
            Err(RuntimePackageError::UnsupportedPlatform)
        ));
    }
}
