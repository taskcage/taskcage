//! Deterministic basic-mode Capsule Pack construction.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use flate2::{Compression, write::GzEncoder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tar::Builder;

use crate::{Error, capsulefile::CapsulefileSpec, runtime_package::manifest::parse_manifest};

const CAPSULE_ARCHIVE: &str = "capsule.tcbundle.tar.gz";
#[cfg(test)]
const RUNTIME_MANIFEST: &str = "runtime-package/runtime-package.json";
#[cfg(test)]
const RUNTIME_ROOTFS: &str = "runtime-package/rootfs";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    pub output: PathBuf,
    pub name: String,
    pub version: String,
    pub runtime_package_id: String,
    pub runtime_package_digest: String,
    pub platform: String,
}

pub fn build(
    spec: &CapsulefileSpec,
    runtime_package: &Path,
    target_platform: &str,
    output: &Path,
) -> Result<BuildReport, Error> {
    validate_regular_directory(runtime_package, "Runtime Package source")?;
    let manifest_path = runtime_package.join("runtime-package.json");
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|source| io_error("Runtime Package manifest 읽기", &manifest_path, source))?;
    let validated_manifest = parse_manifest(&manifest_bytes).map_err(|source| {
        Error::InvalidArgument(format!(
            "Runtime Package manifest가 잘못되었습니다: {source}"
        ))
    })?;
    let manifest = &validated_manifest.manifest;
    let package_id = &manifest.id;
    let architecture = &manifest.platform.architecture;
    let expected_architecture = match target_platform {
        "linux/amd64" => "x86_64",
        "linux/arm64" => "aarch64",
        _ => {
            return Err(Error::InvalidArgument(
                "--platform은 linux/amd64 또는 linux/arm64여야 합니다".to_owned(),
            ));
        }
    };
    if manifest.platform.os != "linux" || architecture != expected_architecture {
        return Err(Error::InvalidArgument(format!(
            "Runtime Package platform이 {target_platform}과 호환되지 않습니다"
        )));
    }
    validate_runtime_tree(runtime_package)?;
    let runtime_digest = validated_manifest.digest.to_string();
    let capsule_archive = capsule_archive(spec, package_id, &runtime_digest)?;

    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|source| io_error("Capsule Pack output 생성", output, source))?;
    let encoder = GzEncoder::new(output_file, Compression::default());
    let mut archive = Builder::new(encoder);
    append_bytes(&mut archive, CAPSULE_ARCHIVE, &capsule_archive)?;
    append_runtime_tree(&mut archive, runtime_package, Path::new("runtime-package"))?;
    archive.finish().map_err(|source| {
        Error::InvalidArgument(format!(
            "Capsule Pack archive 마무리에 실패했습니다: {source}"
        ))
    })?;
    archive
        .into_inner()
        .map_err(|source| {
            Error::InvalidArgument(format!(
                "Capsule Pack compression 마무리에 실패했습니다: {source}"
            ))
        })?
        .finish()
        .map_err(|source| {
            Error::InvalidArgument(format!(
                "Capsule Pack output 마무리에 실패했습니다: {source}"
            ))
        })?;

    Ok(BuildReport {
        output: output.to_path_buf(),
        name: spec.name.clone(),
        version: spec.version.clone(),
        runtime_package_id: package_id.clone(),
        runtime_package_digest: runtime_digest,
        platform: target_platform.to_owned(),
    })
}

fn capsule_archive(
    spec: &CapsulefileSpec,
    package_id: &str,
    runtime_digest: &str,
) -> Result<Vec<u8>, Error> {
    let profile = serde_json::to_vec(&spec.profile)?;
    let bundle = serde_json::to_vec(&serde_json::json!({
        "schemaVersion":"taskcage.bundle/v0alpha1", "name":spec.name, "version":spec.version,
        "signingKeyId":"unsigned", "runtime":{"packageId":package_id,"digest":runtime_digest},
        "profileDigest":format!("sha256:{:x}", Sha256::digest(&profile))
    }))?;
    let checksums = format!(
        "{:x}  bundle.json\n{:x}  profile.json\n",
        Sha256::digest(&bundle),
        Sha256::digest(&profile)
    );
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = Builder::new(encoder);
    append_bytes(&mut archive, "bundle.json", &bundle)?;
    append_bytes(&mut archive, "profile.json", &profile)?;
    append_bytes(&mut archive, "checksums.txt", checksums.as_bytes())?;
    archive.finish().map_err(|source| {
        Error::InvalidArgument(format!("Capsule archive 마무리에 실패했습니다: {source}"))
    })?;
    archive
        .into_inner()
        .map_err(|source| {
            Error::InvalidArgument(format!(
                "Capsule archive compression 마무리에 실패했습니다: {source}"
            ))
        })?
        .finish()
        .map_err(|source| {
            Error::InvalidArgument(format!(
                "Capsule archive output 마무리에 실패했습니다: {source}"
            ))
        })
}

