#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

use taskcaged::Error;

#[cfg(target_os = "linux")]
use super::required_option;

#[cfg(target_os = "linux")]
pub(crate) enum Command {
    Import(ImportConfig),
    List(ListConfig),
    Inspect(InspectConfig),
}

#[cfg(target_os = "linux")]
impl Command {
    fn cache_root(&self) -> &Path {
        match self {
            Self::Import(config) => &config.cache_root,
            Self::List(config) => &config.cache_root,
            Self::Inspect(config) => &config.cache_root,
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct Command;

#[cfg(target_os = "linux")]
pub(crate) struct ImportConfig {
    source: PathBuf,
    cache_root: PathBuf,
    trusted_keys: Vec<(String, PathBuf)>,
}

#[cfg(target_os = "linux")]
impl ImportConfig {
    fn load_keys(&self) -> taskcaged::Result<Vec<taskcaged::bundle::TrustedBundleKey>> {
        self.trusted_keys
            .iter()
            .map(|(id, path)| {
                let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                    Error::InvalidArgument(format!(
                        "trusted key를 읽지 못했습니다 {}: {error}",
                        path.display()
                    ))
                })?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(Error::InvalidArgument(format!(
                        "trusted key는 symlink가 아닌 regular file이어야 합니다: {}",
                        path.display()
                    )));
                }
                let encoded = std::fs::read_to_string(path).map_err(|error| {
                    Error::InvalidArgument(format!(
                        "trusted key를 읽지 못했습니다 {}: {error}",
                        path.display()
                    ))
                })?;
                taskcaged::bundle::TrustedBundleKey::from_base64(id.clone(), &encoded)
                    .map_err(|error| Error::InvalidArgument(error.to_string()))
            })
            .collect()
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct ListConfig {
    cache_root: PathBuf,
}

#[cfg(target_os = "linux")]
pub(crate) struct InspectConfig {
    cache_root: PathBuf,
    name: String,
    version: String,
}

#[cfg(target_os = "linux")]
pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<Command> {
    let (subcommand, options) = args.split_first().ok_or_else(|| {
        Error::InvalidArgument("bundle 뒤에는 import, list 또는 inspect가 필요합니다".to_owned())
    })?;
    let subcommand = subcommand.to_str().ok_or_else(|| {
        Error::InvalidArgument("bundle subcommand는 UTF-8이어야 합니다".to_owned())
    })?;
    match subcommand {
        "import" => parse_import(options).map(Command::Import),
        "list" => parse_list(options).map(Command::List),
        "inspect" => parse_inspect(options).map(Command::Inspect),
        _ => Err(Error::InvalidArgument(
            "bundle subcommand는 import, list 또는 inspect여야 합니다".to_owned(),
        )),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn parse(_args: Vec<std::ffi::OsString>) -> taskcaged::Result<Command> {
    Ok(Command)
}

#[cfg(target_os = "linux")]
fn parse_import(args: &[OsString]) -> taskcaged::Result<ImportConfig> {
    let mut source = None;
    let mut cache_root = None;
    let mut trusted_keys = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let name = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("bundle import option은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{name} option 값이 없습니다")))?;
        match name {
            "--source" if source.is_none() => source = Some(PathBuf::from(value)),
            "--cache-root" if cache_root.is_none() => cache_root = Some(PathBuf::from(value)),
            "--trusted-key" => {
                let value = value.to_str().ok_or_else(|| {
                    Error::InvalidArgument("trusted key value는 UTF-8이어야 합니다".to_owned())
                })?;
                let (id, path) = value.split_once('=').ok_or_else(|| {
                    Error::InvalidArgument(
                        "--trusted-key는 <key-id>=<absolute-path> 형식이어야 합니다".to_owned(),
                    )
                })?;
                if id.is_empty() || path.is_empty() {
                    return Err(Error::InvalidArgument(
                        "--trusted-key는 비어 있지 않은 key id와 path가 필요합니다".to_owned(),
                    ));
                }
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err(Error::InvalidArgument(
                        "--trusted-key path는 absolute path여야 합니다".to_owned(),
                    ));
                }
                if trusted_keys.iter().any(|(existing, _)| existing == id) {
                    return Err(Error::InvalidArgument(format!(
                        "--trusted-key가 중복되었습니다: {id}"
                    )));
                }
                trusted_keys.push((id.to_owned(), path));
            }
            "--source" | "--cache-root" => {
                return Err(Error::InvalidArgument(format!(
                    "bundle import option이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 bundle import option입니다: {name}"
                )));
            }
        }
        index += 2;
    }
    let source = required_option("source", source)?;
    let cache_root = required_option("cache-root", cache_root)?;
    if !source.is_absolute() || !cache_root.is_absolute() {
        return Err(Error::InvalidArgument(
            "bundle import source와 cache-root는 absolute path여야 합니다".to_owned(),
        ));
    }
    if trusted_keys.is_empty() {
        return Err(Error::InvalidArgument(
            "bundle import에는 적어도 하나의 --trusted-key가 필요합니다".to_owned(),
        ));
    }
    Ok(ImportConfig {
        source,
        cache_root,
        trusted_keys,
    })
}

