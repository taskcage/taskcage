use std::collections::{BTreeSet, HashSet};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::digest::Sha256Digest;

use super::{RuntimePackageError, RuntimePackageResult};

pub(super) const MANIFEST_NAME: &str = "runtime-package.json";
pub(super) const ROOTFS_NAME: &str = "rootfs";
pub(super) const MAX_MANIFEST_BYTES: usize = 1_048_576;
const MAX_ID_BYTES: usize = 255;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_PACKAGE_FILES: usize = 4_096;
const MAX_LIBRARY_PATHS: usize = 64;
const MAX_LICENSES: usize = 256;
const MAX_EXACT_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const PACKAGE_SCHEMA: &str = "taskcage.runtime-package/v0alpha1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePackageManifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub platform: RuntimePlatform,
    pub entrypoint: String,
    pub library_paths: Vec<String>,
    pub files: Vec<PackageFile>,
    pub licenses: Vec<PackageLicense>,
    pub sbom: PackageSbom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePlatform {
    pub os: String,
    pub architecture: String,
    pub abi: String,
    pub libc: RuntimeLibc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLibc {
    pub family: String,
    pub minimum_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub digest: Sha256Digest,
    pub size_bytes: u64,
    pub mode: String,
}

impl PackageFile {
    pub(super) fn mode_bits(&self) -> u32 {
        match self.mode.as_str() {
            "0444" => 0o444,
            "0555" => 0o555,
            _ => unreachable!("manifest 검증 뒤에만 mode를 사용합니다"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageLicense {
    pub spdx_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSbom {
    pub format: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub(super) struct ValidatedManifest {
    pub(super) manifest: RuntimePackageManifest,
    pub(super) digest: Sha256Digest,
    pub(super) canonical_json: Vec<u8>,
}

pub(super) fn parse_manifest(source: &[u8]) -> RuntimePackageResult<ValidatedManifest> {
    if source.is_empty() {
        return Err(RuntimePackageError::InvalidManifest(
            "manifest JSON은 비어 있을 수 없습니다".to_owned(),
        ));
    }
    if source.len() > MAX_MANIFEST_BYTES {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "manifest가 {} bytes여서 상한 {MAX_MANIFEST_BYTES} bytes를 넘었습니다",
            source.len()
        )));
    }

    let manifest: RuntimePackageManifest = serde_json::from_slice(source)
        .map_err(|error| RuntimePackageError::InvalidManifest(error.to_string()))?;
    validate_manifest(&manifest)?;
    let canonical_json = serde_json_canonicalizer::to_vec(&manifest)
        .map_err(|error| RuntimePackageError::InvalidManifest(error.to_string()))?;
    let digest = Sha256Digest::from_bytes(Sha256::digest(&canonical_json).into());
    Ok(ValidatedManifest {
        manifest,
        digest,
        canonical_json,
    })
}

fn validate_manifest(package: &RuntimePackageManifest) -> RuntimePackageResult<()> {
    require_exact("schemaVersion", &package.schema_version, PACKAGE_SCHEMA)?;
    validate_identity(&package.id)?;
    let version = Version::parse(&package.version).map_err(|_| {
        RuntimePackageError::InvalidManifest(
            "version은 canonical SemVer 문자열이어야 합니다".to_owned(),
        )
    })?;
    if version.to_string() != package.version {
        return Err(RuntimePackageError::InvalidManifest(
            "version은 canonical SemVer 문자열이어야 합니다".to_owned(),
        ));
    }
    validate_platform(package)?;
    validate_relative_path("entrypoint", &package.entrypoint)?;

    if package.library_paths.len() > MAX_LIBRARY_PATHS {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "libraryPaths는 최대 {MAX_LIBRARY_PATHS}개입니다"
        )));
    }
    validate_unique_paths("libraryPaths", &package.library_paths)?;
    if package.files.is_empty() || package.files.len() > MAX_PACKAGE_FILES {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "files 항목 수는 1~{MAX_PACKAGE_FILES}개여야 합니다"
        )));
    }

    let mut previous_path: Option<&str> = None;
    let mut total_size = 0_u64;
    for file in &package.files {
        validate_relative_path("files.path", &file.path)?;
        if previous_path.is_some_and(|previous| previous.as_bytes() >= file.path.as_bytes()) {
            return Err(RuntimePackageError::InvalidManifest(
                "files는 path byte 순으로 정렬되고 중복이 없어야 합니다".to_owned(),
            ));
        }
        if package
            .files
            .iter()
            .any(|candidate| candidate.path.starts_with(&format!("{}/", file.path)))
        {
            return Err(RuntimePackageError::InvalidManifest(format!(
                "file path는 다른 file의 parent일 수 없습니다: {}",
                file.path
            )));
        }
        previous_path = Some(&file.path);
        if file.mode != "0444" && file.mode != "0555" {
            return Err(RuntimePackageError::InvalidManifest(
                "files.mode는 0444 또는 0555여야 합니다".to_owned(),
            ));
        }
        if file.size_bytes > MAX_EXACT_JSON_INTEGER {
            return Err(RuntimePackageError::InvalidManifest(
                "files.sizeBytes는 정확히 공유할 수 있는 JSON 정수 범위여야 합니다".to_owned(),
            ));
        }
        total_size = total_size.checked_add(file.size_bytes).ok_or_else(|| {
            RuntimePackageError::InvalidManifest("files 전체 크기가 overflow했습니다".to_owned())
        })?;
    }

    let entrypoint = package
        .files
        .iter()
        .find(|file| file.path == package.entrypoint)
        .ok_or_else(|| {
            RuntimePackageError::InvalidManifest(
                "entrypoint는 files의 regular file을 참조해야 합니다".to_owned(),
            )
        })?;
    if entrypoint.mode != "0555" {
        return Err(RuntimePackageError::InvalidManifest(
            "entrypoint mode는 0555여야 합니다".to_owned(),
        ));
    }

    for library_path in &package.library_paths {
        let prefix = format!("{library_path}/");
        if !package
            .files
            .iter()
            .any(|file| file.path.starts_with(&prefix))
        {
            return Err(RuntimePackageError::InvalidManifest(format!(
                "library path 아래에 file이 없습니다: {library_path}"
            )));
        }
    }

    if package.licenses.len() > MAX_LICENSES {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "licenses는 최대 {MAX_LICENSES}개입니다"
        )));
    }
    let file_paths: HashSet<&str> = package
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let mut license_keys = BTreeSet::new();
    for license in &package.licenses {
        if license.spdx_id.is_empty()
            || license.spdx_id.len() > 128
            || !license.spdx_id.is_ascii()
            || !license
                .spdx_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err(RuntimePackageError::InvalidManifest(
                "licenses.spdxId가 canonical SPDX identifier가 아닙니다".to_owned(),
            ));
        }
        validate_relative_path("licenses.path", &license.path)?;
        if !file_paths.contains(license.path.as_str()) {
            return Err(RuntimePackageError::InvalidManifest(
                "licenses.path는 files 항목을 참조해야 합니다".to_owned(),
            ));
        }
        if !license_keys.insert((&license.spdx_id, &license.path)) {
            return Err(RuntimePackageError::InvalidManifest(
                "중복 license metadata를 허용하지 않습니다".to_owned(),
            ));
        }
    }

    require_exact("sbom.format", &package.sbom.format, "SPDX-JSON-2.3")?;
    validate_relative_path("sbom.path", &package.sbom.path)?;
    if !file_paths.contains(package.sbom.path.as_str()) {
        return Err(RuntimePackageError::InvalidManifest(
            "sbom.path는 files 항목을 참조해야 합니다".to_owned(),
        ));
    }
    Ok(())
}