fn append_runtime_tree(
    archive: &mut Builder<GzEncoder<File>>,
    source: &Path,
    destination: &Path,
) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| io_error("Runtime Package metadata 읽기", source, error))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::InvalidArgument(format!(
            "Runtime Package에 symlink를 포함할 수 없습니다: {}",
            source.display()
        )));
    }
    if metadata.is_dir() {
        archive.append_dir(destination, source).map_err(|source| {
            Error::InvalidArgument(format!(
                "Capsule Pack directory 추가에 실패했습니다: {source}"
            ))
        })?;
        let mut entries = fs::read_dir(source)
            .map_err(|error| io_error("Runtime Package directory 읽기", source, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io_error("Runtime Package directory entry 읽기", source, error))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            append_runtime_tree(archive, &entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        archive
            .append_path_with_name(source, destination)
            .map_err(|source| {
                Error::InvalidArgument(format!("Capsule Pack file 추가에 실패했습니다: {source}"))
            })?;
    } else {
        return Err(Error::InvalidArgument(format!(
            "Runtime Package에는 regular file과 directory만 포함할 수 있습니다: {}",
            source.display()
        )));
    }
    Ok(())
}

fn append_bytes<W: Write>(archive: &mut Builder<W>, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o444);
    header.set_cksum();
    archive
        .append_data(&mut header, name, bytes)
        .map_err(|source| {
            Error::InvalidArgument(format!(
                "Capsule archive entry 생성에 실패했습니다: {source}"
            ))
        })
}

fn validate_regular_directory(path: &Path, label: &str) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(label, path, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArgument(format!(
            "{label}는 symlink가 아닌 directory여야 합니다: {}",
            path.display()
        )));
    }
    Ok(())
}
fn validate_runtime_tree(runtime: &Path) -> Result<(), Error> {
    if !runtime.join("rootfs").is_dir() {
        return Err(Error::InvalidArgument(
            "Runtime Package에는 rootfs directory가 필요합니다".to_owned(),
        ));
    }
    Ok(())
}
fn io_error(operation: &str, path: &Path, source: io::Error) -> Error {
    Error::InvalidArgument(format!(
        "{operation}에 실패했습니다 {}: {source}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsulefile;
    use flate2::read::GzDecoder;
    use tar::Archive;

    fn write_runtime_manifest(runtime: &Path) {
        let executable = b"tool";
        let sbom = b"{\"spdxVersion\":\"SPDX-2.3\"}";
        fs::write(runtime.join("rootfs/bin/tool"), executable).unwrap();
        fs::create_dir_all(runtime.join("rootfs/share")).unwrap();
        fs::write(runtime.join("rootfs/share/sbom.spdx.json"), sbom).unwrap();
        fs::write(
            runtime.join("runtime-package.json"),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion":"taskcage.runtime-package/v0alpha1",
                "id":"org.example.tool",
                "version":"1.0.0",
                "platform":{
                    "os":"linux",
                    "architecture":"aarch64",
                    "abi":"gnu",
                    "libc":{"family":"glibc","minimumVersion":"2.17"}
                },
                "entrypoint":"bin/tool",
                "libraryPaths":[],
                "files":[
                    {"path":"bin/tool","digest":format!("sha256:{:x}", Sha256::digest(executable)),"sizeBytes":executable.len(),"mode":"0555"},
                    {"path":"share/sbom.spdx.json","digest":format!("sha256:{:x}", Sha256::digest(sbom)),"sizeBytes":sbom.len(),"mode":"0444"}
                ],
                "licenses":[],
                "sbom":{"format":"SPDX-JSON-2.3","path":"share/sbom.spdx.json"}
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn builds_a_self_contained_unsigned_pack_for_the_selected_platform() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::create_dir(runtime.join("rootfs")).unwrap();
        fs::create_dir(runtime.join("rootfs/bin")).unwrap();
        write_runtime_manifest(&runtime);
        let spec = capsulefile::parse(
            "FROM runtime://example.org/tool:1\nCAPSULE example.tool@1.0.0\nINPUT source ARTIFACT\nOUTPUT result FILE result.bin MEDIA_TYPE application/octet-stream MAX_BYTES 100\nCOMMAND -i ${source} ${result}\nLIMIT CPU 1 MEMORY 1MiB PIDS 1 TIMEOUT 1m\n",
        )
        .unwrap();
        let output = root.path().join("example.tccapsule");
        let report = build(&spec, &runtime, "linux/arm64", &output).unwrap();
        assert_eq!(report.platform, "linux/arm64");
        let file = File::open(output).unwrap();
        let names = Archive::new(GzDecoder::new(file))
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&CAPSULE_ARCHIVE.to_owned()));
        assert!(names.contains(&RUNTIME_MANIFEST.to_owned()));
        assert!(names.contains(&format!("{RUNTIME_ROOTFS}/bin/tool")));
    }
}
