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
const DEFAULT_TRUST_STORE: &str = "/etc/taskcage/trusted-capsules.d";
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
    trust_store: PathBuf,
    trusted_keys: Vec<(String, PathBuf)>,
}

#[cfg(target_os = "linux")]
impl InstallConfig {
    fn load_keys(&self) -> taskcaged::Result<Vec<taskcaged::bundle::TrustedBundleKey>> {
        let mut keys = load_trust_store(
            &self.trust_store,
            !self.trusted_keys.is_empty() && self.trust_store == Path::new(DEFAULT_TRUST_STORE),
        )?;
        let mut ids = keys
            .iter()
            .map(|key| key.id().to_owned())
            .collect::<BTreeSet<_>>();
        for (id, path) in &self.trusted_keys {
            if !ids.insert(id.clone()) {
                return Err(Error::InvalidArgument(format!(
                    "trusted key id가 중복되었습니다: {id}"
                )));
            }
            keys.push(load_trusted_key(id, path)?);
        }
        if keys.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "trusted key가 없습니다: {}에 <key-id>.pub 파일을 추가하세요",
                self.trust_store.display()
            )));
        }
        Ok(keys)
    }
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
    let mut trust_store = None;
    let mut trusted_keys = Vec::new();
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
            "--trust-store" if trust_store.is_none() => trust_store = Some(PathBuf::from(value)),
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
            "--source" | "--cache-root" | "--trust-store" => {
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
    let trust_store = trust_store.unwrap_or_else(|| PathBuf::from(DEFAULT_TRUST_STORE));
    if !cache_root.is_absolute() || !trust_store.is_absolute() {
        return Err(Error::InvalidArgument(
            "capsule install cache-root와 trust-store는 absolute path여야 합니다".to_owned(),
        ));
    }
    Ok(Command::Install(InstallConfig {
        source,
        cache_root,
        trust_store,
        trusted_keys,
    }))
}

#[cfg(target_os = "linux")]
fn load_trust_store(
    path: &Path,
    allow_missing: bool,
) -> taskcaged::Result<Vec<taskcaged::bundle::TrustedBundleKey>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => {
            return Err(Error::InvalidArgument(format!(
                "trust store를 읽지 못했습니다 {}: {error}",
                path.display()
            )));
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::InvalidArgument(format!(
            "trust store는 symlink가 아닌 directory여야 합니다: {}",
            path.display()
        )));
    }

    let mut key_files = fs::read_dir(path)
        .map_err(|error| {
            Error::InvalidArgument(format!(
                "trust store를 읽지 못했습니다 {}: {error}",
                path.display()
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            Error::InvalidArgument(format!(
                "trust store entry를 읽지 못했습니다 {}: {error}",
                path.display()
            ))
        })?;
    key_files.sort_by_key(|entry| entry.file_name());

    let mut keys = Vec::new();
    for entry in key_files {
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            Error::InvalidArgument(format!(
                "trust store key file name은 UTF-8이어야 합니다: {}",
                path.display()
            ))
        })?;
        let Some(id) = name.strip_suffix(".pub") else {
            continue;
        };
        if id.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "trust store key file name은 <key-id>.pub 형식이어야 합니다: {}",
                entry.path().display()
            )));
        }
        keys.push(load_trusted_key(id, &entry.path())?);
    }
    Ok(keys)
}

#[cfg(target_os = "linux")]
fn load_trusted_key(
    id: &str,
    path: &Path,
) -> taskcaged::Result<taskcaged::bundle::TrustedBundleKey> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
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
    let encoded = fs::read_to_string(path).map_err(|error| {
        Error::InvalidArgument(format!(
            "trusted key를 읽지 못했습니다 {}: {error}",
            path.display()
        ))
    })?;
    taskcaged::bundle::TrustedBundleKey::from_base64(id.to_owned(), &encoded)
        .map_err(|error| Error::InvalidArgument(error.to_string()))
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
    let keys = config.load_keys()?;
    let staging = PackStaging::extract(&config.source, &config.cache_root)?;
    let runtime_package = taskcaged::runtime_package::import_for_service_uid(
        &config.cache_root,
        &staging.runtime_package,
    )?;
    let catalog = taskcaged::bundle::BundleCatalog::open(&config.cache_root)?;
    let capsule = catalog.import(&staging.capsule, &keys)?;
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
    fn install_uses_default_daemon_paths_and_accepts_a_positional_source() {
        let source = std::env::temp_dir().join("capsule.tccapsule.tar.gz");
        let command = parse(vec![
            OsString::from("install"),
            source.clone().into_os_string(),
        ])
        .unwrap();
        let Command::Install(config) = command;
        assert_eq!(config.source, source);
        assert_eq!(config.cache_root, PathBuf::from(DEFAULT_CACHE_ROOT));
        assert_eq!(config.trust_store, PathBuf::from(DEFAULT_TRUST_STORE));

        let error = parse(vec![
            OsString::from("install"),
            OsString::from("--source"),
            OsString::from("capsule.tccapsule.tar.gz"),
            OsString::from("--cache-root"),
            OsString::from("relative"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }

    #[test]
    fn trust_store_uses_file_stem_as_key_id() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("official-release.pub"),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .unwrap();

        let keys = load_trust_store(directory.path(), false).unwrap();

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].id(), "official-release");
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
