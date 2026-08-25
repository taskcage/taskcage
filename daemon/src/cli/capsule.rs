#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs::{self, File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::path::{Component, Path, PathBuf};

#[cfg(target_os = "linux")]
use flate2::read::GzDecoder;
#[cfg(target_os = "linux")]
use serde::Serialize;
#[cfg(target_os = "linux")]
use tar::Archive;
#[cfg(target_os = "linux")]
use taskcaged::Error;

#[cfg(target_os = "linux")]
use super::required_option;

#[cfg(target_os = "linux")]
const CAPSULE_ARCHIVE: &str = "capsule.tcbundle.tar.gz";
#[cfg(target_os = "linux")]
const RUNTIME_DIRECTORY: &str = "runtime-package";
#[cfg(target_os = "linux")]
const RUNTIME_MANIFEST: &str = "runtime-package/runtime-package.json";
#[cfg(target_os = "linux")]
const RUNTIME_ROOTFS: &str = "runtime-package/rootfs";
#[cfg(target_os = "linux")]
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(target_os = "linux")]
const DEFAULT_CACHE_ROOT: &str = "/var/lib/taskcage/runtime-package-cache";

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) enum Command {
    Install(InstallConfig),
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct Command;

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct InstallConfig {
    source: PathBuf,
    cache_root: PathBuf,
}

#[cfg(target_os = "linux")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallReport {
    runtime_package: taskcaged::runtime_package::ImportReport,
    capsule: taskcaged::bundle::BundleImportReport,
}

