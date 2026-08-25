//! Runtime Package directory construction for Capsule authors.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    Error,
    digest::Sha256Digest,
    runtime_package::{
        PackageFile, PackageLicense, PackageSbom, RuntimeLibc, RuntimePackageManifest,
        RuntimePlatform,
        manifest::{ROOTFS_NAME, parse_manifest, validate_relative_path},
    },
};

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub source_rootfs: PathBuf,
    pub output: PathBuf,
    pub id: String,
    pub version: String,
    pub platform: String,
    pub glibc_minimum: String,
    pub entrypoint: String,
    pub library_paths: Vec<String>,
    pub licenses: Vec<PackageLicense>,
    pub sbom_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    pub output: PathBuf,
    pub id: String,
    pub version: String,
    pub platform: String,
    pub file_count: usize,
    pub manifest_digest: String,
}

pub fn build(config: &BuildConfig) -> Result<BuildReport, Error> {
    validate_directory(&config.source_rootfs, "Runtime rootfs source")?;
    if config.output.exists() {
        return Err(Error::InvalidArgument(format!(
            "Runtime Package output이 이미 존재합니다: {}",
            config.output.display()
        )));
    }
    let (architecture, platform) = platform(&config.platform)?;
    for value in std::iter::once(&config.entrypoint)
        .chain(config.library_paths.iter())
        .chain(config.licenses.iter().map(|license| &license.path))
        .chain(std::iter::once(&config.sbom_path))
    {
        validate_relative_path("Runtime Package path", value)?;
    }

    let rootfs = config.output.join(ROOTFS_NAME);
    fs::create_dir_all(&rootfs)
        .map_err(|source| io_error("Runtime Package output 생성", &rootfs, source))?;
    let result = (|| {
        let mut files = Vec::new();
        copy_tree(&config.source_rootfs, &rootfs, Path::new(""), &mut files)?;
        files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        let manifest = RuntimePackageManifest {
            schema_version: "taskcage.runtime-package/v0alpha1".to_owned(),
            id: config.id.clone(),
            version: config.version.clone(),
            platform: RuntimePlatform {
                os: "linux".to_owned(),
                architecture: architecture.to_owned(),
                abi: "gnu".to_owned(),
                libc: RuntimeLibc {
                    family: "glibc".to_owned(),
                    minimum_version: config.glibc_minimum.clone(),
                },
            },
            entrypoint: config.entrypoint.clone(),
            library_paths: config.library_paths.clone(),
            files,
            licenses: config.licenses.clone(),
            sbom: PackageSbom {
                format: "SPDX-JSON-2.3".to_owned(),
                path: config.sbom_path.clone(),
            },
        };
        let manifest_bytes = serde_json_canonicalizer::to_vec(&manifest).map_err(|error| {
            Error::InvalidArgument(format!("Runtime Package manifest 직렬화 실패: {error}"))
        })?;
        let validated = parse_manifest(&manifest_bytes)?;
        let manifest_path = config.output.join("runtime-package.json");
        write_new(&manifest_path, &validated.canonical_json)?;
        Ok(BuildReport {
            output: config.output.clone(),
            id: config.id.clone(),
            version: config.version.clone(),
            platform,
            file_count: manifest.files.len(),
            manifest_digest: validated.digest.to_string(),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&config.output);
    }
    result
}

fn copy_tree(
    source_root: &Path,
    destination_root: &Path,
    relative: &Path,
    files: &mut Vec<PackageFile>,
) -> Result<(), Error> {
    let source = source_root.join(relative);
    let mut entries = fs::read_dir(&source)
        .map_err(|error| io_error("Runtime source directory 읽기", &source, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error("Runtime source directory entry 읽기", &source, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let child = relative.join(&name);
        let child_text = child.to_str().ok_or_else(|| {
            Error::InvalidArgument("Runtime source path는 UTF-8이어야 합니다".to_owned())
        })?;
        validate_relative_path("Runtime source path", child_text)?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| io_error("Runtime source metadata 읽기", &entry.path(), error))?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err(Error::InvalidArgument(format!(
                "Runtime source에는 regular file과 directory만 포함할 수 있습니다: {child_text}"
            )));
        }
        let destination = destination_root.join(&child);
        if metadata.is_dir() {
            fs::create_dir(&destination)
                .map_err(|error| io_error("Runtime Package directory 생성", &destination, error))?;
            copy_tree(source_root, destination_root, &child, files)?;
        } else {
            let executable = metadata.permissions().mode() & 0o111 != 0;
            copy_file(&entry.path(), &destination, executable)?;
            let (digest, size_bytes) = digest_file(&destination)?;
            files.push(PackageFile {
                path: child_text.to_owned(),
                digest,
                size_bytes,
                mode: if executable { "0555" } else { "0444" }.to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn copy_file(source: &Path, destination: &Path, executable: bool) -> Result<(), Error> {
    let mut input =
        File::open(source).map_err(|error| io_error("Runtime source file 열기", source, error))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| io_error("Runtime Package file 생성", destination, error))?;
    io::copy(&mut input, &mut output)
        .map_err(|error| io_error("Runtime Package file 복사", destination, error))?;
    #[cfg(unix)]
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(if executable { 0o555 } else { 0o444 }),
    )
    .map_err(|error| io_error("Runtime Package file 권한 설정", destination, error))?;
    output
        .sync_all()
        .map_err(|error| io_error("Runtime Package file 동기화", destination, error))?;
    Ok(())
}

fn digest_file(path: &Path) -> Result<(Sha256Digest, u64), Error> {
    let mut input =
        File::open(path).map_err(|error| io_error("Runtime Package file 읽기", path, error))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut size = 0_u64;
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| io_error("Runtime Package file hash", path, error))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((Sha256Digest::from_bytes(hash.finalize().into()), size))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error("Runtime Package manifest 생성", path, error))?;
    file.write_all(bytes)
        .map_err(|error| io_error("Runtime Package manifest 쓰기", path, error))?;
    file.sync_all()
        .map_err(|error| io_error("Runtime Package manifest 동기화", path, error))?;
    Ok(())
}
fn validate_directory(path: &Path, label: &str) -> Result<(), Error> {
    if fs::symlink_metadata(path)
        .map_err(|error| io_error("Runtime source metadata 읽기", path, error))?
        .is_dir()
    {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!(
            "{label}는 directory여야 합니다: {}",
            path.display()
        )))
    }
}
fn platform(value: &str) -> Result<(&'static str, String), Error> {
    match value {
        "linux/amd64" => Ok(("x86_64", value.to_owned())),
        "linux/arm64" => Ok(("aarch64", value.to_owned())),
        _ => Err(Error::InvalidArgument(
            "--platform은 linux/amd64 또는 linux/arm64여야 합니다".to_owned(),
        )),
    }
}
fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::InvalidArgument(format!("{action} 실패 {}: {source}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_valid_runtime_package_from_a_rootfs() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("share")).unwrap();
        fs::write(source.join("bin/tool"), b"tool").unwrap();
        fs::set_permissions(source.join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(source.join("share/license.txt"), b"license").unwrap();
        fs::write(source.join("share/sbom.json"), b"{}\n").unwrap();
        let output = temporary.path().join("runtime");

        let report = build(&BuildConfig {
            source_rootfs: source,
            output: output.clone(),
            id: "org.example.tool".to_owned(),
            version: "1.0.0".to_owned(),
            platform: "linux/arm64".to_owned(),
            glibc_minimum: "2.35".to_owned(),
            entrypoint: "bin/tool".to_owned(),
            library_paths: Vec::new(),
            licenses: vec![PackageLicense {
                spdx_id: "Apache-2.0".to_owned(),
                path: "share/license.txt".to_owned(),
            }],
            sbom_path: "share/sbom.json".to_owned(),
        })
        .unwrap();

        assert_eq!(report.file_count, 3);
        let manifest = fs::read(output.join("runtime-package.json")).unwrap();
        assert!(parse_manifest(&manifest).is_ok());
        assert_eq!(
            fs::metadata(output.join("rootfs/bin/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
    }
}
