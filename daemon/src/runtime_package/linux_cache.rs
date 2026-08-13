use std::collections::{BTreeSet, HashSet};
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::digest::Sha256Digest;

use super::manifest::{
    MANIFEST_NAME, MAX_MANIFEST_BYTES, ROOTFS_NAME, ValidatedManifest, parse_manifest,
};
use super::{
    ImportOutcome, ImportReport, ResolvedRuntimePackage, RuntimePackageError, RuntimePackageResult,
};

const PACKAGES_DIRECTORY: &str = "packages";
const SHA256_DIRECTORY: &str = "sha256";
const RENAME_NOREPLACE: libc::c_uint = 1;
const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const SAFE_RESOLUTION: u64 =
    RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH;

static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[derive(Debug)]
pub struct RuntimePackageCache {
    root: PathBuf,
    sha256: PathBuf,
    device: u64,
}

impl RuntimePackageCache {
    pub fn open(root: &Path) -> RuntimePackageResult<Self> {
        let root_metadata = validate_cache_root(root)?;
        let packages = ensure_cache_child(root, PACKAGES_DIRECTORY, root_metadata.dev())?;
        let sha256 = ensure_cache_child(&packages, SHA256_DIRECTORY, root_metadata.dev())?;
        sync_directory(&sha256)?;
        sync_directory(&packages)?;
        sync_directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            sha256,
            device: root_metadata.dev(),
        })
    }

    pub fn import(&self, source: &Path) -> RuntimePackageResult<ImportReport> {
        let source = SourcePackage::open(source)?;
        check_host_compatibility(&source.validated)?;
        let digest = source.validated.digest;
        let staging_path = self.create_staging(digest)?;
        let mut staging = StagingDirectory::new(staging_path);

        self.populate_staging(&source, staging.path())?;
        seal_staging(staging.path())?;
        let final_path = self.entry_path(digest);
        let outcome = match rename_no_replace(staging.path(), &final_path) {
            Ok(()) => {
                staging.activated = true;
                sync_directory(&self.sha256)?;
                ImportOutcome::Imported
            }
            Err(RuntimePackageError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                self.resolve(digest).map_err(|error| {
                    RuntimePackageError::Integrity(format!(
                        "기존 digest entry를 재검증하지 못했습니다: {error}"
                    ))
                })?;
                staging.cleanup()?;
                sync_directory(&self.sha256)?;
                ImportOutcome::AlreadyPresent
            }
            Err(error) => return Err(error),
        };

        Ok(ImportReport { digest, outcome })
    }

    pub fn resolve(&self, digest: Sha256Digest) -> RuntimePackageResult<ResolvedRuntimePackage> {
        let sha256_directory = open_directory_absolute(&self.sha256, self.device)?;
        let entry_name = digest.hex();
        let entry = open_beneath(
            sha256_directory.as_raw_fd(),
            &entry_name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )?;
        validate_directory_descriptor(&entry, self.device, 0o555, "cache entry")?;

        let manifest_file = open_beneath(
            entry.as_raw_fd(),
            MANIFEST_NAME,
            libc::O_RDONLY | libc::O_CLOEXEC,
        )?;
        let manifest_metadata = manifest_file.metadata().map_err(|source| {
            io_error(
                "cached manifest metadata",
                self.entry_path(digest).join(MANIFEST_NAME),
                source,
            )
        })?;
        validate_regular_metadata(&manifest_metadata, self.device, 0o444, MANIFEST_NAME)?;
        let manifest_bytes = read_bounded(
            manifest_file,
            MAX_MANIFEST_BYTES,
            &self.entry_path(digest).join(MANIFEST_NAME),
        )?;
        let validated = parse_manifest(&manifest_bytes)?;
        if validated.digest != digest {
            return Err(RuntimePackageError::Integrity(format!(
                "cache path digest와 manifest digest가 다릅니다: path={digest}, manifest={}",
                validated.digest
            )));
        }
        check_host_compatibility(&validated)?;

        let rootfs = open_beneath(
            entry.as_raw_fd(),
            ROOTFS_NAME,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )?;
        validate_directory_descriptor(&rootfs, self.device, 0o555, ROOTFS_NAME)?;
        require_exact_cached_layout(&self.entry_path(digest), self.device, &validated)?;
        verify_cached_files(rootfs.as_raw_fd(), self.device, &validated)?;
        let entrypoint = open_beneath(
            rootfs.as_raw_fd(),
            &validated.manifest.entrypoint,
            libc::O_RDONLY | libc::O_CLOEXEC,
        )?;
        let entrypoint_metadata = entrypoint.metadata().map_err(|source| {
            io_error(
                "cached entrypoint metadata",
                PathBuf::from(&validated.manifest.entrypoint),
                source,
            )
        })?;
        validate_regular_metadata(
            &entrypoint_metadata,
            self.device,
            0o555,
            &validated.manifest.entrypoint,
        )?;

        Ok(ResolvedRuntimePackage {
            digest,
            manifest: validated.manifest,
            rootfs,
            entrypoint,
        })
    }

    fn populate_staging(&self, source: &SourcePackage, staging: &Path) -> RuntimePackageResult<()> {
        let rootfs_destination = staging.join(ROOTFS_NAME);
        create_directory(&rootfs_destination, 0o700)?;
        let directory_set = declared_directories(&source.validated);
        for directory in &directory_set {
            create_directory(&rootfs_destination.join(directory), 0o700)?;
        }

        for declared in &source.validated.manifest.files {
            let source_file = open_beneath(
                source.rootfs.as_raw_fd(),
                &declared.path,
                libc::O_RDONLY | libc::O_CLOEXEC,
            )?;
            let metadata = source_file.metadata().map_err(|error| {
                io_error(
                    "source file metadata",
                    source.path.join(ROOTFS_NAME).join(&declared.path),
                    error,
                )
            })?;
            validate_source_file(&metadata, source.device, declared)?;
            let destination_path = rootfs_destination.join(&declared.path);
            let mut destination = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(declared.mode_bits())
                .open(&destination_path)
                .map_err(|error| io_error("staging file 생성", destination_path.clone(), error))?;
            copy_and_verify(
                source_file,
                &mut destination,
                declared.size_bytes,
                declared.digest,
                &declared.path,
            )?;
            destination
                .set_permissions(fs::Permissions::from_mode(declared.mode_bits()))
                .map_err(|error| {
                    io_error("staging file mode 설정", destination_path.clone(), error)
                })?;
            destination
                .sync_all()
                .map_err(|error| io_error("staging file fsync", destination_path, error))?;
        }

        let manifest_path = staging.join(MANIFEST_NAME);
        let mut manifest = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o444)
            .open(&manifest_path)
            .map_err(|error| io_error("staging manifest 생성", manifest_path.clone(), error))?;
        manifest
            .write_all(&source.validated.canonical_json)
            .map_err(|error| io_error("staging manifest 쓰기", manifest_path.clone(), error))?;
        manifest
            .set_permissions(fs::Permissions::from_mode(0o444))
            .map_err(|error| {
                io_error("staging manifest mode 설정", manifest_path.clone(), error)
            })?;
        manifest
            .sync_all()
            .map_err(|error| io_error("staging manifest fsync", manifest_path, error))?;
        Ok(())
    }

    fn create_staging(&self, digest: Sha256Digest) -> RuntimePackageResult<PathBuf> {
        let sequence = NEXT_STAGING.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            ".staging-{}-{sequence}-{}",
            std::process::id(),
            digest.hex()
        );
        let path = self.sha256.join(name);
        create_directory(&path, 0o700)?;
        Ok(path)
    }

    fn entry_path(&self, digest: Sha256Digest) -> PathBuf {
        self.sha256.join(digest.hex())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

struct SourcePackage {
    path: PathBuf,
    rootfs: File,
    device: u64,
    validated: ValidatedManifest,
}

impl SourcePackage {
    fn open(path: &Path) -> RuntimePackageResult<Self> {
        if !path.is_absolute() {
            return Err(RuntimePackageError::InvalidSource(
                "source는 절대 경로여야 합니다".to_owned(),
            ));
        }
        let source = open_directory_absolute(path, 0)?;
        let source_metadata = source
            .metadata()
            .map_err(|error| io_error("source directory metadata", path.to_path_buf(), error))?;
        require_exact_source_layout(path, source_metadata.dev())?;
        let manifest = open_beneath(
            source.as_raw_fd(),
            MANIFEST_NAME,
            libc::O_RDONLY | libc::O_CLOEXEC,
        )?;
        let manifest_metadata = manifest.metadata().map_err(|error| {
            io_error("source manifest metadata", path.join(MANIFEST_NAME), error)
        })?;
        validate_source_manifest(&manifest_metadata, source_metadata.dev())?;
        let manifest_bytes = read_bounded(manifest, MAX_MANIFEST_BYTES, &path.join(MANIFEST_NAME))?;
        let validated = parse_manifest(&manifest_bytes)?;
        let rootfs = open_beneath(
            source.as_raw_fd(),
            ROOTFS_NAME,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )?;
        validate_directory_descriptor(&rootfs, source_metadata.dev(), None, ROOTFS_NAME)?;
        require_exact_rootfs(path, source_metadata.dev(), &validated)?;
        Ok(Self {
            path: path.to_path_buf(),
            rootfs,
            device: source_metadata.dev(),
            validated,
        })
    }
}

struct StagingDirectory {
    path: PathBuf,
    activated: bool,
}

impl StagingDirectory {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            activated: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(&mut self) -> RuntimePackageResult<()> {
        make_staging_removable(&self.path)?;
        fs::remove_dir_all(&self.path)
            .map_err(|error| io_error("staging cleanup", self.path.clone(), error))?;
        self.activated = true;
        Ok(())
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.activated {
            let _ = make_staging_removable(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn make_staging_removable(path: &Path) -> RuntimePackageResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(io_error(
                "staging cleanup metadata",
                path.to_path_buf(),
                error,
            ));
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RuntimePackageError::Integrity(format!(
            "staging cleanup 대상이 안전한 directory가 아닙니다: {}",
            path.display()
        )));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("staging cleanup mode", path.to_path_buf(), error))?;
    for entry in fs::read_dir(path)
        .map_err(|error| io_error("staging cleanup 열거", path.to_path_buf(), error))?
    {
        let entry =
            entry.map_err(|error| io_error("staging cleanup entry", path.to_path_buf(), error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| io_error("staging cleanup entry type", entry.path(), error))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            make_staging_removable(&entry.path())?;
        } else if !file_type.is_file() {
            return Err(RuntimePackageError::Integrity(format!(
                "staging cleanup에 안전하지 않은 entry가 있습니다: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn validate_cache_root(path: &Path) -> RuntimePackageResult<fs::Metadata> {
    if !path.is_absolute() {
        return Err(RuntimePackageError::UnsafeCacheRoot(path.to_path_buf()));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| io_error("cache root canonicalize", path.to_path_buf(), error))?;
    if canonical != path {
        return Err(RuntimePackageError::UnsafeCacheRoot(path.to_path_buf()));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("cache root metadata", path.to_path_buf(), error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(RuntimePackageError::UnsafeCacheRoot(path.to_path_buf()));
    }
    Ok(metadata)
}

fn ensure_cache_child(parent: &Path, name: &str, device: u64) -> RuntimePackageResult<PathBuf> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|error| io_error("cache directory mode", path.clone(), error))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error("cache directory 생성", path, error)),
    }
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| io_error("cache directory metadata", path.clone(), error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.dev() != device
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(RuntimePackageError::UnsafeCacheRoot(path));
    }
    Ok(path)
}

fn require_exact_source_layout(path: &Path, device: u64) -> RuntimePackageResult<()> {
    let mut entries = BTreeSet::new();
    for entry in fs::read_dir(path)
        .map_err(|error| io_error("source directory 열거", path.to_path_buf(), error))?
    {
        let entry =
            entry.map_err(|error| io_error("source directory entry", path.to_path_buf(), error))?;
        let name = entry.file_name().into_string().map_err(|_| {
            RuntimePackageError::InvalidSource("source entry 이름은 UTF-8이어야 합니다".to_owned())
        })?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| io_error("source entry metadata", entry.path(), error))?;
        if metadata.dev() != device || metadata.file_type().is_symlink() {
            return Err(RuntimePackageError::InvalidSource(format!(
                "source entry가 symlink 또는 다른 filesystem입니다: {name}"
            )));
        }
        entries.insert(name);
    }
    let expected = BTreeSet::from([MANIFEST_NAME.to_owned(), ROOTFS_NAME.to_owned()]);
    if entries != expected {
        return Err(RuntimePackageError::InvalidSource(
            "source에는 runtime-package.json과 rootfs만 있어야 합니다".to_owned(),
        ));
    }
    Ok(())
}

fn require_exact_rootfs(
    source: &Path,
    device: u64,
    validated: &ValidatedManifest,
) -> RuntimePackageResult<()> {
    let rootfs = source.join(ROOTFS_NAME);
    let mut actual_files = BTreeSet::new();
    let mut actual_directories = BTreeSet::new();
    walk_rootfs(
        &rootfs,
        &rootfs,
        device,
        &mut actual_files,
        &mut actual_directories,
    )?;
    let expected_files: BTreeSet<_> = validated
        .manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let expected_directories = declared_directories(validated);
    if actual_files != expected_files || actual_directories != expected_directories {
        return Err(RuntimePackageError::InvalidSource(
            "rootfs file과 directory는 manifest가 선언한 전체 집합과 같아야 합니다".to_owned(),
        ));
    }
    Ok(())
}

fn require_exact_cached_layout(
    entry: &Path,
    device: u64,
    validated: &ValidatedManifest,
) -> RuntimePackageResult<()> {
    let mut top_level = BTreeSet::new();
    for child in fs::read_dir(entry)
        .map_err(|error| io_error("cache entry 열거", entry.to_path_buf(), error))?
    {
        let child =
            child.map_err(|error| io_error("cache entry 읽기", entry.to_path_buf(), error))?;
        let name = child.file_name().into_string().map_err(|_| {
            RuntimePackageError::Integrity("cache entry 이름은 UTF-8이어야 합니다".to_owned())
        })?;
        let metadata = fs::symlink_metadata(child.path())
            .map_err(|error| io_error("cache entry metadata", child.path(), error))?;
        if metadata.dev() != device || metadata.file_type().is_symlink() {
            return Err(RuntimePackageError::Integrity(format!(
                "cache entry가 symlink 또는 다른 filesystem입니다: {name}"
            )));
        }
        top_level.insert(name);
    }
    let expected = BTreeSet::from([MANIFEST_NAME.to_owned(), ROOTFS_NAME.to_owned()]);
    if top_level != expected {
        return Err(RuntimePackageError::Integrity(
            "cache entry에는 runtime-package.json과 rootfs만 있어야 합니다".to_owned(),
        ));
    }

    let rootfs = entry.join(ROOTFS_NAME);
    let mut actual_files = BTreeSet::new();
    let mut actual_directories = BTreeSet::new();
    walk_rootfs(
        &rootfs,
        &rootfs,
        device,
        &mut actual_files,
        &mut actual_directories,
    )?;
    for directory in &actual_directories {
        let path = rootfs.join(directory);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("cached directory metadata", path.clone(), error))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.dev() != device
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o555
        {
            return Err(RuntimePackageError::Integrity(format!(
                "cached directory type, owner 또는 mode가 잘못되었습니다: {directory}"
            )));
        }
    }
    let expected_files: BTreeSet<_> = validated
        .manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    if actual_files != expected_files || actual_directories != declared_directories(validated) {
        return Err(RuntimePackageError::Integrity(
            "cache rootfs가 manifest의 전체 file set과 다릅니다".to_owned(),
        ));
    }
    Ok(())
}

fn walk_rootfs(
    root: &Path,
    directory: &Path,
    device: u64,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
) -> RuntimePackageResult<()> {
    for entry in fs::read_dir(directory)
        .map_err(|error| io_error("rootfs directory 열거", directory.to_path_buf(), error))?
    {
        let entry = entry
            .map_err(|error| io_error("rootfs directory entry", directory.to_path_buf(), error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("rootfs entry metadata", path.clone(), error))?;
        let relative = path.strip_prefix(root).map_err(|_| {
            RuntimePackageError::InvalidSource("rootfs path 계산에 실패했습니다".to_owned())
        })?;
        let relative = relative.to_str().ok_or_else(|| {
            RuntimePackageError::InvalidSource("rootfs path는 UTF-8이어야 합니다".to_owned())
        })?;
        let relative = relative.replace('\\', "/");
        if metadata.dev() != device {
            return Err(RuntimePackageError::InvalidSource(format!(
                "rootfs가 다른 filesystem으로 넘어갔습니다: {relative}"
            )));
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            directories.insert(relative);
            walk_rootfs(root, &path, device, files, directories)?;
        } else if metadata.is_file() && !metadata.file_type().is_symlink() {
            if metadata.nlink() != 1 {
                return Err(RuntimePackageError::InvalidSource(format!(
                    "hardlink는 허용하지 않습니다: {relative}"
                )));
            }
            files.insert(relative);
        } else {
            return Err(RuntimePackageError::InvalidSource(format!(
                "regular file과 directory만 허용합니다: {relative}"
            )));
        }
    }
    Ok(())
}

fn declared_directories(validated: &ValidatedManifest) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for file in &validated.manifest.files {
        let mut current = PathBuf::new();
        let components: Vec<_> = file.path.split('/').collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            current.push(component);
            directories.insert(current.to_string_lossy().replace('\\', "/"));
        }
    }
    directories
}

fn validate_source_manifest(metadata: &fs::Metadata, device: u64) -> RuntimePackageResult<()> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.dev() != device
        || metadata.nlink() != 1
    {
        return Err(RuntimePackageError::InvalidSource(
            "runtime-package.json은 같은 filesystem의 단일 regular file이어야 합니다".to_owned(),
        ));
    }
    if metadata.len() > u64::try_from(MAX_MANIFEST_BYTES).expect("manifest limit fits u64") {
        return Err(RuntimePackageError::InvalidManifest(
            "manifest가 1 MiB 상한을 넘었습니다".to_owned(),
        ));
    }
    Ok(())
}