fn validate_platform(package: &RuntimePackageManifest) -> RuntimePackageResult<()> {
    require_exact("platform.os", &package.platform.os, "linux")?;
    require_exact(
        "platform.architecture",
        &package.platform.architecture,
        "x86_64",
    )?;
    require_exact("platform.abi", &package.platform.abi, "gnu")?;
    require_exact(
        "platform.libc.family",
        &package.platform.libc.family,
        "glibc",
    )?;
    validate_component_version(&package.platform.libc.minimum_version)
}

fn validate_component_version(value: &str) -> RuntimePackageResult<()> {
    let components: Vec<_> = value.split('.').collect();
    if !(2..=3).contains(&components.len())
        || components.iter().any(|component| {
            component.is_empty()
                || !component.bytes().all(|byte| byte.is_ascii_digit())
                || (component.len() > 1 && component.starts_with('0'))
                || component.parse::<u32>().is_err()
        })
    {
        return Err(RuntimePackageError::InvalidManifest(
            "platform.libc.minimumVersion은 두세 개의 canonical 숫자 component여야 합니다"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_identity(value: &str) -> RuntimePackageResult<()> {
    let segments: Vec<_> = value.split('.').collect();
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || segments.len() < 2
        || segments.iter().any(|segment| {
            let bytes = segment.as_bytes();
            bytes.is_empty()
                || !bytes[0].is_ascii_lowercase()
                || !bytes[1..].iter().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                })
        })
    {
        return Err(RuntimePackageError::InvalidManifest(
            "id는 점으로 구분한 canonical lowercase identity여야 합니다".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_relative_path(field: &'static str, value: &str) -> RuntimePackageResult<()> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || value
            .chars()
            .any(|character| character == '\0' || character.is_ascii_control())
        || value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".." | ".taskcage"))
    {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "{field}가 canonical relative path가 아닙니다"
        )));
    }
    Ok(())
}

fn validate_unique_paths(field: &'static str, paths: &[String]) -> RuntimePackageResult<()> {
    let mut unique = HashSet::new();
    for path in paths {
        validate_relative_path(field, path)?;
        if !unique.insert(path) {
            return Err(RuntimePackageError::InvalidManifest(format!(
                "{field}에 중복 path가 있습니다"
            )));
        }
    }
    Ok(())
}

fn require_exact(
    field: &'static str,
    actual: &str,
    expected: &'static str,
) -> RuntimePackageResult<()> {
    if actual != expected {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "{field}는 {expected}여야 합니다"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> Vec<u8> {
        br#"{
          "schemaVersion":"taskcage.runtime-package/v0alpha1",
          "id":"org.taskcage.tool",
          "version":"1.0.0",
          "platform":{"os":"linux","architecture":"x86_64","abi":"gnu","libc":{"family":"glibc","minimumVersion":"2.0"}},
          "entrypoint":"bin/tool",
          "libraryPaths":[],
          "files":[
            {"path":"bin/tool","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sizeBytes":1,"mode":"0555"},
            {"path":"share/license.txt","digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","sizeBytes":1,"mode":"0444"},
            {"path":"share/sbom.json","digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sizeBytes":1,"mode":"0444"}
          ],
          "licenses":[{"spdxId":"Apache-2.0","path":"share/license.txt"}],
          "sbom":{"format":"SPDX-JSON-2.3","path":"share/sbom.json"}
        }"#
        .to_vec()
    }

    #[test]
    fn parses_a_canonical_runtime_package_contract() {
        let validated = parse_manifest(&valid_manifest()).unwrap();
        assert_eq!(validated.manifest.id, "org.taskcage.tool");
        assert!(validated.canonical_json.starts_with(b"{"));
        assert!(validated.digest.to_string().starts_with("sha256:"));
    }

    #[test]
    fn rejects_unknown_fields_and_noncanonical_file_order() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_manifest()).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());

        value.as_object_mut().unwrap().remove("unknown");
        value["files"].as_array_mut().unwrap().reverse();
        assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn rejects_paths_that_can_escape_the_package_root() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_manifest()).unwrap();
        value["entrypoint"] = serde_json::json!("../bin/tool");
        assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn rejects_integers_outside_the_exact_json_range() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_manifest()).unwrap();
        value["files"][0]["sizeBytes"] = serde_json::json!(9_007_199_254_740_992_u64);
        assert!(parse_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
