//! OCI Runtime source retrieval for Capsule authors.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use tar::{Archive, EntryType};

use crate::{
    Error,
    runtime_package_builder::{self, BuildConfig, BuildReport},
};

pub fn build_from_oci(reference: &str, config: &BuildConfig) -> Result<BuildReport, Error> {
    validate_reference(reference)?;
    let image = reference
        .strip_prefix("oci://")
        .expect("validated OCI reference");
    let stage = staging_root(&config.output)?;
    let result = (|| {
        docker(&["pull", "--platform", &config.platform, image])?;
        let container = docker_stdout(&["create", "--platform", &config.platform, image])?;
        let cleanup = ContainerCleanup(container.clone());
        export_rootfs(&container, &stage)?;
        drop(cleanup);
        let mut package_config = config.clone();
        package_config.source_rootfs = stage.clone();
        runtime_package_builder::build(&package_config)
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

struct ContainerCleanup(String);
impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.0])
            .status();
    }
}

fn validate_reference(value: &str) -> Result<(), Error> {
    let Some(image) = value.strip_prefix("oci://") else {
        return Err(Error::InvalidArgument(
            "OCI Runtime은 oci://로 시작해야 합니다".to_owned(),
        ));
    };
    let Some((repository, digest)) = image.rsplit_once('@') else {
        return Err(Error::InvalidArgument(
            "OCI Runtime은 immutable @sha256 digest를 지정해야 합니다".to_owned(),
        ));
    };
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(Error::InvalidArgument(
            "OCI Runtime digest는 sha256이어야 합니다".to_owned(),
        ));
    };
    if repository.is_empty() || hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::InvalidArgument(
            "OCI Runtime reference가 canonical digest 형식이 아닙니다".to_owned(),
        ));
    }
    Ok(())
}

fn staging_root(output: &Path) -> Result<PathBuf, Error> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stage = parent.join(format!(".taskcage-oci-rootfs-{}", std::process::id()));
    fs::create_dir(&stage).map_err(|error| {
        Error::InvalidArgument(format!(
            "OCI Runtime staging directory 생성 실패 {}: {error}",
            stage.display()
        ))
    })?;
    Ok(stage)
}

fn docker(args: &[&str]) -> Result<(), Error> {
    let status = Command::new("docker")
        .args(args)
        .status()
        .map_err(|error| Error::InvalidArgument(format!("Docker가 필요합니다: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!(
            "docker {} 실패",
            args.join(" ")
        )))
    }
}
fn docker_stdout(args: &[&str]) -> Result<String, Error> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .map_err(|error| Error::InvalidArgument(format!("Docker가 필요합니다: {error}")))?;
    if !output.status.success() {
        return Err(Error::InvalidArgument(format!(
            "docker {} 실패",
            args.join(" ")
        )));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| Error::InvalidArgument("Docker output은 UTF-8이어야 합니다".to_owned()))
}

fn export_rootfs(container: &str, destination: &Path) -> Result<(), Error> {
    let mut child = Command::new("docker")
        .args(["export", container])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| Error::InvalidArgument(format!("docker export 실행 실패: {error}")))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::InvalidArgument("docker export stdout을 열 수 없습니다".to_owned())
    })?;
    let result = extract(Archive::new(stdout), destination);
    let status = child
        .wait()
        .map_err(|error| Error::InvalidArgument(format!("docker export 대기 실패: {error}")))?;
    result?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::InvalidArgument("docker export 실패".to_owned()))
    }
}

fn extract<R: io::Read>(mut archive: Archive<R>, destination: &Path) -> Result<(), Error> {
    for entry in archive.entries().map_err(|error| {
        Error::InvalidArgument(format!("OCI Runtime archive 읽기 실패: {error}"))
    })? {
        let mut entry = entry.map_err(|error| {
            Error::InvalidArgument(format!("OCI Runtime archive entry 실패: {error}"))
        })?;
        let path = entry
            .path()
            .map_err(|error| Error::InvalidArgument(format!("OCI Runtime path 실패: {error}")))?;
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(Error::InvalidArgument(
                "OCI Runtime archive path traversal을 허용하지 않습니다".to_owned(),
            ));
        }
        if !matches!(
            entry.header().entry_type(),
            EntryType::Regular | EntryType::Directory
        ) {
            return Err(Error::InvalidArgument(
                "OCI Runtime archive에는 regular file과 directory만 허용합니다".to_owned(),
            ));
        }
        entry.unpack_in(destination).map_err(|error| {
            Error::InvalidArgument(format!("OCI Runtime rootfs 추출 실패: {error}"))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_digest_pinned_oci_references() {
        let digest = "a".repeat(64);
        assert!(
            validate_reference(&format!("oci://registry.example/tool@sha256:{digest}")).is_ok()
        );
        assert!(validate_reference("oci://registry.example/tool:latest").is_err());
        assert!(validate_reference("https://registry.example/tool@sha256:abc").is_err());
    }
}