fn validate_source_file(
    metadata: &fs::Metadata,
    device: u64,
    declared: &super::PackageFile,
) -> RuntimePackageResult<()> {
    if !metadata.is_file() || metadata.dev() != device || metadata.nlink() != 1 {
        return Err(RuntimePackageError::Integrity(format!(
            "source file type 또는 link count가 잘못되었습니다: {}",
            declared.path
        )));
    }
    if metadata.len() != declared.size_bytes {
        return Err(RuntimePackageError::Integrity(format!(
            "source file size가 manifest와 다릅니다: {}",
            declared.path
        )));
    }
    if metadata.mode() & 0o7777 != declared.mode_bits() {
        return Err(RuntimePackageError::Integrity(format!(
            "source file mode가 manifest와 다릅니다: {}",
            declared.path
        )));
    }
    Ok(())
}

fn copy_and_verify(
    mut source: File,
    destination: &mut File,
    expected_size: u64,
    expected_digest: Sha256Digest,
    path: &str,
) -> RuntimePackageResult<()> {
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|error| io_error("source file 읽기", PathBuf::from(path), error))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).expect("read length fits u64"))
            .ok_or_else(|| RuntimePackageError::Integrity("file size overflow".to_owned()))?;
        if size > expected_size {
            return Err(RuntimePackageError::Integrity(format!(
                "source file이 manifest size를 넘었습니다: {path}"
            )));
        }
        hasher.update(&buffer[..read]);
        destination
            .write_all(&buffer[..read])
            .map_err(|error| io_error("staging file 쓰기", PathBuf::from(path), error))?;
    }
    let actual_digest = Sha256Digest::from_bytes(hasher.finalize().into());
    if size != expected_size || actual_digest != expected_digest {
        return Err(RuntimePackageError::Integrity(format!(
            "source file size 또는 digest가 manifest와 다릅니다: {path}"
        )));
    }
    Ok(())
}