#[cfg(target_os = "linux")]
fn parse_list(args: &[OsString]) -> taskcaged::Result<ListConfig> {
    let mut cache_root = None;
    parse_single_option(args, "list", &mut cache_root, None, None)?;
    let cache_root = required_option("cache-root", cache_root)?;
    if !cache_root.is_absolute() {
        return Err(Error::InvalidArgument(
            "bundle list cache-root는 absolute path여야 합니다".to_owned(),
        ));
    }
    Ok(ListConfig { cache_root })
}

#[cfg(target_os = "linux")]
fn parse_inspect(args: &[OsString]) -> taskcaged::Result<InspectConfig> {
    let mut cache_root = None;
    let mut name = None;
    let mut version = None;
    parse_single_option(
        args,
        "inspect",
        &mut cache_root,
        Some(&mut name),
        Some(&mut version),
    )?;
    let cache_root = required_option("cache-root", cache_root)?;
    let name = required_option("name", name)?;
    let version = required_option("version", version)?;
    if !cache_root.is_absolute() {
        return Err(Error::InvalidArgument(
            "bundle inspect cache-root는 absolute path여야 합니다".to_owned(),
        ));
    }
    Ok(InspectConfig {
        cache_root,
        name,
        version,
    })
}

#[cfg(target_os = "linux")]
fn parse_single_option(
    args: &[OsString],
    subcommand: &str,
    cache_root: &mut Option<PathBuf>,
    mut name: Option<&mut Option<String>>,
    mut version: Option<&mut Option<String>>,
) -> taskcaged::Result<()> {
    let mut index = 0;
    while index < args.len() {
        let option = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("bundle option은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{option} option 값이 없습니다")))?;
        match option {
            "--cache-root" if cache_root.is_none() => *cache_root = Some(PathBuf::from(value)),
            "--name" if name.as_ref().is_some_and(|slot| slot.is_none()) => {
                **name.as_mut().expect("checked") = Some(value.to_string_lossy().into_owned())
            }
            "--version" if version.as_ref().is_some_and(|slot| slot.is_none()) => {
                **version.as_mut().expect("checked") = Some(value.to_string_lossy().into_owned())
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 또는 중복된 bundle {subcommand} option입니다: {option}"
                )));
            }
        }
        index += 2;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn execute(command: Command) -> taskcaged::Result<()> {
    let catalog = taskcaged::bundle::BundleCatalog::open(command.cache_root())?;
    match command {
        Command::Import(config) => {
            let keys = config.load_keys()?;
            let report = catalog.import(&config.source, &keys)?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::List(_) => {
            let bundles = catalog.list()?;
            println!("{}", serde_json::to_string(&bundles)?);
        }
        Command::Inspect(config) => {
            let bundle = catalog.inspect(&config.name, &config.version)?;
            println!("{}", serde_json::to_string(&bundle)?);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn execute(_command: Command) -> taskcaged::Result<()> {
    Err(Error::UnsupportedPlatform)
}
