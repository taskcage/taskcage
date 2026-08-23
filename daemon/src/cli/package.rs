use std::ffi::OsString;
use std::path::PathBuf;

use taskcaged::Error;

use super::required_option;

#[derive(Debug)]
pub(crate) struct Config {
    source: PathBuf,
    cache_root: PathBuf,
}

pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<Config> {
    let mut source = None;
    let mut cache_root = None;
    let mut index = 0;
    while index < args.len() {
        let name = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("import-package 옵션 이름은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{name} 옵션 값이 없습니다")))?;
        match name {
            "--source" if source.is_none() => source = Some(PathBuf::from(value)),
            "--cache-root" if cache_root.is_none() => cache_root = Some(PathBuf::from(value)),
            "--source" | "--cache-root" => {
                return Err(Error::InvalidArgument(format!(
                    "import-package 옵션이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 import-package 옵션입니다: {name}"
                )));
            }
        }
        index += 2;
    }

    let source = required_option("source", source)?;
    let cache_root = required_option("cache-root", cache_root)?;
    if !source.is_absolute() || !cache_root.is_absolute() {
        return Err(Error::InvalidArgument(
            "import-package source와 cache-root는 절대 경로여야 합니다".to_owned(),
        ));
    }
    Ok(Config { source, cache_root })
}

pub(crate) fn execute(config: Config) -> taskcaged::Result<()> {
    let report =
        taskcaged::runtime_package::import_for_service_uid(&config.cache_root, &config.source)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_package_requires_two_absolute_paths() {
        let source = std::env::temp_dir().join("taskcage-package-source");
        let cache_root = std::env::temp_dir().join("taskcage-package-cache");
        let config = parse(vec![
            OsString::from("--source"),
            source.clone().into_os_string(),
            OsString::from("--cache-root"),
            cache_root.clone().into_os_string(),
        ])
        .unwrap();
        assert_eq!(config.source, source);
        assert_eq!(config.cache_root, cache_root);

        let error = parse(vec![
            OsString::from("--source"),
            OsString::from("relative"),
            OsString::from("--cache-root"),
            cache_root.into_os_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("절대 경로"));
    }

    #[test]
    fn import_package_rejects_missing_unknown_and_duplicate_options() {
        let source = std::env::temp_dir().join("taskcage-package-source");
        let cache_root = std::env::temp_dir().join("taskcage-package-cache");
        assert!(
            parse(vec![
                OsString::from("--source"),
                source.clone().into_os_string(),
            ])
            .unwrap_err()
            .to_string()
            .contains("cache-root 옵션은 필수")
        );
        assert!(
            parse(vec![OsString::from("--unknown"), OsString::from("value")])
                .unwrap_err()
                .to_string()
                .contains("알 수 없는")
        );
        assert!(
            parse(vec![
                OsString::from("--source"),
                source.clone().into_os_string(),
                OsString::from("--source"),
                source.into_os_string(),
                OsString::from("--cache-root"),
                cache_root.into_os_string(),
            ])
            .unwrap_err()
            .to_string()
            .contains("중복")
        );
    }
}