fn verify_cached_files(
    rootfs: RawFd,
    device: u64,
    validated: &ValidatedManifest,
) -> RuntimePackageResult<()> {
    let mut seen_inodes = HashSet::new();
    for declared in &validated.manifest.files {
        let descriptor = open_beneath(rootfs, &declared.path, libc::O_RDONLY | libc::O_CLOEXEC)?;
        let metadata = descriptor.metadata().map_err(|error| {
            io_error("cached file metadata", PathBuf::from(&declared.path), error)
        })?;
        validate_regular_metadata(&metadata, device, declared.mode_bits(), &declared.path)?;
        if !seen_inodes.insert((metadata.dev(), metadata.ino())) {
            return Err(RuntimePackageError::Integrity(format!(
                "cache entry에 hardlink가 있습니다: {}",
                declared.path
            )));
        }
        verify_reader(
            descriptor,
            declared.size_bytes,
            declared.digest,
            &declared.path,
        )?;
    }
    Ok(())
}

fn verify_reader(
    mut reader: File,
    expected_size: u64,
    expected_digest: Sha256Digest,
    path: &str,
) -> RuntimePackageResult<()> {
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| io_error("cached file 읽기", PathBuf::from(path), error))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).expect("read length fits u64"))
            .ok_or_else(|| RuntimePackageError::Integrity("file size overflow".to_owned()))?;
        if size > expected_size {
            return Err(RuntimePackageError::Integrity(format!(
                "cached file이 manifest size를 넘었습니다: {path}"
            )));
        }
        hasher.update(&buffer[..read]);
    }
    if size != expected_size
        || Sha256Digest::from_bytes(hasher.finalize().into()) != expected_digest
    {
        return Err(RuntimePackageError::Integrity(format!(
            "cached file size 또는 digest가 manifest와 다릅니다: {path}"
        )));
    }
    Ok(())
}