#[cfg(target_os = "linux")]
pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<Command> {
    let (subcommand, options) = args
        .split_first()
        .ok_or_else(|| Error::InvalidArgument("capsule 뒤에는 install이 필요합니다".to_owned()))?;
    if subcommand != "install" {
        return Err(Error::InvalidArgument(
            "capsule subcommand는 install이어야 합니다".to_owned(),
        ));
    }

    let mut source = None;
    let mut cache_root = None;
    let mut index = 0;
    while index < options.len() {
        if !options[index].to_string_lossy().starts_with('-') && source.is_none() {
            source = Some(PathBuf::from(&options[index]));
            index += 1;
            continue;
        }
        let name = options[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("capsule install option은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = options
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{name} option 값이 없습니다")))?;
        match name {
            "--source" if source.is_none() => source = Some(PathBuf::from(value)),
            "--cache-root" if cache_root.is_none() => cache_root = Some(PathBuf::from(value)),
            "--source" | "--cache-root" => {
                return Err(Error::InvalidArgument(format!(
                    "capsule install option이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 capsule install option입니다: {name}"
                )));
            }
        }
        index += 2;
    }

    let source = required_option("source", source)?;
    let cache_root = cache_root.unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE_ROOT));
    if !cache_root.is_absolute() {
        return Err(Error::InvalidArgument(
            "capsule install cache-root는 absolute path여야 합니다".to_owned(),
        ));
    }
    Ok(Command::Install(InstallConfig { source, cache_root }))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn parse(_args: Vec<std::ffi::OsString>) -> taskcaged::Result<Command> {
    Ok(Command)
}

#[cfg(target_os = "linux")]
pub(crate) fn execute(command: Command) -> taskcaged::Result<()> {
    match command {
        Command::Install(config) => install(config),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn execute(_command: Command) -> taskcaged::Result<()> {
    Err(Error::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn install(config: InstallConfig) -> taskcaged::Result<()> {
    let staging = PackStaging::extract(&config.source, &config.cache_root)?;
    let runtime_package = taskcaged::runtime_package::import_for_service_uid(
        &config.cache_root,
        &staging.runtime_package,
    )?;
    let catalog = taskcaged::bundle::BundleCatalog::open(&config.cache_root)?;
    let capsule = catalog.import_unsigned(&staging.capsule)?;
    println!(
        "{}",
        serde_json::to_string(&InstallReport {
            runtime_package,
            capsule,
        })?
    );
    Ok(())
}

#[cfg(target_os = "linux")]
struct PackStaging {
    root: PathBuf,
    runtime_package: PathBuf,
    capsule: PathBuf,
}

#[cfg(target_os = "linux")]
impl PackStaging {
    fn extract(source: &Path, cache_root: &Path) -> taskcaged::Result<Self> {
        let source_metadata = fs::symlink_metadata(source).map_err(|error| {
            Error::InvalidArgument(format!(
                "Capsule Pack을 읽지 못했습니다 {}: {error}",
                source.display()
            ))
        })?;
        if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
            return Err(Error::InvalidArgument(
                "Capsule Pack source는 symlink가 아닌 regular file이어야 합니다".to_owned(),
            ));
        }

        taskcaged::runtime_package::RuntimePackageCache::open(cache_root)?;
        let root = create_staging(cache_root)?;
        let staging = Self {
            runtime_package: root.join(RUNTIME_DIRECTORY),
            capsule: root.join(CAPSULE_ARCHIVE),
            root,
        };
        if let Err(error) = extract_pack(source, &staging) {
            let _ = fs::remove_dir_all(&staging.root);
            return Err(error);
        }
        Ok(staging)
    }
}

#[cfg(target_os = "linux")]
impl Drop for PackStaging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(target_os = "linux")]
fn create_staging(cache_root: &Path) -> taskcaged::Result<PathBuf> {
    for attempt in 0..128_u32 {
        let candidate = cache_root.join(format!(
            ".capsule-pack-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).map_err(
                    |source| {
                        io_error(
                            "Capsule Pack staging directory 권한 설정",
                            &candidate,
                            source,
                        )
                    },
                )?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(io_error(
                    "Capsule Pack staging directory 생성",
                    &candidate,
                    source,
                ));
            }
        }
    }
    Err(Error::InvalidArgument(
        "Capsule Pack staging directory를 만들 수 없습니다".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn extract_pack(source: &Path, staging: &PackStaging) -> taskcaged::Result<()> {
    fs::create_dir(&staging.runtime_package).map_err(|source| {
        io_error(
            "Capsule Pack runtime directory 생성",
            &staging.runtime_package,
            source,
        )
    })?;
    let rootfs = staging.runtime_package.join("rootfs");
    fs::create_dir(&rootfs)
        .map_err(|source| io_error("Capsule Pack rootfs directory 생성", &rootfs, source))?;

    let file =
        File::open(source).map_err(|error| io_error("Capsule Pack source 열기", source, error))?;
    let mut archive = Archive::new(GzDecoder::new(file));
    let entries = archive.entries().map_err(|error| {
        Error::InvalidArgument(format!("Capsule Pack archive를 읽지 못했습니다: {error}"))
    })?;
    let mut seen = BTreeSet::new();
    let mut unpacked = 0_u64;

    for entry in entries {
        let mut entry = entry.map_err(|error| {
            Error::InvalidArgument(format!("Capsule Pack entry를 읽지 못했습니다: {error}"))
        })?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(Error::InvalidArgument(
                "Capsule Pack은 regular file 또는 directory entry만 포함할 수 있습니다".to_owned(),
            ));
        }
        let path = entry.path().map_err(|error| {
            Error::InvalidArgument(format!("Capsule Pack entry path가 잘못되었습니다: {error}"))
        })?;
        let relative = if entry_type.is_dir() {
            validate_pack_directory(&path)?
        } else {
            validate_pack_path(&path)?
        };
        if !seen.insert(relative.clone()) {
            return Err(Error::InvalidArgument(format!(
                "Capsule Pack에 중복된 entry가 있습니다: {}",
                relative.display()
            )));
        }
        let size = entry.size();
        unpacked = unpacked.checked_add(size).ok_or_else(|| {
            Error::InvalidArgument("Capsule Pack unpacked size가 너무 큽니다".to_owned())
        })?;
        if unpacked > MAX_UNPACKED_BYTES {
            return Err(Error::InvalidArgument(format!(
                "Capsule Pack unpacked size는 {} bytes를 초과할 수 없습니다",
                MAX_UNPACKED_BYTES
            )));
        }
        let destination = destination_for(&staging.root, &relative)?;
        if entry_type.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|source| io_error("Capsule Pack directory 생성", &destination, source))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| io_error("Capsule Pack parent directory 생성", parent, source))?;
        }
        let mode = entry.header().mode().map_err(|error| {
            Error::InvalidArgument(format!("Capsule Pack entry mode가 잘못되었습니다: {error}"))
        })? & 0o777;
        copy_entry(&mut entry, &destination, size)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(mode))
            .map_err(|source| io_error("Capsule Pack entry 권한 설정", &destination, source))?;
    }

    for required in [CAPSULE_ARCHIVE, RUNTIME_MANIFEST] {
        if !seen.contains(Path::new(required)) {
            return Err(Error::InvalidArgument(format!(
                "Capsule Pack에 필수 entry가 없습니다: {required}"
            )));
        }
    }
    if !seen.iter().any(|path| path.starts_with(RUNTIME_ROOTFS)) {
        return Err(Error::InvalidArgument(
            "Capsule Pack에는 runtime-package/rootfs 아래 파일이 필요합니다".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_pack_directory(path: &Path) -> taskcaged::Result<PathBuf> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(Error::InvalidArgument(
            "Capsule Pack directory path는 상대 경로여야 합니다".to_owned(),
        ));
    }
    if path == Path::new(RUNTIME_DIRECTORY) || path == Path::new(RUNTIME_ROOTFS) {
        return Ok(path.to_path_buf());
    }
    if path.starts_with(RUNTIME_ROOTFS)
        && path
            .strip_prefix(RUNTIME_ROOTFS)
            .is_ok_and(|suffix| !suffix.as_os_str().is_empty())
    {
        return Ok(path.to_path_buf());
    }
    Err(Error::InvalidArgument(format!(
        "Capsule Pack directory는 {RUNTIME_DIRECTORY} 또는 {RUNTIME_ROOTFS}/ 아래여야 합니다: {}",
        path.display()
    )))
}

#[cfg(target_os = "linux")]
fn validate_pack_path(path: &Path) -> taskcaged::Result<PathBuf> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(Error::InvalidArgument(
            "Capsule Pack entry path는 상대 경로여야 합니다".to_owned(),
        ));
    }
    if path == Path::new(CAPSULE_ARCHIVE) || path == Path::new(RUNTIME_MANIFEST) {
        return Ok(path.to_path_buf());
    }
    let rootfs = Path::new(RUNTIME_ROOTFS);
    if path.starts_with(rootfs)
        && path
            .strip_prefix(rootfs)
            .is_ok_and(|suffix| !suffix.as_os_str().is_empty())
    {
        return Ok(path.to_path_buf());
    }
    Err(Error::InvalidArgument(format!(
        "Capsule Pack entry는 {CAPSULE_ARCHIVE}, {RUNTIME_MANIFEST} 또는 {RUNTIME_ROOTFS}/ 아래여야 합니다: {}",
        path.display()
    )))
}

