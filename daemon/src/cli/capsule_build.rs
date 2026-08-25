use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use taskcaged::{Error, runtime_oci, runtime_package_builder};

use super::required_option;

#[derive(Debug)]
pub(crate) struct Config {
    capsulefile: PathBuf,
    runtime_package: Option<PathBuf>,
    platform: String,
    output: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    output: PathBuf,
    name: String,
    version: String,
    runtime_package_id: String,
    runtime_package_digest: String,
    platform: String,
}

pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<Config> {
    let mut capsulefile = None;
    let mut runtime_package = None;
    let mut platform = None;
    let mut output = None;
    let mut index = 0;

    while index < args.len() {
        if !args[index].to_string_lossy().starts_with('-') && capsulefile.is_none() {
            capsulefile = Some(PathBuf::from(&args[index]));
            index += 1;
            continue;
        }

        let name = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("capsule build option은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args.get(index + 1).ok_or_else(|| {
            Error::InvalidArgument(format!("capsule build {name} option 값이 없습니다"))
        })?;
        match name {
            "--file" if capsulefile.is_none() => capsulefile = Some(PathBuf::from(value)),
            "--runtime-package" if runtime_package.is_none() => {
                runtime_package = Some(PathBuf::from(value));
            }
            "--platform" if platform.is_none() => {
                platform = Some(
                    value
                        .to_str()
                        .ok_or_else(|| {
                            Error::InvalidArgument(
                                "capsule build platform은 UTF-8이어야 합니다".to_owned(),
                            )
                        })?
                        .to_owned(),
                );
            }
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--file" | "--runtime-package" | "--platform" | "--output" => {
                return Err(Error::InvalidArgument(format!(
                    "capsule build option이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 capsule build option입니다: {name}"
                )));
            }
        }
        index += 2;
    }

    let capsulefile = required_option("capsule build file", capsulefile)?;
    let platform = required_option("capsule build platform", platform)?;
    let output = required_option("capsule build output", output)?;
    if output.extension().and_then(|extension| extension.to_str()) != Some("tccapsule") {
        return Err(Error::InvalidArgument(
            "capsule build output은 .tccapsule 확장자여야 합니다".to_owned(),
        ));
    }

    Ok(Config {
        capsulefile,
        runtime_package,
        platform,
        output,
    })
}

pub(crate) fn execute(config: Config) -> taskcaged::Result<()> {
    let source = fs::read_to_string(&config.capsulefile).map_err(|source| {
        Error::InvalidArgument(format!(
            "Capsulefile을 읽지 못했습니다 {}: {source}",
            config.capsulefile.display()
        ))
    })?;
    let spec = taskcaged::capsulefile::parse(&source)?;
    let temporary = config.output.with_extension("runtime-package.staging");
    let runtime_package = match (
        &config.runtime_package,
        spec.runtime_source.starts_with("oci://"),
    ) {
        (Some(path), false) => path.clone(),
        (None, true) => {
            let runtime = spec
                .runtime
                .as_ref()
                .expect("OCI runtime metadata was required by parser");
            let package = runtime_package_builder::BuildConfig {
                source_rootfs: PathBuf::new(),
                output: temporary.clone(),
                id: runtime.id.clone(),
                version: runtime.version.clone(),
                platform: config.platform.clone(),
                glibc_minimum: runtime.glibc_minimum.clone(),
                entrypoint: runtime.entrypoint.clone(),
                library_paths: runtime.library_paths.clone(),
                licenses: Vec::new(),
                sbom_path: runtime.sbom_path.clone(),
            };
            runtime_oci::build_from_oci(&spec.runtime_source, &package)?;
            temporary.clone()
        }
        (Some(_), true) => {
            return Err(Error::InvalidArgument(
                "OCI FROM은 --runtime-package을 함께 지정할 수 없습니다".to_owned(),
            ));
        }
        (None, false) => {
            return Err(Error::InvalidArgument(
                "runtime:// FROM에는 --runtime-package이 필요합니다".to_owned(),
            ));
        }
    };
    let result =
        taskcaged::capsule_pack::build(&spec, &runtime_package, &config.platform, &config.output);
    if runtime_package == temporary {
        let _ = fs::remove_dir_all(&temporary);
    }
    let report = result?;
    println!(
        "{}",
        serde_json::to_string(&Report {
            output: report.output,
            name: report.name,
            version: report.version,
            runtime_package_id: report.runtime_package_id,
            runtime_package_digest: report.runtime_package_digest,
            platform: report.platform,
        })?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_positional_capsulefile() {
        let config = parse(vec![
            "Capsulefile".into(),
            "--runtime-package".into(),
            "runtime".into(),
            "--platform".into(),
            "linux/arm64".into(),
            "--output".into(),
            "example.tccapsule".into(),
        ])
        .unwrap();
        assert_eq!(config.platform, "linux/arm64");
    }

    #[test]
    fn rejects_an_output_without_the_pack_extension() {
        let error = parse(vec![
            "Capsulefile".into(),
            "--runtime-package".into(),
            "runtime".into(),
            "--platform".into(),
            "linux/amd64".into(),
            "--output".into(),
            "example.tar.gz".into(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains(".tccapsule"));
    }
}