fn seal_staging(staging: &Path) -> RuntimePackageResult<()> {
    let rootfs = staging.join(ROOTFS_NAME);
    let mut directories = Vec::new();
    collect_directories(&rootfs, &mut directories)?;
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555))
            .map_err(|error| io_error("staging directory seal", directory.clone(), error))?;
        sync_directory(&directory)?;
    }
    fs::set_permissions(staging, fs::Permissions::from_mode(0o555))
        .map_err(|error| io_error("staging package seal", staging.to_path_buf(), error))?;
    sync_directory(staging)?;
    Ok(())
}

fn collect_directories(path: &Path, directories: &mut Vec<PathBuf>) -> RuntimePackageResult<()> {
    directories.push(path.to_path_buf());
    for entry in fs::read_dir(path)
        .map_err(|error| io_error("staging directory 열거", path.to_path_buf(), error))?
    {
        let entry = entry
            .map_err(|error| io_error("staging directory entry", path.to_path_buf(), error))?;
        if entry
            .file_type()
            .map_err(|error| io_error("staging entry type", entry.path(), error))?
            .is_dir()
        {
            collect_directories(&entry.path(), directories)?;
        }
    }
    Ok(())
}

fn create_directory(path: &Path, mode: u32) -> RuntimePackageResult<()> {
    fs::DirBuilder::new()
        .mode(mode)
        .create(path)
        .map_err(|error| io_error("directory 생성", path.to_path_buf(), error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| io_error("directory mode 설정", path.to_path_buf(), error))
}

fn open_directory_absolute(path: &Path, expected_device: u64) -> RuntimePackageResult<File> {
    let path_text = path.to_str().ok_or_else(|| {
        RuntimePackageError::InvalidSource("directory path는 UTF-8이어야 합니다".to_owned())
    })?;
    let path_c = CString::new(path_text).map_err(|_| {
        RuntimePackageError::InvalidSource("directory path에 NUL을 허용하지 않습니다".to_owned())
    })?;
    let raw = unsafe {
        libc::open(
            path_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw == -1 {
        return Err(io_error(
            "directory 열기",
            path.to_path_buf(),
            io::Error::last_os_error(),
        ));
    }
    let descriptor = unsafe { File::from_raw_fd(raw) };
    let metadata = descriptor
        .metadata()
        .map_err(|error| io_error("directory metadata", path.to_path_buf(), error))?;
    if !metadata.is_dir() || (expected_device != 0 && metadata.dev() != expected_device) {
        return Err(RuntimePackageError::Integrity(format!(
            "directory type 또는 filesystem이 잘못되었습니다: {}",
            path.display()
        )));
    }
    Ok(descriptor)
}

fn open_beneath(directory: RawFd, path: &str, flags: i32) -> RuntimePackageResult<File> {
    let path_c = CString::new(path).map_err(|_| {
        RuntimePackageError::Integrity("package path에 NUL을 허용하지 않습니다".to_owned())
    })?;
    let how = OpenHow {
        flags: u64::try_from(flags).expect("open flags are non-negative"),
        mode: 0,
        resolve: SAFE_RESOLUTION,
    };
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory,
            path_c.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if raw == -1 {
        return Err(io_error(
            "openat2 package path 열기",
            PathBuf::from(path),
            io::Error::last_os_error(),
        ));
    }
    let raw = i32::try_from(raw).map_err(|_| {
        RuntimePackageError::Integrity("openat2 descriptor 범위가 잘못되었습니다".to_owned())
    })?;
    Ok(unsafe { File::from_raw_fd(raw) })
}

fn validate_directory_descriptor(
    descriptor: &File,
    device: u64,
    expected_mode: impl Into<Option<u32>>,
    path: &str,
) -> RuntimePackageResult<()> {
    let metadata = descriptor
        .metadata()
        .map_err(|error| io_error("directory metadata", PathBuf::from(path), error))?;
    if !metadata.is_dir()
        || metadata.dev() != device
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(RuntimePackageError::Integrity(format!(
            "directory type, filesystem 또는 owner가 잘못되었습니다: {path}"
        )));
    }
    if let Some(expected_mode) = expected_mode.into()
        && metadata.mode() & 0o7777 != expected_mode
    {
        return Err(RuntimePackageError::Integrity(format!(
            "directory mode가 잘못되었습니다: {path}"
        )));
    }
    Ok(())
}

fn validate_regular_metadata(
    metadata: &fs::Metadata,
    device: u64,
    expected_mode: u32,
    path: &str,
) -> RuntimePackageResult<()> {
    if !metadata.is_file()
        || metadata.dev() != device
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != expected_mode
    {
        return Err(RuntimePackageError::Integrity(format!(
            "cached file type, owner, link count 또는 mode가 잘못되었습니다: {path}"
        )));
    }
    Ok(())
}

fn read_bounded(mut file: File, maximum: usize, path: &Path) -> RuntimePackageResult<Vec<u8>> {
    let limit = u64::try_from(maximum).expect("manifest limit fits u64") + 1;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("manifest 읽기", path.to_path_buf(), error))?;
    if bytes.len() > maximum {
        return Err(RuntimePackageError::InvalidManifest(format!(
            "manifest가 {maximum} bytes 상한을 넘었습니다"
        )));
    }
    Ok(bytes)
}

fn rename_no_replace(source: &Path, destination: &Path) -> RuntimePackageResult<()> {
    let source_c = path_cstring(source)?;
    let destination_c = path_cstring(destination)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOSYS) || error.raw_os_error() == Some(libc::EINVAL) {
        return Err(RuntimePackageError::AtomicActivationUnavailable(
            destination.to_path_buf(),
        ));
    }
    Err(io_error(
        "Runtime Package activation",
        destination.to_path_buf(),
        error,
    ))
}

