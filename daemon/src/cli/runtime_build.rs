use std::ffi::OsString;
use std::path::PathBuf;

use taskcaged::{Error, runtime_package::PackageLicense, runtime_package_builder};

use super::required_option;

#[derive(Debug)]
pub(crate) struct Config {
    source_rootfs: PathBuf,
    output: PathBuf,
    id: String,
    version: String,
    platform: String,
    glibc_minimum: String,
    entrypoint: String,
    library_paths: Vec<String>,
    licenses: Vec<PackageLicense>,
    sbom_path: String,
}

pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<Config> {
    let mut source_rootfs = None;
    let mut output = None;
    let mut id = None;
    let mut version = None;
    let mut platform = None;
    let mut glibc_minimum = None;
    let mut entrypoint = None;
    let mut sbom_path = None;
    let mut library_paths = Vec::new();
    let mut licenses = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let name = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("runtime build option은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("runtime build {name} 값이 없습니다")))?
            .to_str()
            .ok_or_else(|| {
                Error::InvalidArgument(format!("runtime build {name} 값은 UTF-8이어야 합니다"))
            })?
            .to_owned();
        match name {
            "--source-rootfs" if source_rootfs.is_none() => {
                source_rootfs = Some(PathBuf::from(value))
            }
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--id" if id.is_none() => id = Some(value),
            "--version" if version.is_none() => version = Some(value),
            "--platform" if platform.is_none() => platform = Some(value),
            "--glibc-minimum" if glibc_minimum.is_none() => glibc_minimum = Some(value),
            "--entrypoint" if entrypoint.is_none() => entrypoint = Some(value),
            "--sbom" if sbom_path.is_none() => sbom_path = Some(value),
            "--library-path" => library_paths.push(value),
            "--license" => {
                let (spdx_id, path) = value.split_once(':').ok_or_else(|| {
                    Error::InvalidArgument("--license은 SPDX:path 형식이어야 합니다".to_owned())
                })?;
                licenses.push(PackageLicense {
                    spdx_id: spdx_id.to_owned(),
                    path: path.to_owned(),
                });
            }
            "--source-rootfs" | "--output" | "--id" | "--version" | "--platform"
            | "--glibc-minimum" | "--entrypoint" | "--sbom" => {
                return Err(Error::InvalidArgument(format!(
                    "runtime build option이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 runtime build option입니다: {name}"
                )));
            }
        };
        index += 2;
    }
    Ok(Config {
        source_rootfs: required_option("runtime build source-rootfs", source_rootfs)?,
        output: required_option("runtime build output", output)?,
        id: required_option("runtime build id", id)?,
        version: required_option("runtime build version", version)?,
        platform: required_option("runtime build platform", platform)?,
        glibc_minimum: required_option("runtime build glibc-minimum", glibc_minimum)?,
        entrypoint: required_option("runtime build entrypoint", entrypoint)?,
        library_paths,
        licenses,
        sbom_path: required_option("runtime build sbom", sbom_path)?,
    })
}
pub(crate) fn execute(config: Config) -> taskcaged::Result<()> {
    let report = runtime_package_builder::build(&runtime_package_builder::BuildConfig {
        source_rootfs: config.source_rootfs,
        output: config.output,
        id: config.id,
        version: config.version,
        platform: config.platform,
        glibc_minimum: config.glibc_minimum,
        entrypoint: config.entrypoint,
        library_paths: config.library_paths,
        licenses: config.licenses,
        sbom_path: config.sbom_path,
    })?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_required_metadata() {
        let parsed = parse(vec![
            "--source-rootfs".into(),
            "rootfs".into(),
            "--output".into(),
            "runtime".into(),
            "--id".into(),
            "org.example.tool".into(),
            "--version".into(),
            "1.0.0".into(),
            "--platform".into(),
            "linux/arm64".into(),
            "--glibc-minimum".into(),
            "2.35".into(),
            "--entrypoint".into(),
            "bin/tool".into(),
            "--sbom".into(),
            "share/sbom.json".into(),
        ])
        .unwrap();
        assert_eq!(parsed.platform, "linux/arm64");
    }
}