#[cfg(target_os = "linux")]
fn destination_for(staging: &Path, relative: &Path) -> taskcaged::Result<PathBuf> {
    let destination = staging.join(relative);
    if !destination.starts_with(staging) {
        return Err(Error::InvalidArgument(
            "Capsule Pack entry destination이 staging 밖을 가리킵니다".to_owned(),
        ));
    }
    Ok(destination)
}

#[cfg(target_os = "linux")]
fn copy_entry(
    entry: &mut tar::Entry<'_, GzDecoder<File>>,
    destination: &Path,
    expected: u64,
) -> taskcaged::Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| io_error("Capsule Pack entry 생성", destination, source))?;
    let copied = io::copy(entry, &mut output)
        .map_err(|source| io_error("Capsule Pack entry 쓰기", destination, source))?;
    if copied != expected {
        return Err(Error::InvalidArgument(format!(
            "Capsule Pack entry 크기가 예상과 다릅니다: {}",
            destination.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn io_error(operation: &str, path: &Path, source: io::Error) -> Error {
    Error::InvalidArgument(format!(
        "{operation}에 실패했습니다 {}: {source}",
        path.display()
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::{Builder, Header};

    #[test]
    fn install_uses_default_cache_and_accepts_a_positional_source() {
        let source = std::env::temp_dir().join("capsule.tccapsule");
        let command = parse(vec![
            OsString::from("install"),
            source.clone().into_os_string(),
        ])
        .unwrap();
        let Command::Install(config) = command;
        assert_eq!(config.source, source);
        assert_eq!(config.cache_root, PathBuf::from(DEFAULT_CACHE_ROOT));

        let error = parse(vec![
            OsString::from("install"),
            OsString::from("--source"),
            OsString::from("capsule.tccapsule"),
            OsString::from("--cache-root"),
            OsString::from("relative"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }

    #[test]
    fn pack_paths_are_strictly_limited() {
        assert!(validate_pack_path(Path::new(CAPSULE_ARCHIVE)).is_ok());
        assert!(validate_pack_path(Path::new("runtime-package/rootfs/bin/ffmpeg")).is_ok());
        assert!(validate_pack_path(Path::new("../escape")).is_err());
        assert!(validate_pack_path(Path::new("unexpected.txt")).is_err());
        assert!(validate_pack_directory(Path::new("runtime-package/rootfs/bin")).is_ok());
        assert!(validate_pack_directory(Path::new("unexpected")).is_err());
    }

    #[test]
    fn extraction_preserves_runtime_file_modes() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("ffmpeg.tccapsule.tar.gz");
        write_pack(
            &archive_path,
            [
                (CAPSULE_ARCHIVE, b"capsule".as_slice(), 0o444),
                (RUNTIME_MANIFEST, b"{}".as_slice(), 0o444),
                (
                    "runtime-package/rootfs/bin/ffmpeg",
                    b"binary".as_slice(),
                    0o555,
                ),
            ],
        );
        let staging = PackStaging {
            root: directory.path().join("staging"),
            runtime_package: directory.path().join("staging/runtime-package"),
            capsule: directory.path().join("staging").join(CAPSULE_ARCHIVE),
        };
        fs::create_dir(&staging.root).unwrap();

        extract_pack(&archive_path, &staging).unwrap();

        let executable = staging.runtime_package.join("rootfs/bin/ffmpeg");
        assert_eq!(
            fs::metadata(executable).unwrap().permissions().mode() & 0o777,
            0o555
        );
    }

    fn write_pack<'a>(
        destination: &Path,
        entries: impl IntoIterator<Item = (&'a str, &'a [u8], u32)>,
    ) {
        let file = File::create(destination).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = Builder::new(encoder);
        for (path, contents, mode) in entries {
            let mut header = Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive.append_data(&mut header, path, contents).unwrap();
        }
        let mut encoder = archive.into_inner().unwrap();
        encoder.flush().unwrap();
        encoder.finish().unwrap();
    }
}