fn path_cstring(path: &Path) -> RuntimePackageResult<CString> {
    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        RuntimePackageError::Integrity(format!(
            "filesystem path에 NUL이 있습니다: {}",
            path.display()
        ))
    })
}

fn sync_directory(path: &Path) -> RuntimePackageResult<()> {
    let directory = File::open(path)
        .map_err(|error| io_error("directory fsync open", path.to_path_buf(), error))?;
    directory
        .sync_all()
        .map_err(|error| io_error("directory fsync", path.to_path_buf(), error))
}

fn check_host_compatibility(validated: &ValidatedManifest) -> RuntimePackageResult<()> {
    if std::env::consts::OS != "linux"
        || !matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
        || validated.manifest.platform.architecture != std::env::consts::ARCH
    {
        return Err(RuntimePackageError::IncompatiblePlatform(format!(
            "package는 linux/{}을 요구하지만 host는 {}/{}입니다",
            validated.manifest.platform.architecture,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )));
    }
    #[cfg(not(target_env = "gnu"))]
    {
        let _ = validated;
        return Err(RuntimePackageError::IncompatiblePlatform(
            "GNU ABI와 glibc host가 필요합니다".to_owned(),
        ));
    }
    #[cfg(target_env = "gnu")]
    {
        unsafe extern "C" {
            fn gnu_get_libc_version() -> *const libc::c_char;
        }
        let version = unsafe { CStr::from_ptr(gnu_get_libc_version()) }
            .to_str()
            .map_err(|_| {
                RuntimePackageError::IncompatiblePlatform(
                    "host glibc version이 UTF-8이 아닙니다".to_owned(),
                )
            })?;
        if compare_component_versions(version, &validated.manifest.platform.libc.minimum_version)?
            == std::cmp::Ordering::Less
        {
            return Err(RuntimePackageError::IncompatiblePlatform(format!(
                "glibc {version}은 minimum {}보다 낮습니다",
                validated.manifest.platform.libc.minimum_version
            )));
        }
        Ok(())
    }
}

