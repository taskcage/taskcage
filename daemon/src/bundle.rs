//! Signed TaskCage Bundle archive validation and immutable local catalog.
//!
//! A Bundle references an already imported Runtime Package. It never contains executable bytes,
//! and it is activated only after archive, signature, package, and profile validation succeeds.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use flate2::read::GzDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

use crate::codec;
use crate::digest::Sha256Digest;
use crate::protocol::{OutputLimits, ResourceLimits};
use crate::runtime_package::{RuntimePackageCache, RuntimePackageError};

const BUNDLES_DIRECTORY: &str = "bundles";
const SHA256_DIRECTORY: &str = "sha256";
const CATALOG_DIRECTORY: &str = "catalog";
const BUNDLE_JSON: &str = "bundle.json";
const PROFILE_JSON: &str = "profile.json";
const CHECKSUMS: &str = "checksums.txt";
const SIGNATURE: &str = "signature.sig";
const BUNDLE_SCHEMA: &str = "taskcage.bundle/v0alpha1";
const PROFILE_SCHEMA: &str = "taskcage.profile/v0alpha1";
const MAX_ARCHIVE_BYTES: usize = 1_048_576;
const MAX_FILE_BYTES: usize = 262_144;
const RENAME_NOREPLACE: libc::c_uint = 1;
static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("Bundle archive가 잘못되었습니다: {0}")]
    Archive(String),
    #[error("Bundle manifest가 잘못되었습니다: {0}")]
    Manifest(String),
    #[error("Bundle signature 검증에 실패했습니다: {0}")]
    Signature(String),
    #[error("Bundle가 참조한 Runtime Package가 준비되지 않았습니다: {0}")]
    RuntimePackage(#[from] RuntimePackageError),
    #[error("Bundle cache root가 안전하지 않습니다: {0}")]
    UnsafeCacheRoot(PathBuf),
    #[error("Bundle cache 작업 {operation}에 실패했습니다: {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Bundle identity {name}@{version}는 이미 다른 immutable digest로 설치되어 있습니다")]
    IdentityConflict { name: String, version: String },
    #[error("설치된 Bundle을 찾을 수 없습니다: {name}@{version}")]
    NotFound { name: String, version: String },
    #[error("현재 platform은 Bundle catalog를 지원하지 않습니다")]
    UnsupportedPlatform,
}

pub type BundleResult<T> = std::result::Result<T, BundleError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedBundleKey {
    id: String,
    key: VerifyingKey,
}