fn compare_component_versions(
    actual: &str,
    minimum: &str,
) -> RuntimePackageResult<std::cmp::Ordering> {
    fn components(value: &str) -> RuntimePackageResult<Vec<u32>> {
        value
            .split('.')
            .map(|component| {
                component.parse::<u32>().map_err(|_| {
                    RuntimePackageError::IncompatiblePlatform(format!(
                        "platform version을 해석할 수 없습니다: {value}"
                    ))
                })
            })
            .collect()
    }
    let mut actual = components(actual)?;
    let mut minimum = components(minimum)?;
    let width = actual.len().max(minimum.len());
    actual.resize(width, 0);
    minimum.resize(width, 0);
    Ok(actual.cmp(&minimum))
}

fn io_error(operation: &'static str, path: PathBuf, source: io::Error) -> RuntimePackageError {
    RuntimePackageError::Io {
        operation,
        path,
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    const RESTRICTIVE_UMASK_HELPER_ENV: &str = "TASKCAGE_RESTRICTIVE_UMASK_IMPORT_HELPER";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = NEXT_STAGING.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taskcage-package-test-{name}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = make_tree_writable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_tree_writable(path: &Path) -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
            for entry in fs::read_dir(path)? {
                make_tree_writable(&entry?.path())?;
            }
        } else if metadata.is_file() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
        }
        Ok(())
    }

    fn sha256(bytes: &[u8]) -> String {
        Sha256Digest::from_bytes(Sha256::digest(bytes).into()).to_string()
    }

    fn create_source(root: &Path, executable: &[u8]) -> PathBuf {
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let rootfs = source.join(ROOTFS_NAME);
        fs::create_dir(&rootfs).unwrap();
        fs::create_dir(rootfs.join("bin")).unwrap();
        fs::create_dir(rootfs.join("share")).unwrap();
        let files = [
            ("bin/tool", executable, 0o555),
            ("share/license.txt", b"license".as_slice(), 0o444),
            ("share/sbom.json", b"{}".as_slice(), 0o444),
        ];
        let declarations: Vec<_> = files
            .iter()
            .map(|(path, bytes, mode)| {
                let destination = rootfs.join(path);
                fs::write(&destination, bytes).unwrap();
                fs::set_permissions(&destination, fs::Permissions::from_mode(*mode)).unwrap();
                serde_json::json!({
                    "path": path,
                    "digest": sha256(bytes),
                    "sizeBytes": bytes.len(),
                    "mode": format!("{mode:04o}")
                })
            })
            .collect();
        let manifest = serde_json::json!({
            "schemaVersion": "taskcage.runtime-package/v0alpha1",
            "id": "org.taskcage.tool",
            "version": "1.0.0",
            "platform": {
                "os": "linux",
                "architecture": std::env::consts::ARCH,
                "abi": "gnu",
                "libc": {"family": "glibc", "minimumVersion": "2.0"}
            },
            "entrypoint": "bin/tool",
            "libraryPaths": [],
            "files": declarations,
            "licenses": [{"spdxId": "Apache-2.0", "path": "share/license.txt"}],
            "sbom": {"format": "SPDX-JSON-2.3", "path": "share/sbom.json"}
        });
        fs::write(
            source.join(MANIFEST_NAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        source
    }

    fn create_cache(root: &Path) -> PathBuf {
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        cache
    }

    #[test]
    fn imports_and_reopens_a_verified_package_by_digest() {
        let fixture = TestDirectory::new("roundtrip");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let cache = RuntimePackageCache::open(&cache_root).unwrap();

        let report = cache.import(&source).unwrap();
        assert_eq!(report.outcome, ImportOutcome::Imported);
        let resolved = cache.resolve(report.digest).unwrap();
        assert_eq!(resolved.manifest().id, "org.taskcage.tool");
        assert_eq!(resolved.digest(), report.digest);
        assert!(resolved.entrypoint().metadata().unwrap().ino() > 0);

        let original_entry = cache.entry_path(report.digest);
        let moved_entry = cache.sha256.join("moved-after-resolve");
        fs::rename(&original_entry, &moved_entry).unwrap();
        assert!(cache.resolve(report.digest).is_err());
        let mut pinned = resolved.entrypoint().try_clone().unwrap();
        let mut bytes = Vec::new();
        pinned.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"executable");
        fs::rename(moved_entry, original_entry).unwrap();
    }

    #[test]
    fn rejects_a_package_for_the_other_supported_architecture() {
        let fixture = TestDirectory::new("architecture-mismatch");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let other_architecture = match std::env::consts::ARCH {
            "x86_64" => "aarch64",
            "aarch64" => "x86_64",
            architecture => panic!("unsupported Linux test architecture: {architecture}"),
        };
        let manifest_path = source.join(MANIFEST_NAME);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["platform"]["architecture"] = serde_json::json!(other_architecture);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            RuntimePackageCache::open(&cache_root)
                .unwrap()
                .import(&source),
            Err(RuntimePackageError::IncompatiblePlatform(_))
        ));
    }

    #[test]
    fn import_is_independent_of_a_restrictive_process_umask() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("runtime_package::linux_cache::tests::restrictive_umask_import_helper")
            .arg("--nocapture")
            .env(RESTRICTIVE_UMASK_HELPER_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn restrictive_umask_import_helper() {
        if std::env::var_os(RESTRICTIVE_UMASK_HELPER_ENV).is_none() {
            return;
        }

        let fixture = TestDirectory::new("restrictive-umask");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let cache = RuntimePackageCache::open(&cache_root).unwrap();

        unsafe { libc::umask(0o177) };
        let report = cache.import(&source).unwrap();
        let resolved = cache.resolve(report.digest).unwrap();
        assert_eq!(resolved.manifest().id, "org.taskcage.tool");
        assert_eq!(
            resolved.entrypoint().metadata().unwrap().mode() & 0o777,
            0o555
        );
    }

    #[test]
    fn invalid_content_never_becomes_a_cache_entry() {
        let fixture = TestDirectory::new("invalid");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let tool = source.join(ROOTFS_NAME).join("bin/tool");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&tool, b"tampered!").unwrap();
        fs::set_permissions(tool, fs::Permissions::from_mode(0o555)).unwrap();
        let cache = RuntimePackageCache::open(&cache_root).unwrap();

        assert!(cache.import(&source).is_err());
        let entries: Vec<_> = fs::read_dir(cache_root.join("packages/sha256"))
            .unwrap()
            .collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn symlink_and_hardlink_sources_are_rejected() {
        let fixture = TestDirectory::new("links");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let tool = source.join(ROOTFS_NAME).join("bin/tool");
        let real = source.join(ROOTFS_NAME).join("bin/real");
        fs::rename(&tool, &real).unwrap();
        symlink("real", &tool).unwrap();
        assert!(
            RuntimePackageCache::open(&cache_root)
                .unwrap()
                .import(&source)
                .is_err()
        );

        fs::remove_file(&tool).unwrap();
        fs::hard_link(&real, &tool).unwrap();
        assert!(
            RuntimePackageCache::open(&cache_root)
                .unwrap()
                .import(&source)
                .is_err()
        );
    }

    #[test]
    fn concurrent_imports_publish_one_complete_entry() {
        let fixture = TestDirectory::new("concurrent");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let source = source.clone();
                let cache_root = cache_root.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    RuntimePackageCache::open(&cache_root)
                        .unwrap()
                        .import(&source)
                        .unwrap()
                })
            })
            .collect();
        let reports: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(
            reports
                .iter()
                .filter(|report| report.outcome == ImportOutcome::Imported)
                .count(),
            1
        );
        assert!(
            reports
                .iter()
                .all(|report| report.digest == reports[0].digest)
        );
        let entries: Vec<_> = fs::read_dir(cache_root.join("packages/sha256"))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].file_name().to_string_lossy(),
            reports[0].digest.hex()
        );
        RuntimePackageCache::open(&cache_root)
            .unwrap()
            .resolve(reports[0].digest)
            .unwrap();
    }

    #[test]
    fn corrupt_existing_entry_is_not_overwritten() {
        let fixture = TestDirectory::new("corrupt");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let cache = RuntimePackageCache::open(&cache_root).unwrap();
        let report = cache.import(&source).unwrap();
        let cached_tool = cache
            .entry_path(report.digest)
            .join(ROOTFS_NAME)
            .join("bin/tool");
        fs::set_permissions(&cached_tool, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&cached_tool, b"corrupt!!!").unwrap();

        assert!(cache.import(&source).is_err());
        assert_eq!(fs::read(&cached_tool).unwrap(), b"corrupt!!!");
        let entries: Vec<_> = fs::read_dir(cache_root.join("packages/sha256"))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn undeclared_cached_content_invalidates_resolution() {
        let fixture = TestDirectory::new("extra-cache-content");
        let source = create_source(fixture.path(), b"executable");
        let cache_root = create_cache(fixture.path());
        let cache = RuntimePackageCache::open(&cache_root).unwrap();
        let report = cache.import(&source).unwrap();
        let entry = cache.entry_path(report.digest);
        let extra = entry.join(ROOTFS_NAME).join("bin/extra");
        fs::set_permissions(
            entry.join(ROOTFS_NAME).join("bin"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(&extra, b"extra").unwrap();
        fs::set_permissions(&extra, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(
            entry.join(ROOTFS_NAME).join("bin"),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        assert!(cache.resolve(report.digest).is_err());
    }
}