impl TrustedBundleKey {
    pub fn from_base64(id: String, encoded: &str) -> BundleResult<Self> {
        validate_key_id(&id)?;
        let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(encoded.trim())
            .map_err(|error| {
                BundleError::Signature(format!("trusted key base64가 잘못되었습니다: {error}"))
            })?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
            BundleError::Signature(
                "trusted key는 base64로 표현한 32-byte Ed25519 public key여야 합니다".to_owned(),
            )
        })?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|error| {
            BundleError::Signature(format!(
                "trusted key가 Ed25519 public key가 아닙니다: {error}"
            ))
        })?;
        Ok(Self { id, key })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BundleImportOutcome {
    Imported,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleImportReport {
    pub digest: Sha256Digest,
    pub name: String,
    pub version: String,
    pub outcome: BundleImportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledBundle {
    pub digest: Sha256Digest,
    pub name: String,
    pub version: String,
    pub runtime_package_digest: Sha256Digest,
    pub runtime_package_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VerifiedBundleInspection;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleInspection {
    pub installed: InstalledBundle,
    pub manifest: BundleManifest,
    pub profile: BundleProfile,
    #[serde(skip)]
    _verified: VerifiedBundleInspection,
}

impl BundleInspection {
    pub fn installed(&self) -> &InstalledBundle {
        &self.installed
    }

    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    pub fn profile(&self) -> &BundleProfile {
        &self.profile
    }

    pub(crate) fn into_parts(self) -> (InstalledBundle, BundleManifest, BundleProfile) {
        (self.installed, self.manifest, self.profile)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleManifest {
    pub schema_version: String,
    pub name: String,
    pub version: String,
    pub signing_key_id: String,
    pub runtime: BundleRuntimeReference,
    pub profile_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleRuntimeReference {
    pub package_id: String,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleProfile {
    pub schema_version: String,
    pub name: String,
    pub version: String,
    pub inputs: Vec<BundleInput>,
    pub output: BundleOutput,
    pub argv: Vec<BundleProfileArgument>,
    pub policy: BundleResourcePolicy,
    pub allowed_overrides: Vec<String>,
}

/// Bundle-owned default task limits.  Keeping this wire shape identical to the
/// Profile API makes a Bundle executable without letting a caller supply an
/// unconstrained resource policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleResourcePolicy {
    pub limits: ResourceLimits,
    pub output: OutputLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleInput {
    pub name: String,
    pub kind: String,
    pub required: bool,
    #[serde(default)]
    pub allowed_values: Option<Vec<i64>>,
    #[serde(default)]
    pub minimum: Option<i64>,
    #[serde(default)]
    pub maximum: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleOutput {
    pub name: String,
    pub file_name: String,
    pub media_type: String,
    pub maximum_bytes: u64,
}

/// Profile manifest의 shell-free argv template을 load 시점에 고정한 표현이다.
///
/// custom serde 구현은 v0alpha1 JSON shape을 그대로 유지하면서 task 실행 경로가
/// `serde_json::Value`를 다시 해석하지 않도록 한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleProfileArgument {
    Literal(String),
    InputPath {
        slot: String,
    },
    InputValue {
        kind: BundleProfileInputValueKind,
        slot: String,
    },
    OutputPath {
        slot: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleProfileInputValueKind {
    String,
    Int64,
    Boolean,
}

impl Serialize for BundleProfileArgument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Literal(value) => value.serialize(serializer),
            Self::InputPath { slot } => BundleInputPathArgument {
                input: slot.clone(),
            }
            .serialize(serializer),
            Self::InputValue { kind, slot } => match kind {
                BundleProfileInputValueKind::String => BundleStringArgument {
                    string: slot.clone(),
                }
                .serialize(serializer),
                BundleProfileInputValueKind::Int64 => BundleInt64Argument {
                    int64: slot.clone(),
                }
                .serialize(serializer),
                BundleProfileInputValueKind::Boolean => BundleBooleanArgument {
                    boolean: slot.clone(),
                }
                .serialize(serializer),
            },
            Self::OutputPath { slot } => BundleOutputPathArgument {
                output: slot.clone(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for BundleProfileArgument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(
            match SerializedBundleProfileArgument::deserialize(deserializer)? {
                SerializedBundleProfileArgument::Literal(value) => Self::Literal(value),
                SerializedBundleProfileArgument::InputPath(argument) => Self::InputPath {
                    slot: argument.input,
                },
                SerializedBundleProfileArgument::String(argument) => Self::InputValue {
                    kind: BundleProfileInputValueKind::String,
                    slot: argument.string,
                },
                SerializedBundleProfileArgument::Int64(argument) => Self::InputValue {
                    kind: BundleProfileInputValueKind::Int64,
                    slot: argument.int64,
                },
                SerializedBundleProfileArgument::Boolean(argument) => Self::InputValue {
                    kind: BundleProfileInputValueKind::Boolean,
                    slot: argument.boolean,
                },
                SerializedBundleProfileArgument::OutputPath(argument) => Self::OutputPath {
                    slot: argument.output,
                },
            },
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SerializedBundleProfileArgument {
    Literal(String),
    InputPath(BundleInputPathArgument),
    String(BundleStringArgument),
    Int64(BundleInt64Argument),
    Boolean(BundleBooleanArgument),
    OutputPath(BundleOutputPathArgument),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleInputPathArgument {
    input: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleStringArgument {
    string: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleInt64Argument {
    int64: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleBooleanArgument {
    boolean: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleOutputPathArgument {
    output: String,
}

#[derive(Debug)]
pub struct BundleCatalog {
    root: PathBuf,
    bundle_sha256: PathBuf,
    catalog: PathBuf,
    device: u64,
}

impl BundleCatalog {
    pub fn open(cache_root: &Path) -> BundleResult<Self> {
        let root = validate_cache_root(cache_root)?;
        let bundles = ensure_child(cache_root, BUNDLES_DIRECTORY, root.dev())?;
        let bundle_sha256 = ensure_child(&bundles, SHA256_DIRECTORY, root.dev())?;
        let catalog = ensure_child(&bundles, CATALOG_DIRECTORY, root.dev())?;
        sync_directory(&bundle_sha256)?;
        sync_directory(&catalog)?;
        sync_directory(&bundles)?;
        sync_directory(cache_root)?;
        Ok(Self {
            root: cache_root.to_path_buf(),
            bundle_sha256,
            catalog,
            device: root.dev(),
        })
    }

    pub fn import(
        &self,
        source: &Path,
        keys: &[TrustedBundleKey],
    ) -> BundleResult<BundleImportReport> {
        self.import_with_identity_hook(source, keys, |_| Ok(()))
    }

    fn import_with_identity_hook<F>(
        &self,
        source: &Path,
        keys: &[TrustedBundleKey],
        identity_hook: F,
    ) -> BundleResult<BundleImportReport>
    where
        F: FnMut(IdentityActivationPoint) -> BundleResult<()>,
    {
        if !source.is_absolute() {
            return Err(BundleError::Archive(
                "source는 absolute path여야 합니다".to_owned(),
            ));
        }
        if keys.is_empty() {
            return Err(BundleError::Signature(
                "적어도 하나의 trusted key가 필요합니다".to_owned(),
            ));
        }
        let verified = VerifiedArchive::read(source, keys)?;
        let packages = RuntimePackageCache::open(&self.root)?;
        let package = packages.resolve(verified.bundle.runtime.digest)?;
        if package.manifest().id != verified.bundle.runtime.package_id {
            return Err(BundleError::Manifest(format!(
                "runtime.packageId가 referenced Package manifest와 다릅니다: expected={}, actual={}",
                verified.bundle.runtime.package_id,
                package.manifest().id
            )));
        }

        let digest = verified.digest;
        let mut staging = Staging::new(self.create_staging(digest)?);
        write_readonly(
            &staging.path.join(BUNDLE_JSON),
            &verified.bundle_raw,
            self.device,
        )?;
        write_readonly(
            &staging.path.join(PROFILE_JSON),
            &verified.profile_raw,
            self.device,
        )?;
        seal_directory(&staging.path)?;

        let entry = self.bundle_sha256.join(digest.hex());
        let outcome = match rename_no_replace(&staging.path, &entry) {
            Ok(()) => {
                staging.activated = true;
                sync_directory(&self.bundle_sha256)?;
                BundleImportOutcome::Imported
            }
            Err(BundleError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                self.inspect_by_digest(digest)?;
                staging.cleanup()?;
                BundleImportOutcome::AlreadyPresent
            }
            Err(error) => return Err(error),
        };
        self.activate_identity_with_hook(&verified.bundle, digest, identity_hook)?;
        Ok(BundleImportReport {
            digest,
            name: verified.bundle.name,
            version: verified.bundle.version,
            outcome,
        })
    }

    pub fn list(&self) -> BundleResult<Vec<InstalledBundle>> {
        let mut result = Vec::new();
        for name in fs::read_dir(&self.catalog)
            .map_err(|e| io_error("catalog 열거", self.catalog.clone(), e))?
        {
            let name = name.map_err(|e| io_error("catalog entry", self.catalog.clone(), e))?;
            let metadata = fs::symlink_metadata(name.path())
                .map_err(|e| io_error("catalog metadata", name.path(), e))?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.dev() != self.device
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o022 != 0
            {
                return Err(BundleError::UnsafeCacheRoot(name.path()));
            }
            let name_text = name.file_name().into_string().map_err(|_| {
                BundleError::Manifest("catalog identity는 UTF-8이어야 합니다".to_owned())
            })?;
            for version in fs::read_dir(name.path())
                .map_err(|e| io_error("catalog version 열거", name.path(), e))?
            {
                let version =
                    version.map_err(|e| io_error("catalog version entry", name.path(), e))?;
                let version_path = version.path();
                let version_metadata = fs::symlink_metadata(&version_path)
                    .map_err(|e| io_error("catalog version metadata", version_path.clone(), e))?;
                let version_text = version.file_name().into_string().map_err(|_| {
                    BundleError::Manifest("catalog version은 UTF-8이어야 합니다".to_owned())
                })?;
                if version_text.starts_with(".staging-") {
                    validate_identity_staging_entry(
                        &version_path,
                        &version_text,
                        &version_metadata,
                        self.device,
                    )?;
                    continue;
                }
                if !version_text.ends_with(".json") {
                    return Err(BundleError::Manifest(
                        "catalog에는 .json identity mapping만 있어야 합니다".to_owned(),
                    ));
                }
                let version_text = version_text.trim_end_matches(".json");
                result.push(self.inspect(&name_text, version_text)?.installed);
            }
        }
        result
            .sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));
        Ok(result)
    }

    pub fn inspect(&self, name: &str, version: &str) -> BundleResult<BundleInspection> {
        validate_identity(name, version)?;
        let path = self.catalog.join(name).join(format!("{version}.json"));
        let mapping = read_mapping(&path, self.device).map_err(|error| match error {
            BundleError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                BundleError::NotFound {
                    name: name.to_owned(),
                    version: version.to_owned(),
                }
            }
            error => error,
        })?;
        validate_identity_staging_entries(
            path.parent()
                .expect("catalog mapping has an identity parent"),
            self.device,
        )?;
        let installed = self.inspect_by_digest(mapping.digest)?;
        if installed.installed.name != name || installed.installed.version != version {
            return Err(BundleError::Manifest(
                "catalog identity mapping이 Bundle manifest와 다릅니다".to_owned(),
            ));
        }
        Ok(installed)
    }

    fn inspect_by_digest(&self, digest: Sha256Digest) -> BundleResult<BundleInspection> {
        let entry = self.bundle_sha256.join(digest.hex());
        let bundle_bytes = read_safe_file(&entry.join(BUNDLE_JSON), self.device, MAX_FILE_BYTES)?;
        let profile_bytes = read_safe_file(&entry.join(PROFILE_JSON), self.device, MAX_FILE_BYTES)?;
        let bundle: BundleManifest = decode_manifest(&bundle_bytes, "bundle.json")?;
        let profile: BundleProfile = decode_manifest(&profile_bytes, "profile.json")?;
        let canonical_bundle = canonical_json(&bundle)?;
        let canonical_profile = canonical_json(&profile)?;
        let actual = bundle_digest(&canonical_bundle, &canonical_profile);
        if actual != digest {
            return Err(BundleError::Manifest(
                "cache path digest와 Bundle content digest가 다릅니다".to_owned(),
            ));
        }
        validate_bundle(&bundle, &profile, &profile_bytes)?;
        Ok(BundleInspection {
            installed: InstalledBundle {
                digest,
                name: bundle.name.clone(),
                version: bundle.version.clone(),
                runtime_package_digest: bundle.runtime.digest,
                runtime_package_id: bundle.runtime.package_id.clone(),
            },
            manifest: bundle,
            profile,
            _verified: VerifiedBundleInspection,
        })
    }

    fn activate_identity_with_hook<F>(
        &self,
        bundle: &BundleManifest,
        digest: Sha256Digest,
        mut identity_hook: F,
    ) -> BundleResult<()>
    where
        F: FnMut(IdentityActivationPoint) -> BundleResult<()>,
    {
        let directory = ensure_child(&self.catalog, &bundle.name, self.device)?;
        let final_path = directory.join(format!("{}.json", bundle.version));
        let mapping = CatalogMapping { digest };
        let bytes = canonical_json(&mapping)?;
        let temporary = directory.join(format!(
            ".staging-{}-{}",
            std::process::id(),
            NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(error) = write_readonly(&temporary, &bytes, self.device) {
            let _ = remove_identity_staging(&temporary, self.device);
            return Err(error);
        }
        if let Err(error) = identity_hook(IdentityActivationPoint::BeforeRename) {
            remove_identity_staging(&temporary, self.device)?;
            return Err(error);
        }
        match rename_no_replace(&temporary, &final_path) {
            Ok(()) => {
                identity_hook(IdentityActivationPoint::AfterRename)?;
                sync_directory(&directory)?;
                Ok(())
            }
            Err(BundleError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                let current = read_mapping(&final_path, self.device)?;
                remove_identity_staging(&temporary, self.device)?;
                if current.digest == digest {
                    sync_directory(&directory)?;
                    Ok(())
                } else {
                    Err(BundleError::IdentityConflict {
                        name: bundle.name.clone(),
                        version: bundle.version.clone(),
                    })
                }
            }
            Err(error) => {
                remove_identity_staging(&temporary, self.device)?;
                Err(error)
            }
        }
    }

    fn create_staging(&self, digest: Sha256Digest) -> BundleResult<PathBuf> {
        let path = self.bundle_sha256.join(format!(
            ".staging-{}-{}-{}",
            std::process::id(),
            NEXT_STAGING.fetch_add(1, Ordering::Relaxed),
            digest.hex()
        ));
        fs::create_dir(&path).map_err(|e| io_error("Bundle staging 생성", path.clone(), e))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|e| io_error("Bundle staging mode", path.clone(), e))?;
        Ok(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogMapping {
    digest: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityActivationPoint {
    BeforeRename,
    AfterRename,
}

struct Staging {
    path: PathBuf,
    activated: bool,
}
impl Staging {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            activated: false,
        }
    }
    fn cleanup(&mut self) -> BundleResult<()> {
        remove_staging(&self.path)?;
        self.activated = true;
        Ok(())
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if !self.activated {
            let _ = remove_staging(&self.path);
        }
    }
}

struct VerifiedArchive {
    bundle: BundleManifest,
    digest: Sha256Digest,
    bundle_raw: Vec<u8>,
    profile_raw: Vec<u8>,
}

impl VerifiedArchive {
    fn read(source: &Path, keys: &[TrustedBundleKey]) -> BundleResult<Self> {
        let bytes = fs::read(source)
            .map_err(|e| io_error("Bundle archive 읽기", source.to_path_buf(), e))?;
        if bytes.is_empty() || bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(BundleError::Archive(format!(
                "archive size는 1..={MAX_ARCHIVE_BYTES} bytes여야 합니다"
            )));
        }
        let mut archive = Archive::new(GzDecoder::new(Cursor::new(bytes)));
        let mut files = BTreeMap::new();
        for entry in archive
            .entries()
            .map_err(|e| BundleError::Archive(format!("tar entries를 읽지 못했습니다: {e}")))?
        {
            let entry = entry
                .map_err(|e| BundleError::Archive(format!("tar entry를 읽지 못했습니다: {e}")))?;
            let path = entry.path_bytes();
            let path = std::str::from_utf8(&path)
                .map_err(|_| {
                    BundleError::Archive("archive entry 이름은 UTF-8이어야 합니다".to_owned())
                })?
                .to_owned();
            if ![BUNDLE_JSON, PROFILE_JSON, CHECKSUMS, SIGNATURE].contains(&path.as_str()) {
                return Err(BundleError::Archive(format!(
                    "허용되지 않은 archive entry입니다: {path}"
                )));
            }
            if !entry.header().entry_type().is_file() {
                return Err(BundleError::Archive(format!(
                    "archive entry는 regular file이어야 합니다: {path}"
                )));
            }
            if entry.size() > u64::try_from(MAX_FILE_BYTES).expect("limit fits") {
                return Err(BundleError::Archive(format!(
                    "archive entry가 {MAX_FILE_BYTES} bytes를 넘습니다: {path}"
                )));
            }
            if files.contains_key(&path) {
                return Err(BundleError::Archive(format!(
                    "duplicate archive entry입니다: {path}"
                )));
            }
            let mut content = Vec::new();
            entry
                .take(u64::try_from(MAX_FILE_BYTES + 1).expect("limit fits"))
                .read_to_end(&mut content)
                .map_err(|e| {
                    BundleError::Archive(format!("archive entry를 읽지 못했습니다: {e}"))
                })?;
            if content.len() > MAX_FILE_BYTES {
                return Err(BundleError::Archive(format!(
                    "archive entry가 {MAX_FILE_BYTES} bytes를 넘습니다: {path}"
                )));
            }
            files.insert(path, content);
        }
        if files.len() != 4 {
            return Err(BundleError::Archive(
                "archive는 bundle.json, profile.json, checksums.txt, signature.sig만 가져야 합니다"
                    .to_owned(),
            ));
        }
        let bundle_raw = files.remove(BUNDLE_JSON).expect("checked");
        let profile_raw = files.remove(PROFILE_JSON).expect("checked");
        let checksums = files.remove(CHECKSUMS).expect("checked");
        let signature = files.remove(SIGNATURE).expect("checked");
        verify_checksums(&checksums, &bundle_raw, &profile_raw)?;
        let bundle: BundleManifest = decode_manifest(&bundle_raw, BUNDLE_JSON)?;
        let profile: BundleProfile = decode_manifest(&profile_raw, PROFILE_JSON)?;
        validate_bundle(&bundle, &profile, &profile_raw)?;
        verify_signature(&bundle, &checksums, &signature, keys)?;
        let bundle_canonical = canonical_json(&bundle)?;
        let profile_canonical = canonical_json(&profile)?;
        Ok(Self {
            digest: bundle_digest(&bundle_canonical, &profile_canonical),
            bundle,
            bundle_raw,
            profile_raw,
        })
    }
}

fn verify_checksums(checksums: &[u8], bundle: &[u8], profile: &[u8]) -> BundleResult<()> {
    let expected = format!(
        "{:x}  {BUNDLE_JSON}\n{:x}  {PROFILE_JSON}\n",
        Sha256::digest(bundle),
        Sha256::digest(profile)
    );
    if checksums != expected.as_bytes() {
        return Err(BundleError::Archive(
            "checksums.txt는 bundle.json/profile.json의 exact SHA-256 두 줄이어야 합니다"
                .to_owned(),
        ));
    }
    Ok(())
}

fn verify_signature(
    bundle: &BundleManifest,
    checksums: &[u8],
    signature: &[u8],
    keys: &[TrustedBundleKey],
) -> BundleResult<()> {
    let key = keys
        .iter()
        .find(|key| key.id == bundle.signing_key_id)
        .ok_or_else(|| {
            BundleError::Signature(format!(
                "configured trusted key가 없습니다: {}",
                bundle.signing_key_id
            ))
        })?;
    let text = std::str::from_utf8(signature).map_err(|_| {
        BundleError::Signature("signature.sig는 base64 ASCII여야 합니다".to_owned())
    })?;
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(text.trim_end())
        .map_err(|e| {
            BundleError::Signature(format!("signature.sig base64가 잘못되었습니다: {e}"))
        })?;
    let bytes: [u8; 64] = bytes.try_into().map_err(|_| {
        BundleError::Signature("signature.sig는 64-byte Ed25519 signature여야 합니다".to_owned())
    })?;
    key.key
        .verify(checksums, &Signature::from_bytes(&bytes))
        .map_err(|_| {
            BundleError::Signature("Ed25519 signature가 trusted key와 일치하지 않습니다".to_owned())
        })
}

fn decode_manifest<T: for<'de> Deserialize<'de>>(bytes: &[u8], name: &str) -> BundleResult<T> {
    if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
        return Err(BundleError::Manifest(format!(
            "{name} size가 잘못되었습니다"
        )));
    }
    codec::decode_json(bytes).map_err(|error| BundleError::Manifest(format!("{name}: {error}")))
}

fn canonical_json<T: Serialize>(value: &T) -> BundleResult<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|e| BundleError::Manifest(format!("canonical JSON을 만들지 못했습니다: {e}")))
}

fn bundle_digest(bundle: &[u8], profile: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(bundle);
    hasher.update([0]);
    hasher.update(profile);
    Sha256Digest::from_bytes(hasher.finalize().into())
}

fn validate_bundle(
    bundle: &BundleManifest,
    profile: &BundleProfile,
    profile_raw: &[u8],
) -> BundleResult<()> {
    if bundle.schema_version != BUNDLE_SCHEMA {
        return Err(BundleError::Manifest(
            "bundle.schemaVersion이 지원되지 않습니다".to_owned(),
        ));
    }
    validate_identity(&bundle.name, &bundle.version)?;
    validate_key_id(&bundle.signing_key_id)?;
    if bundle.runtime.package_id.is_empty()
        || bundle.runtime.package_id.len() > 255
        || !bundle
            .runtime
            .package_id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
    {
        return Err(BundleError::Manifest(
            "runtime.packageId가 잘못되었습니다".to_owned(),
        ));
    }
    if bundle.profile_digest != Sha256Digest::from_bytes(Sha256::digest(profile_raw).into()) {
        return Err(BundleError::Manifest(
            "profileDigest가 exact profile.json digest와 다릅니다".to_owned(),
        ));
    }
    if profile.schema_version != PROFILE_SCHEMA
        || profile.name != bundle.name
        || profile.version != bundle.version
    {
        return Err(BundleError::Manifest(
            "profile identity 또는 schemaVersion이 bundle.json과 다릅니다".to_owned(),
        ));
    }
    if profile.inputs.is_empty() || profile.inputs.len() > 32 {
        return Err(BundleError::Manifest(
            "profile inputs는 1..=32개여야 합니다".to_owned(),
        ));
    }
    let mut names = BTreeSet::new();
    let mut local_inputs = 0;
    for input in &profile.inputs {
        validate_slot(&input.name)?;
        if !names.insert(&input.name) || !input.required {
            return Err(BundleError::Manifest(
                "input names는 unique이고 v0alpha1에서는 required여야 합니다".to_owned(),
            ));
        }
        match input.kind.as_str() {
            "LOCAL_INPUT" => local_inputs += 1,
            "STRING" | "INT64" | "BOOLEAN" => {}
            _ => {
                return Err(BundleError::Manifest(
                    "지원하지 않는 input kind입니다".to_owned(),
                ));
            }
        }
        validate_input_schema(input)?;
    }
    if local_inputs != 1 {
        return Err(BundleError::Manifest(
            "v0alpha1 Profile은 정확히 하나의 LOCAL_INPUT을 가져야 합니다".to_owned(),
        ));
    }
    validate_slot(&profile.output.name)?;
    validate_relative_filename(&profile.output.file_name)?;
    if profile.output.media_type.is_empty() || profile.output.maximum_bytes == 0 {
        return Err(BundleError::Manifest(
            "output mediaType과 maximumBytes가 필요합니다".to_owned(),
        ));
    }
    if profile.argv.is_empty() || profile.argv.len() > 128 {
        return Err(BundleError::Manifest(
            "argv는 1..=128개여야 합니다".to_owned(),
        ));
    }
    for argument in &profile.argv {
        validate_argv(argument, &profile.inputs, &profile.output)?;
    }
    if profile.allowed_overrides.len() > 6 {
        return Err(BundleError::Manifest(
            "policy와 allowedOverrides가 잘못되었습니다".to_owned(),
        ));
    }
    let mut overrides = BTreeSet::new();
    for field in &profile.allowed_overrides {
        if !matches!(
            field.as_str(),
            "limits.cpuMax"
                | "limits.memoryMaxBytes"
                | "limits.pidsMax"
                | "limits.wallTimeLimitMs"
                | "output.stdoutTailMaxBytes"
                | "output.stderrTailMaxBytes"
        ) || !overrides.insert(field)
        {
            return Err(BundleError::Manifest(
                "allowedOverrides에는 지원되는 unique resource field만 허용됩니다".to_owned(),
            ));
        }
    }
    crate::resource_budget::ResourceBudget::try_from_protocol(
        profile.policy.limits.clone(),
        profile.policy.output.clone(),
    )
    .map_err(|error| BundleError::Manifest(format!("policy가 잘못되었습니다: {error}")))?;
    Ok(())
}

fn validate_input_schema(input: &BundleInput) -> BundleResult<()> {
    if input.kind != "INT64" {
        if input.allowed_values.is_some() || input.minimum.is_some() || input.maximum.is_some() {
            return Err(BundleError::Manifest(
                "allowedValues와 minimum/maximum은 INT64 input에만 허용됩니다".to_owned(),
            ));
        }
        return Ok(());
    }

    match (&input.allowed_values, input.minimum, input.maximum) {
        (Some(values), None, None)
            if (1..=64).contains(&values.len())
                && values.windows(2).all(|pair| pair[0] < pair[1]) =>
        {
            Ok(())
        }
        (None, Some(minimum), Some(maximum)) if minimum <= maximum => Ok(()),
        _ => Err(BundleError::Manifest(
            "INT64 input은 1..=64개의 strictly ascending allowedValues 또는 완전한 ordered minimum/maximum 중 하나만 가져야 합니다"
                .to_owned(),
        )),
    }
}

fn validate_argv(
    argument: &BundleProfileArgument,
    inputs: &[BundleInput],
    output: &BundleOutput,
) -> BundleResult<()> {
    match argument {
        BundleProfileArgument::Literal(literal) => {
            if literal.is_empty() || literal.len() > 4096 || literal.contains('\0') {
                return Err(BundleError::Manifest(
                    "argv literal이 잘못되었습니다".to_owned(),
                ));
            }
            Ok(())
        }
        BundleProfileArgument::InputPath { slot } => {
            require_input_kind(inputs, slot, "LOCAL_INPUT")
        }
        BundleProfileArgument::InputValue { kind, slot } => require_input_kind(
            inputs,
            slot,
            match kind {
                BundleProfileInputValueKind::String => "STRING",
                BundleProfileInputValueKind::Int64 => "INT64",
                BundleProfileInputValueKind::Boolean => "BOOLEAN",
            },
        ),
        BundleProfileArgument::OutputPath { slot } if slot == &output.name => Ok(()),
        BundleProfileArgument::OutputPath { .. } => Err(BundleError::Manifest(
            "argv placeholder가 matching output slot을 참조하지 않습니다".to_owned(),
        )),
    }
}
fn require_input_kind(inputs: &[BundleInput], slot: &str, kind: &str) -> BundleResult<()> {
    if inputs
        .iter()
        .any(|input| input.name == slot && input.kind == kind)
    {
        Ok(())
    } else {
        Err(BundleError::Manifest(
            "argv placeholder가 matching input slot을 참조하지 않습니다".to_owned(),
        ))
    }
}
pub(crate) fn valid_capsule_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=63).contains(&bytes.len())
        && value.split('.').all(|segment| {
            let bytes = segment.as_bytes();
            !bytes.is_empty()
                && bytes[0].is_ascii_lowercase()
                && bytes[1..]
                    .iter()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        })
}

fn validate_identity(name: &str, version: &str) -> BundleResult<()> {
    if !valid_capsule_name(name) {
        return Err(BundleError::Manifest(
            "Bundle name이 잘못되었습니다".to_owned(),
        ));
    }
    let parsed = Version::parse(version).map_err(|_| {
        BundleError::Manifest("Bundle version은 canonical SemVer여야 합니다".to_owned())
    })?;
    if parsed.to_string() != version || !parsed.pre.is_empty() || !parsed.build.is_empty() {
        return Err(BundleError::Manifest(
            "Bundle version은 prerelease/build 없는 canonical SemVer여야 합니다".to_owned(),
        ));
    }
    Ok(())
}
fn validate_key_id(value: &str) -> BundleResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphanumeric()
            } else {
                c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')
            }
        })
    {
        return Err(BundleError::Manifest(
            "signingKeyId가 잘못되었습니다".to_owned(),
        ));
    }
    Ok(())
}
fn validate_slot(value: &str) -> BundleResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_lowercase()
            } else {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
            }
        })
    {
        return Err(BundleError::Manifest(
            "Profile slot name이 잘못되었습니다".to_owned(),
        ));
    }
    Ok(())
}
fn validate_relative_filename(value: &str) -> BundleResult<()> {
    if value.is_empty()
        || value.len() > 255
        || value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
    {
        return Err(BundleError::Manifest(
            "output fileName은 single relative filename이어야 합니다".to_owned(),
        ));
    }
    Ok(())
}

fn validate_cache_root(path: &Path) -> BundleResult<fs::Metadata> {
    if !path.is_absolute() {
        return Err(BundleError::UnsafeCacheRoot(path.to_path_buf()));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|e| io_error("cache root canonicalize", path.to_path_buf(), e))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| io_error("cache root metadata", path.to_path_buf(), e))?;
    if canonical != path
        || !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(BundleError::UnsafeCacheRoot(path.to_path_buf()));
    }
    Ok(metadata)
}
fn ensure_child(parent: &Path, name: &str, device: u64) -> BundleResult<PathBuf> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|e| io_error("cache directory mode", path.clone(), e))?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_error("cache directory 생성", path, e)),
    };
    let metadata = fs::symlink_metadata(&path)
        .map_err(|e| io_error("cache directory metadata", path.clone(), e))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.dev() != device
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(BundleError::UnsafeCacheRoot(path));
    }
    Ok(path)
}
fn write_readonly(path: &Path, bytes: &[u8], device: u64) -> BundleResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o444)
        .open(path)
        .map_err(|e| io_error("cache file 생성", path.to_path_buf(), e))?;
    file.write_all(bytes)
        .map_err(|e| io_error("cache file 쓰기", path.to_path_buf(), e))?;
    file.set_permissions(fs::Permissions::from_mode(0o444))
        .map_err(|e| io_error("cache file mode", path.to_path_buf(), e))?;
    file.sync_all()
        .map_err(|e| io_error("cache file fsync", path.to_path_buf(), e))?;
    let opened = file
        .metadata()
        .map_err(|e| io_error("cache file metadata", path.to_path_buf(), e))?;
    let linked = fs::symlink_metadata(path)
        .map_err(|e| io_error("cache file metadata", path.to_path_buf(), e))?;
    if !opened.is_file()
        || opened.dev() != device
        || opened.uid() != unsafe { libc::geteuid() }
        || opened.nlink() != 1
        || opened.mode() & 0o7777 != 0o444
        || linked.file_type().is_symlink()
        || linked.dev() != opened.dev()
        || linked.ino() != opened.ino()
    {
        return Err(BundleError::Manifest(format!(
            "cache file을 read-only regular file로 확인하지 못했습니다: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_identity_staging_entry(
    path: &Path,
    name: &str,
    metadata: &fs::Metadata,
    device: u64,
) -> BundleResult<()> {
    if !is_safe_identity_staging_name(name) {
        return Err(BundleError::Manifest(
            "catalog staging 이름이 잘못되었습니다".to_owned(),
        ));
    }
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.dev() != device
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 & !0o444 != 0
        || metadata.len() > u64::try_from(MAX_FILE_BYTES).expect("limit fits")
    {
        return Err(BundleError::UnsafeCacheRoot(path.to_path_buf()));
    }
    Ok(())
}

fn validate_identity_staging_entries(directory: &Path, device: u64) -> BundleResult<()> {
    for entry in fs::read_dir(directory)
        .map_err(|e| io_error("catalog identity 열거", directory.to_path_buf(), e))?
    {
        let entry =
            entry.map_err(|e| io_error("catalog identity entry", directory.to_path_buf(), e))?;
        let name = entry.file_name().into_string().map_err(|_| {
            BundleError::Manifest("catalog version은 UTF-8이어야 합니다".to_owned())
        })?;
        if !name.starts_with(".staging-") {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| io_error("catalog staging metadata", path.clone(), e))?;
        validate_identity_staging_entry(&path, &name, &metadata, device)?;
    }
    Ok(())
}

fn is_safe_identity_staging_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix(".staging-") else {
        return false;
    };
    let mut fields = suffix.split('-');
    matches!(
        (fields.next(), fields.next(), fields.next()),
        (Some(process), Some(sequence), None)
            if process.parse::<u32>().is_ok() && sequence.parse::<u64>().is_ok()
    )
}

fn remove_identity_staging(path: &Path, device: u64) -> BundleResult<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BundleError::Manifest("catalog staging 이름이 잘못되었습니다".to_owned()))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| io_error("catalog staging metadata", path.to_path_buf(), e))?;
    validate_identity_staging_entry(path, name, &metadata, device)?;
    fs::remove_file(path).map_err(|e| io_error("catalog staging cleanup", path.to_path_buf(), e))
}
fn seal_directory(path: &Path) -> BundleResult<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o555))
        .map_err(|e| io_error("cache directory seal", path.to_path_buf(), e))?;
    sync_directory(path)
}
fn remove_staging(path: &Path) -> BundleResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| io_error("Bundle staging metadata", path.to_path_buf(), e))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(BundleError::Manifest(
            "Bundle staging directory가 안전하지 않습니다".to_owned(),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| io_error("Bundle staging mode", path.to_path_buf(), e))?;
    fs::remove_dir_all(path).map_err(|e| io_error("Bundle staging cleanup", path.to_path_buf(), e))
}
fn sync_directory(path: &Path) -> BundleResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| io_error("cache directory fsync", path.to_path_buf(), e))
}
fn read_safe_file(path: &Path, device: u64, maximum: usize) -> BundleResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| io_error("cache file metadata", path.to_path_buf(), e))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.dev() != device
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o444
        || metadata.len() > u64::try_from(maximum).expect("limit fits")
    {
        return Err(BundleError::Manifest(format!(
            "cache file이 안전하지 않습니다: {}",
            path.display()
        )));
    }
    fs::read(path).map_err(|e| io_error("cache file 읽기", path.to_path_buf(), e))
}
fn read_mapping(path: &Path, device: u64) -> BundleResult<CatalogMapping> {
    let bytes = read_safe_file(path, device, MAX_FILE_BYTES)?;
    decode_manifest(&bytes, "catalog mapping")
}
fn rename_no_replace(source: &Path, target: &Path) -> BundleResult<()> {
    let source = std::ffi::CString::new(source.as_os_str().as_encoded_bytes())
        .map_err(|_| BundleError::Archive("staging path에 NUL이 있습니다".to_owned()))?;
    let target = std::ffi::CString::new(target.as_os_str().as_encoded_bytes())
        .map_err(|_| BundleError::Archive("target path에 NUL이 있습니다".to_owned()))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io_error(
            "Bundle atomic activation",
            target.to_string_lossy().into_owned().into(),
            io::Error::last_os_error(),
        ))
    }
}
fn io_error(operation: &'static str, path: PathBuf, source: io::Error) -> BundleError {
    BundleError::Io {
        operation,
        path,
        source,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use ed25519_dalek::{Signer, SigningKey};
    use flate2::{Compression, write::GzEncoder};
    use tempfile::{NamedTempFile, TempDir};

    use super::*;

    pub(crate) fn profile_bytes() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": PROFILE_SCHEMA,
            "name": "ffmpeg-audio-to-wav",
            "version": "1.0.0",
            "inputs": [
                {"name":"source","kind":"LOCAL_INPUT","required":true},
                {"name":"sample_rate_hz","kind":"INT64","required":true,"allowedValues":[8000,16000,22050,44100,48000]},
                {"name":"channels","kind":"INT64","required":true,"allowedValues":[1,2]}
            ],
            "output": {"name":"audio","fileName":"result.wav","mediaType":"audio/wav","maximumBytes":1024},
            "argv": ["-hide_banner", "-loglevel", "error", "-nostdin", "-i", {"input":"source"}, "-map", "0:a:0", "-vn", "-c:a", "pcm_s16le", "-ar", {"int64":"sample_rate_hz"}, "-ac", {"int64":"channels"}, {"output":"audio"}],
            "policy": {
                "limits": {"cpuMax":{"quotaMicros":100000,"periodMicros":100000},"memoryMaxBytes":536870912,"pidsMax":32,"wallTimeLimitMs":120000},
                "output": {"stdoutTailMaxBytes":1024,"stderrTailMaxBytes":1024}
            },
            "allowedOverrides": []
        })).unwrap()
    }

    fn profile_value() -> serde_json::Value {
        serde_json::from_slice(&profile_bytes()).unwrap()
    }

    fn validate_profile_manifest(profile: serde_json::Value) -> BundleResult<()> {
        let profile_raw = serde_json::to_vec(&profile).unwrap();
        let bundle_raw = bundle_bytes(
            &profile_raw,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let bundle: BundleManifest = decode_manifest(&bundle_raw, BUNDLE_JSON)?;
        let profile: BundleProfile = decode_manifest(&profile_raw, PROFILE_JSON)?;

        validate_bundle(&bundle, &profile, &profile_raw)
    }

    fn assert_profile_manifest_rejected(profile: serde_json::Value) {
        assert!(matches!(
            validate_profile_manifest(profile),
            Err(BundleError::Manifest(_))
        ));
    }

    pub(crate) fn bundle_bytes(profile: &[u8], runtime_digest: &str) -> Vec<u8> {
        let profile_value: serde_json::Value = serde_json::from_slice(profile).unwrap();
        let name = profile_value["name"].as_str().unwrap();
        let version = profile_value["version"].as_str().unwrap();
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": BUNDLE_SCHEMA,
            "name": name,
            "version": version,
            "signingKeyId": "test-release",
            "runtime": {"packageId":"org.taskcage.ffmpeg","digest":runtime_digest},
            "profileDigest": format!("sha256:{:x}", Sha256::digest(profile))
        }))
        .unwrap()
    }

    fn archive(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut tar = tar::Builder::new(encoder);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, Cursor::new(bytes))
                .unwrap();
        }
        tar.finish().unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    pub(crate) fn signed_entries(
        bundle: Vec<u8>,
        profile: Vec<u8>,
    ) -> (Vec<(&'static str, Vec<u8>)>, Vec<TrustedBundleKey>) {
        let checksums = format!(
            "{:x}  {BUNDLE_JSON}\n{:x}  {PROFILE_JSON}\n",
            Sha256::digest(&bundle),
            Sha256::digest(&profile)
        )
        .into_bytes();
        let signing = SigningKey::from_bytes(&[7; 32]);
        let signature = base64::engine::general_purpose::STANDARD_NO_PAD
            .encode(signing.sign(&checksums).to_bytes());
        let key = TrustedBundleKey::from_base64(
            "test-release".to_owned(),
            &base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(signing.verifying_key().to_bytes()),
        )
        .unwrap();
        (
            vec![
                (BUNDLE_JSON, bundle),
                (PROFILE_JSON, profile),
                (CHECKSUMS, checksums),
                (SIGNATURE, signature.into_bytes()),
            ],
            vec![key],
        )
    }

    pub(crate) fn write_archive(entries: Vec<(&str, Vec<u8>)>) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&archive(entries)).unwrap();
        file
    }

    fn runtime_package_source(root: &Path) -> PathBuf {
        let source = root.join("runtime-source");
        let rootfs = source.join("rootfs");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&rootfs).unwrap();
        fs::create_dir(rootfs.join("bin")).unwrap();
        fs::create_dir(rootfs.join("share")).unwrap();
        let files = [
            ("bin/tool", b"tool".as_slice(), 0o555),
            ("share/license.txt", b"license".as_slice(), 0o444),
            ("share/sbom.json", b"{}".as_slice(), 0o444),
        ];
        let declarations: Vec<_> = files
            .iter()
            .map(|(path, bytes, mode)| {
                let destination = rootfs.join(path);
                fs::write(&destination, bytes).unwrap();
                fs::set_permissions(&destination, fs::Permissions::from_mode(*mode)).unwrap();
                serde_json::json!({
                    "path": path,
                    "digest": format!("sha256:{:x}", Sha256::digest(bytes)),
                    "sizeBytes": bytes.len(),
                    "mode": format!("{mode:04o}")
                })
            })
            .collect();
        let manifest = serde_json::json!({
            "schemaVersion": "taskcage.runtime-package/v0alpha1",
            "id": "org.taskcage.ffmpeg",
            "version": "1.0.0",
            "platform": {"os":"linux", "architecture":std::env::consts::ARCH, "abi":"gnu", "libc":{"family":"glibc", "minimumVersion":"2.0"}},
            "entrypoint": "bin/tool",
            "libraryPaths": [],
            "files": declarations,
            "licenses": [{"spdxId":"Apache-2.0", "path":"share/license.txt"}],
            "sbom": {"format":"SPDX-JSON-2.3", "path":"share/sbom.json"}
        });
        fs::write(
            source.join("runtime-package.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        source
    }

    pub(crate) fn catalog_with_runtime_package() -> (TempDir, BundleCatalog, Sha256Digest) {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join("cache");
        fs::create_dir(&cache_root).unwrap();
        fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o755)).unwrap();
        let package = RuntimePackageCache::open(&cache_root)
            .unwrap()
            .import(&runtime_package_source(root.path()))
            .unwrap();
        let catalog = BundleCatalog::open(&cache_root).unwrap();
        (root, catalog, package.digest)
    }

    pub(crate) fn signed_bundle_archive(
        profile: &[u8],
        package_digest: Sha256Digest,
    ) -> (NamedTempFile, Vec<TrustedBundleKey>) {
        let bundle = bundle_bytes(profile, &package_digest.to_string());
        let (entries, keys) = signed_entries(bundle, profile.to_vec());
        (write_archive(entries), keys)
    }

    fn identity_mapping_path(catalog: &BundleCatalog) -> PathBuf {
        catalog
            .catalog
            .join("ffmpeg-audio-to-wav")
            .join("1.0.0.json")
    }

    fn injected_identity_activation_failure(point: IdentityActivationPoint) -> BundleError {
        io_error(
            "injected identity activation failure",
            PathBuf::from(format!("{point:?}")),
            io::Error::other("injected identity activation failure"),
        )
    }

    #[test]
    fn capsule_names_accept_strict_namespaced_segments_without_path_like_values() {
        assert!(valid_capsule_name("ffmpeg-audio-to-wav"));
        assert!(valid_capsule_name("media.extract-audio"));
        for invalid in [
            "",
            ".media",
            "media.",
            "media..extract-audio",
            "media.1extract-audio",
            "Media.extract-audio",
            "media/extract-audio",
            "../media",
        ] {
            assert!(!valid_capsule_name(invalid), "accepted {invalid:?}");
        }
        assert!(!valid_capsule_name(&"a".repeat(64)));
    }

    #[test]
    fn accepts_a_signed_bundle_with_limited_argv_placeholders() {
        let profile = profile_bytes();
        let bundle = bundle_bytes(
            &profile,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let (entries, keys) = signed_entries(bundle, profile);
        let file = write_archive(entries);
        let verified = VerifiedArchive::read(file.path(), &keys).unwrap();
        assert_eq!(verified.bundle.name, "ffmpeg-audio-to-wav");
        assert_eq!(verified.bundle.version, "1.0.0");
    }

    #[test]
    fn compiles_profile_argv_to_typed_ir_without_changing_its_json_shape() {
        let mut profile = profile_value();
        profile["inputs"].as_array_mut().unwrap().extend([
            serde_json::json!({"name":"label","kind":"STRING","required":true}),
            serde_json::json!({"name":"retain_metadata","kind":"BOOLEAN","required":true}),
        ]);
        profile["argv"] = serde_json::json!([
            "literal",
            {"input":"source"},
            {"int64":"sample_rate_hz"},
            {"string":"label"},
            {"boolean":"retain_metadata"},
            {"output":"audio"}
        ]);

        assert!(validate_profile_manifest(profile.clone()).is_ok());
        let compiled: BundleProfile =
            decode_manifest(&serde_json::to_vec(&profile).unwrap(), PROFILE_JSON).unwrap();

        assert_eq!(
            compiled.argv,
            vec![
                BundleProfileArgument::Literal("literal".to_owned()),
                BundleProfileArgument::InputPath {
                    slot: "source".to_owned(),
                },
                BundleProfileArgument::InputValue {
                    kind: BundleProfileInputValueKind::Int64,
                    slot: "sample_rate_hz".to_owned(),
                },
                BundleProfileArgument::InputValue {
                    kind: BundleProfileInputValueKind::String,
                    slot: "label".to_owned(),
                },
                BundleProfileArgument::InputValue {
                    kind: BundleProfileInputValueKind::Boolean,
                    slot: "retain_metadata".to_owned(),
                },
                BundleProfileArgument::OutputPath {
                    slot: "audio".to_owned(),
                },
            ]
        );
        assert_eq!(
            serde_json::to_vec(&compiled.argv).unwrap(),
            serde_json::to_vec(&profile["argv"]).unwrap()
        );
    }

    #[test]
    fn rejects_malformed_dangling_and_kind_mismatched_argv_nodes() {
        let invalid_arguments = [
            serde_json::json!({"unknown":"source"}),
            serde_json::json!({"input":"source","output":"audio"}),
            serde_json::json!({"input":1}),
            serde_json::json!({"input":"missing"}),
            serde_json::json!({"int64":"source"}),
            serde_json::json!({"output":"missing"}),
        ];

        for argument in invalid_arguments {
            let mut profile = profile_value();
            profile["argv"] = serde_json::json!([argument]);
            assert_profile_manifest_rejected(profile);
        }
    }

    #[test]
    fn accepts_a_complete_int64_range_instead_of_allowed_values() {
        let mut profile = profile_value();
        let sample_rate = profile["inputs"][1].as_object_mut().unwrap();
        sample_rate.remove("allowedValues");
        sample_rate.insert("minimum".to_owned(), serde_json::json!(8_000));
        sample_rate.insert("maximum".to_owned(), serde_json::json!(48_000));

        assert!(validate_profile_manifest(profile).is_ok());
    }

    #[test]
    fn rejects_empty_duplicate_unsorted_or_oversized_int64_allowed_values() {
        let invalid_values = [
            serde_json::json!([]),
            serde_json::json!([8_000, 8_000]),
            serde_json::json!([16_000, 8_000]),
            serde_json::json!((0..65).collect::<Vec<_>>()),
        ];

        for values in invalid_values {
            let mut profile = profile_value();
            profile["inputs"][1]["allowedValues"] = values;
            assert_profile_manifest_rejected(profile);
        }
    }

    #[test]
    fn rejects_allowed_values_on_non_int64_or_together_with_a_range() {
        let mut non_int64 = profile_value();
        non_int64["inputs"][0]["allowedValues"] = serde_json::json!([1]);
        assert_profile_manifest_rejected(non_int64);

        let mut mixed = profile_value();
        mixed["inputs"][1]["minimum"] = serde_json::json!(8_000);
        mixed["inputs"][1]["maximum"] = serde_json::json!(48_000);
        assert_profile_manifest_rejected(mixed);
    }

    #[test]
    fn rejects_int64_without_one_complete_validation_contract() {
        let mut missing = profile_value();
        missing["inputs"][1]
            .as_object_mut()
            .unwrap()
            .remove("allowedValues");
        assert_profile_manifest_rejected(missing);

        let mut partial_range = profile_value();
        let sample_rate = partial_range["inputs"][1].as_object_mut().unwrap();
        sample_rate.remove("allowedValues");
        sample_rate.insert("minimum".to_owned(), serde_json::json!(8_000));
        assert_profile_manifest_rejected(partial_range);
    }

    #[test]
    fn rejects_an_archive_entry_outside_the_fixed_layout() {
        let profile = profile_bytes();
        let bundle = bundle_bytes(
            &profile,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let (mut entries, keys) = signed_entries(bundle, profile);
        entries.push(("unexpected", b"nope".to_vec()));
        let file = write_archive(entries);
        assert!(matches!(
            VerifiedArchive::read(file.path(), &keys),
            Err(BundleError::Archive(_))
        ));
    }

    #[test]
    fn rejects_a_signature_from_an_untrusted_key() {
        let profile = profile_bytes();
        let bundle = bundle_bytes(
            &profile,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let (entries, _) = signed_entries(bundle, profile);
        let file = write_archive(entries);
        let other = SigningKey::from_bytes(&[8; 32]);
        let keys = vec![
            TrustedBundleKey::from_base64(
                "test-release".to_owned(),
                &base64::engine::general_purpose::STANDARD_NO_PAD
                    .encode(other.verifying_key().to_bytes()),
            )
            .unwrap(),
        ];
        assert!(matches!(
            VerifiedArchive::read(file.path(), &keys),
            Err(BundleError::Signature(_))
        ));
    }

    #[test]
    fn rejects_duplicate_manifest_keys_before_deserialization() {
        let profile = profile_bytes();
        let bundle = br#"{"schemaVersion":"taskcage.bundle/v0alpha1","name":"ffmpeg-audio-to-wav","name":"other","version":"1.0.0","signingKeyId":"test-release","runtime":{"packageId":"org.taskcage.ffmpeg","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"profileDigest":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}"#.to_vec();
        let (entries, keys) = signed_entries(bundle, profile);
        let file = write_archive(entries);
        assert!(matches!(
            VerifiedArchive::read(file.path(), &keys),
            Err(BundleError::Manifest(_))
        ));
    }

    #[test]
    fn imports_and_resolves_a_bundle_only_after_its_package_is_verified() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join("cache");
        fs::create_dir(&cache_root).unwrap();
        fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o755)).unwrap();
        let package = RuntimePackageCache::open(&cache_root)
            .unwrap()
            .import(&runtime_package_source(root.path()))
            .unwrap();
        let profile = profile_bytes();
        let bundle = bundle_bytes(&profile, &package.digest.to_string());
        let (entries, keys) = signed_entries(bundle, profile);
        let archive = write_archive(entries);
        let catalog = BundleCatalog::open(&cache_root).unwrap();

        let imported = catalog.import(archive.path(), &keys).unwrap();
        assert_eq!(imported.outcome, BundleImportOutcome::Imported);
        assert_eq!(
            fs::symlink_metadata(identity_mapping_path(&catalog))
                .unwrap()
                .nlink(),
            1
        );
        assert_eq!(catalog.list().unwrap().len(), 1);
        let installed = catalog.inspect("ffmpeg-audio-to-wav", "1.0.0").unwrap();
        assert_eq!(installed.installed.digest, imported.digest);
        assert_eq!(installed.installed.runtime_package_digest, package.digest);
        assert_eq!(
            catalog.import(archive.path(), &keys).unwrap().outcome,
            BundleImportOutcome::AlreadyPresent
        );
    }

    #[test]
    fn conflicting_digest_never_replaces_an_installed_identity_mapping() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let profile = profile_bytes();
        let (first_archive, first_keys) = signed_bundle_archive(&profile, package_digest);
        let first = catalog.import(first_archive.path(), &first_keys).unwrap();

        let mut different_profile = profile_value();
        different_profile["output"]["maximumBytes"] = serde_json::json!(2048);
        let different_profile = serde_json::to_vec(&different_profile).unwrap();
        let (conflicting_archive, conflicting_keys) =
            signed_bundle_archive(&different_profile, package_digest);
        let error = catalog
            .import(conflicting_archive.path(), &conflicting_keys)
            .unwrap_err();

        assert!(matches!(
            error,
            BundleError::IdentityConflict { name, version }
                if name == "ffmpeg-audio-to-wav" && version == "1.0.0"
        ));
        assert_eq!(
            read_mapping(&identity_mapping_path(&catalog), catalog.device)
                .unwrap()
                .digest,
            first.digest
        );
        assert_eq!(
            fs::symlink_metadata(identity_mapping_path(&catalog))
                .unwrap()
                .nlink(),
            1
        );
    }

    #[test]
    fn safe_stale_identity_staging_does_not_hide_a_valid_mapping() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(&profile_bytes(), package_digest);
        let imported = catalog.import(archive.path(), &keys).unwrap();
        let staging = identity_mapping_path(&catalog)
            .parent()
            .unwrap()
            .join(".staging-4242-7");
        write_readonly(&staging, b"incomplete crash residue", catalog.device).unwrap();

        assert_eq!(catalog.list().unwrap().len(), 1);
        assert_eq!(
            catalog
                .inspect("ffmpeg-audio-to-wav", "1.0.0")
                .unwrap()
                .installed
                .digest,
            imported.digest
        );
        assert!(staging.exists(), "reader must not delete stale staging");
    }

    #[test]
    fn malformed_or_symlink_identity_staging_fails_closed_without_deletion() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(&profile_bytes(), package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        let identity_directory = identity_mapping_path(&catalog)
            .parent()
            .unwrap()
            .to_path_buf();

        let malformed = identity_directory.join(".staging-invalid");
        write_readonly(&malformed, b"malformed", catalog.device).unwrap();
        assert!(matches!(catalog.list(), Err(BundleError::Manifest(_))));
        assert!(matches!(
            catalog.inspect("ffmpeg-audio-to-wav", "1.0.0"),
            Err(BundleError::Manifest(_))
        ));
        assert!(malformed.exists());
        fs::remove_file(&malformed).unwrap();

        let staging_symlink = identity_directory.join(".staging-4242-8");
        symlink(identity_mapping_path(&catalog), &staging_symlink).unwrap();
        assert!(matches!(
            catalog.list(),
            Err(BundleError::UnsafeCacheRoot(path)) if path == staging_symlink
        ));
        assert!(matches!(
            catalog.inspect("ffmpeg-audio-to-wav", "1.0.0"),
            Err(BundleError::UnsafeCacheRoot(path)) if path == staging_symlink
        ));
        assert!(
            fs::symlink_metadata(&staging_symlink)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn failure_before_identity_rename_leaves_no_final_mapping_and_retry_recovers() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(&profile_bytes(), package_digest);
        let error = catalog
            .import_with_identity_hook(archive.path(), &keys, |point| {
                if point == IdentityActivationPoint::BeforeRename {
                    Err(injected_identity_activation_failure(point))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(error, BundleError::Io { .. }));
        assert!(!identity_mapping_path(&catalog).exists());
        assert!(catalog.list().unwrap().is_empty());
        assert_eq!(
            catalog.import(archive.path(), &keys).unwrap().outcome,
            BundleImportOutcome::AlreadyPresent
        );
        assert_eq!(
            fs::symlink_metadata(identity_mapping_path(&catalog))
                .unwrap()
                .nlink(),
            1
        );
    }

    #[test]
    fn failure_after_identity_rename_exposes_only_a_complete_mapping_and_retry_recovers() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let profile = profile_bytes();
        let (archive, keys) = signed_bundle_archive(&profile, package_digest);
        let expected_digest = VerifiedArchive::read(archive.path(), &keys).unwrap().digest;
        let error = catalog
            .import_with_identity_hook(archive.path(), &keys, |point| {
                if point == IdentityActivationPoint::AfterRename {
                    Err(injected_identity_activation_failure(point))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(matches!(error, BundleError::Io { .. }));
        let final_path = identity_mapping_path(&catalog);
        assert_eq!(fs::symlink_metadata(&final_path).unwrap().nlink(), 1);
        assert_eq!(
            read_mapping(&final_path, catalog.device).unwrap().digest,
            expected_digest
        );
        assert_eq!(
            catalog
                .inspect("ffmpeg-audio-to-wav", "1.0.0")
                .unwrap()
                .installed
                .digest,
            expected_digest
        );
        assert_eq!(
            catalog.import(archive.path(), &keys).unwrap().outcome,
            BundleImportOutcome::AlreadyPresent
        );
    }

    #[test]
    fn missing_catalog_identity_is_a_profile_not_found_result() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join("cache");
        fs::create_dir(&cache_root).unwrap();
        fs::set_permissions(&cache_root, fs::Permissions::from_mode(0o755)).unwrap();
        let error = BundleCatalog::open(&cache_root)
            .unwrap()
            .inspect("file-copy", "1.0.0")
            .unwrap_err();
        assert!(matches!(
            error,
            BundleError::NotFound { name, version }
                if name == "file-copy" && version == "1.0.0"
        ));
    }
}
