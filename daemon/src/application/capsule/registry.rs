//! Capsule resolver chain과 검증된 실행 계약을 조립한다.
//!
//! exact Bundle identity를 먼저 조회하고, Bundle이 설치되지 않은 경우에만 daemon built-in
//! Profile로 fallback한다. Runtime Package 기반 계약은 각 task resolve에서 다시 검증하고
//! entrypoint descriptor를 실행 plan이 만들어질 때까지 고정한다.

use std::collections::BTreeMap;
use std::ffi::OsString;
#[cfg(test)]
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::artifact::{
    ArtifactStoreError, DeclaredOutputArtifact, LocalInputArtifact, StagedArtifactTask,
};
#[cfg(test)]
use crate::bundle::BundleProfile;
use crate::bundle::{BundleError, valid_capsule_name};
use crate::digest::Sha256Digest;
use crate::execution_plan::ResolvedExecutionPlan;
use crate::profile_invocation::VerifiedArgument;
#[cfg(test)]
use crate::protocol::ProfileInputValue;
use crate::protocol::{CommandSpec, ErrorCode, ProfileIdentity, ProfileRequestPayload};
use crate::resource_budget::ResourceBudget;
use crate::runtime_package::RuntimePackageError;

use super::installed::InstalledCapsuleResolver;
#[cfg(test)]
use super::installed::profile_invocation_error;
#[cfg(test)]
use super::legacy::ffmpeg::{
    FFMPEG_CHANNELS, FFMPEG_OUTPUT_FILE, FFMPEG_OUTPUT_MEDIA_TYPE, FFMPEG_OUTPUT_SLOT,
    FFMPEG_PACKAGE_ENTRYPOINT, FFMPEG_PACKAGE_ID, FFMPEG_PROFILE_NAME, FFMPEG_PROFILE_VERSION,
    FFMPEG_SAMPLE_RATES, validate_ffmpeg_inputs,
};
use super::legacy::file_copy::FILE_COPY_PROGRAM;
#[cfg(test)]
use super::legacy::file_copy::{FILE_COPY_PROFILE_NAME, FILE_COPY_PROFILE_VERSION};
use super::legacy::{FfmpegResolver, FileCopyResolver, ffmpeg_arguments};
use super::resolver::{CapsuleResolution, CapsuleResolver};
#[cfg(test)]
use crate::capsule::{CapsuleCatalog, CompiledCapsule};
#[cfg(test)]
use crate::profile_invocation::verify_profile_call;
#[cfg(test)]
use crate::protocol_mapper;

#[derive(Debug)]
pub(crate) struct ProfileRegistry {
    resolvers: Vec<Box<dyn CapsuleResolver>>,
}

/// 설치 상태와 request 계약을 모두 검증한 task-local Profile 해석 결과다.
#[derive(Debug)]
pub(crate) struct ResolvedProfile {
    pub(super) request: ProfileRequestPayload,
    pub(super) source: LocalInputArtifact,
    pub(super) output: DeclaredOutputArtifact,
    pub(super) budget: ResourceBudget,
    pub(super) execution: VerifiedProfileExecution,
    pub(super) output_slot: String,
}

/// Registry가 허용한 실행 형태만 표현한다. caller가 Raw Command fallback을 주입할 수 없다.
#[derive(Debug)]
pub(super) enum VerifiedProfileExecution {
    BuiltInFileCopy,
    LegacyFfmpeg {
        entrypoint: File,
        sample_rate_hz: i64,
        channels: i64,
    },
    Bundle {
        entrypoint: File,
        arguments: Vec<VerifiedArgument>,
    },
}

#[derive(Debug)]
pub(crate) struct StagedProfile {
    request: ProfileRequestPayload,
    budget: ResourceBudget,
    staged: StagedArtifactTask,
    execution: VerifiedProfileExecution,
    output_slot: String,
}

#[derive(Debug, Error)]
pub(crate) enum ProfileStartupError {
    #[error(transparent)]
    Artifact(#[from] ArtifactStoreError),
    #[error(transparent)]
    RuntimePackage(#[from] RuntimePackageError),
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("FFmpeg Runtime Package 계약이 잘못되었습니다: {0}")]
    FfmpegPackageContract(String),
}

#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct ProfileError {
    code: ErrorCode,
    message: String,
}

impl ProfileError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn code(&self) -> ErrorCode {
        self.code
    }
}

impl ProfileRegistry {
    pub(crate) fn open(
        maximum_artifact_bytes: u64,
        default_budget: ResourceBudget,
        ffmpeg_registration: Option<(&Path, Sha256Digest)>,
        bundle_cache_root: Option<&Path>,
    ) -> Result<Self, ProfileStartupError> {
        let file_copy = FileCopyResolver::open(maximum_artifact_bytes, default_budget.clone())?;
        let ffmpeg = ffmpeg_registration
            .map(|(cache_root, digest)| {
                FfmpegResolver::open(
                    cache_root,
                    digest,
                    maximum_artifact_bytes,
                    default_budget.clone(),
                )
            })
            .transpose()?;
        let bundles = bundle_cache_root
            .map(|cache_root| InstalledCapsuleResolver::open(cache_root, maximum_artifact_bytes))
            .transpose()?;
        let mut resolvers: Vec<Box<dyn CapsuleResolver>> = Vec::with_capacity(3);
        if let Some(bundles) = bundles {
            resolvers.push(Box::new(bundles));
        }
        resolvers.push(Box::new(file_copy));
        if let Some(ffmpeg) = ffmpeg {
            resolvers.push(Box::new(ffmpeg));
        }
        Ok(Self { resolvers })
    }

    pub(crate) fn resolve(
        &self,
        request: &ProfileRequestPayload,
    ) -> Result<ResolvedProfile, ProfileError> {
        validate_uuid("clientRequestId", &request.client_request_id)?;
        validate_profile_identity(&request.profile)?;
        for slot in request.inputs.keys() {
            validate_slot_name(slot)?;
        }

        for resolver in &self.resolvers {
            match resolver.resolve(request)? {
                CapsuleResolution::Resolved(profile) => return Ok(*profile),
                CapsuleResolution::NotFound => {}
            }
        }
        Err(ProfileError::new(
            ErrorCode::ProfileNotFound,
            format!(
                "profile {}@{} is not installed",
                request.profile.name, request.profile.version
            ),
        ))
    }
}

impl ResolvedProfile {
    pub(crate) fn budget(&self) -> &ResourceBudget {
        &self.budget
    }

    pub(crate) fn source(&self) -> &LocalInputArtifact {
        &self.source
    }

    pub(crate) fn output(&self) -> DeclaredOutputArtifact {
        self.output.clone()
    }

    pub(crate) fn into_staged(self, staged: StagedArtifactTask) -> StagedProfile {
        StagedProfile {
            request: self.request,
            budget: self.budget,
            staged,
            execution: self.execution,
            output_slot: self.output_slot,
        }
    }
}

impl StagedProfile {
    pub(crate) fn into_plan(
        self,
    ) -> (
        ProfileRequestPayload,
        ResourceBudget,
        StagedArtifactTask,
        ResolvedExecutionPlan,
        String,
    ) {
        let input = self.staged.input_path();
        let output = self.staged.output_path();
        let working_directory = self.staged.working_directory();
        let plan = self.execution.into_plan(
            &self.request.profile.name,
            input,
            output,
            working_directory,
            self.budget.clone(),
        );
        (
            self.request,
            self.budget,
            self.staged,
            plan,
            self.output_slot,
        )
    }
}

impl VerifiedProfileExecution {
    fn into_plan(
        self,
        profile_name: &str,
        input: PathBuf,
        output: PathBuf,
        working_directory: PathBuf,
        budget: ResourceBudget,
    ) -> ResolvedExecutionPlan {
        match self {
            Self::BuiltInFileCopy => {
                let command = CommandSpec {
                    program: FILE_COPY_PROGRAM.to_owned(),
                    args: vec![
                        input.to_string_lossy().into_owned(),
                        output.to_string_lossy().into_owned(),
                    ],
                    working_directory: working_directory.to_string_lossy().into_owned(),
                    environment: BTreeMap::new(),
                };
                ResolvedExecutionPlan::from_validated_raw(&command, budget)
            }
            Self::LegacyFfmpeg {
                entrypoint,
                sample_rate_hz,
                channels,
            } => ResolvedExecutionPlan::from_pinned_entrypoint(
                entrypoint,
                OsString::from("ffmpeg"),
                ffmpeg_arguments(&input, sample_rate_hz, channels, &output),
                working_directory,
                BTreeMap::new(),
                budget,
            ),
            Self::Bundle {
                entrypoint,
                arguments,
            } => ResolvedExecutionPlan::from_pinned_entrypoint(
                entrypoint,
                OsString::from(profile_name),
                arguments
                    .into_iter()
                    .map(|argument| match argument {
                        VerifiedArgument::Literal(value) => OsString::from(value),
                        VerifiedArgument::InputArtifactPath { .. } => input.as_os_str().to_owned(),
                        VerifiedArgument::OutputArtifactPath { .. } => {
                            output.as_os_str().to_owned()
                        }
                    })
                    .collect(),
                working_directory,
                BTreeMap::new(),
                budget,
            ),
        }
    }
}

fn validate_profile_identity(profile: &ProfileIdentity) -> Result<(), ProfileError> {
    if !valid_capsule_name(&profile.name) {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile.name must use dot-separated [a-z][a-z0-9-]* segments (maximum 63 bytes)",
        ));
    }
    if !profile.version.split('.').all(valid_version_component)
        || profile.version.split('.').count() != 3
    {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile.version must be strict MAJOR.MINOR.PATCH",
        ));
    }
    Ok(())
}

fn valid_version_component(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn validate_slot_name(value: &str) -> Result<(), ProfileError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_lowercase()
        || !bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'_' | b'-')
        })
    {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "profile input slot names must match [a-z][a-z0-9_-]{0,63}",
        ));
    }
    Ok(())
}

fn validate_uuid(field: &'static str, value: &str) -> Result<(), ProfileError> {
    let bytes = value.as_bytes();
    if bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        Ok(())
    } else {
        Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            format!("{field} must be a UUID"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use taskcage_core::capsule::{
        ProfileCall, ProfileIdentity as InvocationProfileIdentity,
        ProfileValue as InvocationProfileValue,
    };

    use crate::bundle::test_support::{
        catalog_with_runtime_package, profile_bytes, signed_bundle_archive,
    };
    use crate::deployment_policy::DeploymentResourcePolicy;
    use crate::execution_plan::ResolvedExecutable;
    use crate::protocol::{
        CpuMax, OutputLimits, PartialOutputLimits, PartialResourceLimits, ProfileResourceOverrides,
        ResourceLimits,
    };
    use crate::runtime_package::import_for_service_uid;

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taskcage-profile-registry-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = make_tree_writable(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn make_tree_writable(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(path)? {
                make_tree_writable(&entry?.path())?;
            }
        } else if !metadata.file_type().is_symlink() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn budget() -> ResourceBudget {
        ResourceBudget::try_from_protocol(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 100_000,
                    period_micros: 100_000,
                },
                memory_max_bytes: 512 * 1024 * 1024,
                pids_max: 32,
                wall_time_limit_ms: 120_000,
            },
            OutputLimits {
                stdout_tail_max_bytes: 65_536,
                stderr_tail_max_bytes: 65_536,
            },
        )
        .unwrap()
    }

    fn cache_root(root: &Path, label: &str) -> PathBuf {
        let path = root.join(format!("cache-{label}"));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn package_source(
        root: &Path,
        label: &str,
        id: &str,
        entrypoint: &str,
        executable: &[u8],
    ) -> PathBuf {
        let source = root.join(format!("source-{label}"));
        let entrypoint_path = source.join("rootfs").join(entrypoint);
        let sbom_path = source.join("rootfs/share/sbom.spdx.json");
        fs::create_dir_all(entrypoint_path.parent().unwrap()).unwrap();
        fs::create_dir_all(sbom_path.parent().unwrap()).unwrap();
        fs::write(&entrypoint_path, executable).unwrap();
        fs::set_permissions(&entrypoint_path, fs::Permissions::from_mode(0o555)).unwrap();
        let sbom = br#"{"spdxVersion":"SPDX-2.3"}"#;
        fs::write(&sbom_path, sbom).unwrap();
        fs::set_permissions(&sbom_path, fs::Permissions::from_mode(0o444)).unwrap();
        let manifest = json!({
            "schemaVersion": "taskcage.runtime-package/v0alpha1",
            "id": id,
            "version": "0.0.0-test.1",
            "platform": {
                "os": "linux",
                "architecture": std::env::consts::ARCH,
                "abi": "gnu",
                "libc": { "family": "glibc", "minimumVersion": "2.17" }
            },
            "entrypoint": entrypoint,
            "libraryPaths": [],
            "files": [
                {
                    "path": entrypoint,
                    "digest": sha256(executable),
                    "sizeBytes": executable.len(),
                    "mode": "0555"
                },
                {
                    "path": "share/sbom.spdx.json",
                    "digest": sha256(sbom),
                    "sizeBytes": sbom.len(),
                    "mode": "0444"
                }
            ],
            "licenses": [],
            "sbom": { "format": "SPDX-JSON-2.3", "path": "share/sbom.spdx.json" }
        });
        fs::write(
            source.join("runtime-package.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        source
    }

    fn import_package(
        root: &Path,
        label: &str,
        id: &str,
        entrypoint: &str,
        executable: &[u8],
    ) -> (PathBuf, Sha256Digest) {
        let cache = cache_root(root, label);
        let source = package_source(root, label, id, entrypoint, executable);
        let report = import_for_service_uid(&cache, &source).unwrap();
        (cache, report.digest)
    }

    fn request() -> ProfileRequestPayload {
        ProfileRequestPayload {
            client_request_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            profile: ProfileIdentity {
                name: FFMPEG_PROFILE_NAME.to_owned(),
                version: FFMPEG_PROFILE_VERSION.to_owned(),
            },
            inputs: BTreeMap::from([
                (
                    "source".to_owned(),
                    ProfileInputValue::LocalInput {
                        path: "jobs/42/source.mp3".to_owned(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        size_bytes: 128,
                    },
                ),
                (
                    "sample_rate_hz".to_owned(),
                    ProfileInputValue::Int64 { value: 16_000 },
                ),
                ("channels".to_owned(), ProfileInputValue::Int64 { value: 1 }),
            ]),
            resource_overrides: None,
        }
    }

    fn file_copy_request() -> ProfileRequestPayload {
        ProfileRequestPayload {
            client_request_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            profile: ProfileIdentity {
                name: FILE_COPY_PROFILE_NAME.to_owned(),
                version: FILE_COPY_PROFILE_VERSION.to_owned(),
            },
            inputs: BTreeMap::from([
                (
                    "source".to_owned(),
                    ProfileInputValue::LocalInput {
                        path: "jobs/42/source.txt".to_owned(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        size_bytes: 128,
                    },
                ),
                (
                    "label".to_owned(),
                    ProfileInputValue::String {
                        value: "copy".to_owned(),
                    },
                ),
                (
                    "retain_metadata".to_owned(),
                    ProfileInputValue::Boolean { value: false },
                ),
                (
                    "priority".to_owned(),
                    ProfileInputValue::Int64 { value: 50 },
                ),
            ]),
            resource_overrides: None,
        }
    }

    fn file_copy_bundle_profile() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion": "taskcage.profile/v0alpha1",
            "name": FILE_COPY_PROFILE_NAME,
            "version": FILE_COPY_PROFILE_VERSION,
            "inputs": [
                {"name":"source","kind":"LOCAL_INPUT","required":true},
                {"name":"label","kind":"STRING","required":true},
                {"name":"retain_metadata","kind":"BOOLEAN","required":true},
                {"name":"priority","kind":"INT64","required":true,"minimum":0,"maximum":100}
            ],
            "output": {
                "name":"bundle-result",
                "fileName":"bundle.txt",
                "mediaType":"application/x-bundle-copy",
                "maximumBytes":512
            },
            "argv": [
                "bundle-copy", {"input":"source"}, {"string":"label"},
                {"boolean":"retain_metadata"}, {"int64":"priority"},
                {"output":"bundle-result"}
            ],
            "policy": {
                "limits": {
                    "cpuMax":{"quotaMicros":100000,"periodMicros":100000},
                    "memoryMaxBytes":536870912,
                    "pidsMax":32,
                    "wallTimeLimitMs":120000
                },
                "output":{"stdoutTailMaxBytes":1024,"stderrTailMaxBytes":1024}
            },
            "allowedOverrides":[]
        }))
        .unwrap()
    }

    fn ffmpeg_bundle_profile() -> BundleProfile {
        serde_json::from_slice(&profile_bytes()).unwrap()
    }

    fn compile_bundle_profile(profile: &BundleProfile) -> CompiledCapsule {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let profile_bytes = serde_json::to_vec(profile).unwrap();
        let (archive, keys) = signed_bundle_archive(&profile_bytes, package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        CapsuleCatalog::new(&catalog)
            .compile(&profile.name, &profile.version)
            .unwrap()
    }

    fn verify_bundle_request(
        profile: &BundleProfile,
        request: &ProfileRequestPayload,
    ) -> Result<crate::profile_invocation::VerifiedInvocation, ProfileError> {
        let capsule = compile_bundle_profile(profile);
        verify_profile_call(&capsule, protocol_mapper::profile_call(request))
            .map_err(profile_invocation_error)
    }

    fn set_int64_input(request: &mut ProfileRequestPayload, slot: &str, value: i64) {
        request
            .inputs
            .insert(slot.to_owned(), ProfileInputValue::Int64 { value });
    }

    fn validate_ffmpeg_bundle_inputs(
        request: &ProfileRequestPayload,
    ) -> Result<(LocalInputArtifact, Vec<VerifiedArgument>), ProfileError> {
        let invocation = verify_bundle_request(&ffmpeg_bundle_profile(), request)?;
        let source = invocation
            .input_artifacts()
            .values()
            .next()
            .unwrap()
            .clone();
        Ok((source, invocation.arguments().to_vec()))
    }

    #[derive(Clone, Copy)]
    enum TestResourceOverride {
        Cpu {
            quota_micros: u64,
            period_micros: u64,
        },
        Memory(u64),
        Pids(u64),
        WallTime(u64),
        Stdout(u32),
        Stderr(u32),
    }

    impl TestResourceOverride {
        fn field(self) -> &'static str {
            match self {
                Self::Cpu { .. } => "limits.cpuMax",
                Self::Memory(_) => "limits.memoryMaxBytes",
                Self::Pids(_) => "limits.pidsMax",
                Self::WallTime(_) => "limits.wallTimeLimitMs",
                Self::Stdout(_) => "output.stdoutTailMaxBytes",
                Self::Stderr(_) => "output.stderrTailMaxBytes",
            }
        }

        fn request(self) -> ProfileResourceOverrides {
            let mut limits = PartialResourceLimits::default();
            let mut output = PartialOutputLimits::default();
            match self {
                Self::Cpu {
                    quota_micros,
                    period_micros,
                } => {
                    limits.cpu_max = Some(CpuMax {
                        quota_micros,
                        period_micros,
                    });
                }
                Self::Memory(value) => limits.memory_max_bytes = Some(value),
                Self::Pids(value) => limits.pids_max = Some(value),
                Self::WallTime(value) => limits.wall_time_limit_ms = Some(value),
                Self::Stdout(value) => output.stdout_tail_max_bytes = Some(value),
                Self::Stderr(value) => output.stderr_tail_max_bytes = Some(value),
            }
            ProfileResourceOverrides {
                limits: (limits != PartialResourceLimits::default()).then_some(limits),
                output: (output != PartialOutputLimits::default()).then_some(output),
            }
        }
    }

    fn bundle_profile(allowed_overrides: &[&str]) -> BundleProfile {
        serde_json::from_value(json!({
            "schemaVersion": "taskcage.profile/v0alpha1",
            "name": "ffmpeg-audio-to-wav",
            "version": "1.0.0",
            "inputs": [
                {"name": "source", "kind": "LOCAL_INPUT", "required": true}
            ],
            "output": {
                "name": "audio",
                "fileName": "result.wav",
                "mediaType": "audio/wav",
                "maximumBytes": 1024
            },
            "argv": [{"input": "source"}, {"output": "audio"}],
            "policy": {
                "limits": {
                    "cpuMax": {"quotaMicros": 100, "periodMicros": 100},
                    "memoryMaxBytes": 1000,
                    "pidsMax": 10,
                    "wallTimeLimitMs": 1000
                },
                "output": {
                    "stdoutTailMaxBytes": 1000,
                    "stderrTailMaxBytes": 1000
                }
            },
            "allowedOverrides": allowed_overrides
        }))
        .unwrap()
    }

    fn resolve_bundle_budget(
        profile: &BundleProfile,
        overrides: Option<&ProfileResourceOverrides>,
    ) -> Result<ResourceBudget, ProfileError> {
        let inputs = profile
            .inputs
            .iter()
            .map(|input| {
                let value = match input.kind.as_str() {
                    "LOCAL_INPUT" => ProfileInputValue::LocalInput {
                        path: "jobs/42/source.bin".to_owned(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        size_bytes: 128,
                    },
                    "STRING" => ProfileInputValue::String {
                        value: "value".to_owned(),
                    },
                    "BOOLEAN" => ProfileInputValue::Boolean { value: false },
                    "INT64" => ProfileInputValue::Int64 {
                        value: input
                            .allowed_values
                            .as_ref()
                            .and_then(|values| values.first().copied())
                            .or(input.minimum)
                            .expect("test INT64 input must have a validation contract"),
                    },
                    kind => panic!("unsupported test input kind {kind}"),
                };
                (input.name.clone(), value)
            })
            .collect();
        let request = ProfileRequestPayload {
            client_request_id: "33333333-3333-4333-8333-333333333333".to_owned(),
            profile: ProfileIdentity {
                name: profile.name.clone(),
                version: profile.version.clone(),
            },
            inputs,
            resource_overrides: overrides.cloned(),
        };
        verify_bundle_request(profile, &request)
            .map(|invocation| invocation.effective_resources().clone())
    }

    fn all_bundle_override_fields() -> [&'static str; 6] {
        [
            "limits.cpuMax",
            "limits.memoryMaxBytes",
            "limits.pidsMax",
            "limits.wallTimeLimitMs",
            "output.stdoutTailMaxBytes",
            "output.stderrTailMaxBytes",
        ]
    }

    #[test]
    fn exact_bundle_identity_precedes_static_and_legacy_profiles() {
        let (bundle_root, catalog, package_digest) = catalog_with_runtime_package();
        for profile in [file_copy_bundle_profile(), profile_bytes()] {
            let (archive, keys) = signed_bundle_archive(&profile, package_digest);
            catalog.import(archive.path(), &keys).unwrap();
        }
        let (legacy_cache, legacy_digest) = import_package(
            bundle_root.path(),
            "legacy-ffmpeg",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"legacy-ffmpeg",
        );
        let registry = ProfileRegistry::open(
            1024,
            budget(),
            Some((&legacy_cache, legacy_digest)),
            Some(&bundle_root.path().join("cache")),
        )
        .unwrap();

        assert!(matches!(
            registry.resolve(&file_copy_request()).unwrap().execution,
            VerifiedProfileExecution::Bundle { .. }
        ));
        assert!(matches!(
            registry.resolve(&request()).unwrap().execution,
            VerifiedProfileExecution::Bundle { .. }
        ));
    }

    #[test]
    fn namespaced_bundle_identity_resolves_through_the_public_profile_gate() {
        let (bundle_root, catalog, package_digest) = catalog_with_runtime_package();
        let mut profile: serde_json::Value = serde_json::from_slice(&profile_bytes()).unwrap();
        profile["name"] = json!("media.extract-audio");
        let profile = serde_json::to_vec(&profile).unwrap();
        let (archive, keys) = signed_bundle_archive(&profile, package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        let registry = ProfileRegistry::open(
            1024,
            budget(),
            None,
            Some(&bundle_root.path().join("cache")),
        )
        .unwrap();
        let mut request = request();
        request.profile.name = "media.extract-audio".to_owned();

        assert!(matches!(
            registry.resolve(&request).unwrap().execution,
            VerifiedProfileExecution::Bundle { .. }
        ));
    }

    #[test]
    fn bundle_not_found_falls_back_to_file_copy_and_legacy_ffmpeg() {
        let (bundle_root, _catalog, _package_digest) = catalog_with_runtime_package();
        let (legacy_cache, legacy_digest) = import_package(
            bundle_root.path(),
            "fallback-ffmpeg",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"legacy-ffmpeg",
        );
        let registry = ProfileRegistry::open(
            1024,
            budget(),
            Some((&legacy_cache, legacy_digest)),
            Some(&bundle_root.path().join("cache")),
        )
        .unwrap();

        assert!(matches!(
            registry.resolve(&file_copy_request()).unwrap().execution,
            VerifiedProfileExecution::BuiltInFileCopy
        ));
        assert!(matches!(
            registry.resolve(&request()).unwrap().execution,
            VerifiedProfileExecution::LegacyFfmpeg { .. }
        ));
    }

    #[test]
    fn corrupt_or_unreadable_bundle_mapping_never_falls_back_to_static_identity() {
        let (bundle_root, _catalog, _package_digest) = catalog_with_runtime_package();
        let cache = bundle_root.path().join("cache");
        let registry = ProfileRegistry::open(1024, budget(), None, Some(&cache)).unwrap();
        let identity_directory = cache.join("bundles/catalog").join(FILE_COPY_PROFILE_NAME);
        fs::create_dir(&identity_directory).unwrap();
        fs::set_permissions(&identity_directory, fs::Permissions::from_mode(0o755)).unwrap();
        let mapping = identity_directory.join(format!("{FILE_COPY_PROFILE_VERSION}.json"));
        fs::write(&mapping, b"{").unwrap();
        fs::set_permissions(&mapping, fs::Permissions::from_mode(0o444)).unwrap();

        assert_eq!(
            registry.resolve(&file_copy_request()).unwrap_err().code(),
            ErrorCode::EnvironmentUnavailable
        );

        fs::set_permissions(&mapping, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(&mapping).unwrap();
        let target = bundle_root.path().join("unreadable-mapping");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, &mapping).unwrap();
        assert_eq!(
            registry.resolve(&file_copy_request()).unwrap_err().code(),
            ErrorCode::EnvironmentUnavailable
        );
    }

    #[test]
    fn unknown_identity_is_profile_not_found() {
        let registry = ProfileRegistry::open(1024, budget(), None, None).unwrap();
        let mut unknown = file_copy_request();
        unknown.profile.name = "not-installed".to_owned();

        assert_eq!(
            registry.resolve(&unknown).unwrap_err().code(),
            ErrorCode::ProfileNotFound
        );
    }

    #[test]
    fn bundle_package_corruption_fails_new_task_resolution_before_plan_creation() {
        let (bundle_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(&profile_bytes(), package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        let cache = bundle_root.path().join("cache");
        let registry = ProfileRegistry::open(1024, budget(), None, Some(&cache)).unwrap();
        assert!(registry.resolve(&request()).is_ok());

        let entrypoint = cache
            .join("packages/sha256")
            .join(package_digest.hex())
            .join("rootfs/bin/tool");
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&entrypoint, b"corrupted-package").unwrap();
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o555)).unwrap();

        assert_eq!(
            registry.resolve(&request()).unwrap_err().code(),
            ErrorCode::EnvironmentUnavailable
        );
    }

    #[test]
    fn file_copy_and_ffmpeg_final_plans_preserve_argv_and_output_contracts() {
        let fixture = TestDirectory::new("plans");
        let (cache, digest) = import_package(
            fixture.path(),
            "plans",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"verified-package",
        );
        let registry = ProfileRegistry::open(1024, budget(), Some((&cache, digest)), None).unwrap();
        let input = PathBuf::from("/var/lib/taskcage/staging/task/artifacts/in/source");
        let working_directory = PathBuf::from("/var/lib/taskcage/staging/task");

        let file_copy = registry.resolve(&file_copy_request()).unwrap();
        assert_eq!(file_copy.output.file_name(), "result.txt");
        assert_eq!(file_copy.output.media_type(), "text/plain");
        assert_eq!(file_copy.output_slot, "result");
        let file_copy_output =
            PathBuf::from("/var/lib/taskcage/staging/task/artifacts/out/result.txt");
        let (command, _) = file_copy
            .execution
            .into_plan(
                FILE_COPY_PROFILE_NAME,
                input.clone(),
                file_copy_output.clone(),
                working_directory.clone(),
                file_copy.budget,
            )
            .into_parts();
        let (executable, arguments, actual_working_directory, environment) = command.into_parts();
        assert!(matches!(
            executable,
            ResolvedExecutable::Path(value) if value == FILE_COPY_PROGRAM
        ));
        assert_eq!(
            arguments,
            vec![
                input.as_os_str().to_owned(),
                file_copy_output.as_os_str().to_owned()
            ]
        );
        assert_eq!(actual_working_directory, working_directory);
        assert!(environment.is_empty());

        let ffmpeg = registry.resolve(&request()).unwrap();
        assert_eq!(ffmpeg.output.file_name(), "result.wav");
        assert_eq!(ffmpeg.output.media_type(), "audio/wav");
        assert_eq!(ffmpeg.output_slot, "audio");
        let ffmpeg_output =
            PathBuf::from("/var/lib/taskcage/staging/task/artifacts/out/result.wav");
        let expected_arguments = ffmpeg_arguments(&input, 16_000, 1, &ffmpeg_output);
        let (command, _) = ffmpeg
            .execution
            .into_plan(
                FFMPEG_PROFILE_NAME,
                input,
                ffmpeg_output,
                working_directory.clone(),
                ffmpeg.budget,
            )
            .into_parts();
        let (executable, arguments, actual_working_directory, environment) = command.into_parts();
        assert!(matches!(
            executable,
            ResolvedExecutable::Pinned { argv0, .. } if argv0 == "ffmpeg"
        ));
        assert_eq!(arguments, expected_arguments);
        assert_eq!(actual_working_directory, working_directory);
        assert!(environment.is_empty());
    }

    #[test]
    fn bundle_policy_values_are_the_default_without_overrides() {
        let resolved = resolve_bundle_budget(&bundle_profile(&[]), None).unwrap();

        assert_eq!(
            resolved.protocol_limits(),
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 100,
                    period_micros: 100,
                },
                memory_max_bytes: 1000,
                pids_max: 10,
                wall_time_limit_ms: 1000,
            }
        );
        assert_eq!(
            resolved.protocol_output(),
            OutputLimits {
                stdout_tail_max_bytes: 1000,
                stderr_tail_max_bytes: 1000,
            }
        );
    }

    #[test]
    fn bundle_rejects_each_override_missing_from_the_allowlist() {
        let profile = bundle_profile(&[]);
        let overrides = [
            TestResourceOverride::Cpu {
                quota_micros: 100,
                period_micros: 100,
            },
            TestResourceOverride::Memory(1000),
            TestResourceOverride::Pids(10),
            TestResourceOverride::WallTime(1000),
            TestResourceOverride::Stdout(1000),
            TestResourceOverride::Stderr(1000),
        ];

        for value in overrides {
            let error = resolve_bundle_budget(&profile, Some(&value.request())).unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::LimitExceedsPolicy,
                "{}",
                value.field()
            );
        }
    }

    #[test]
    fn bundle_accepts_each_override_equal_to_its_policy_maximum() {
        let profile = bundle_profile(&all_bundle_override_fields());
        for value in [
            TestResourceOverride::Cpu {
                quota_micros: 100,
                period_micros: 100,
            },
            TestResourceOverride::Memory(1000),
            TestResourceOverride::Pids(10),
            TestResourceOverride::WallTime(1000),
            TestResourceOverride::Stdout(1000),
            TestResourceOverride::Stderr(1000),
        ] {
            assert!(
                resolve_bundle_budget(&profile, Some(&value.request())).is_ok(),
                "{}",
                value.field()
            );
        }
    }

    #[test]
    fn bundle_accepts_each_more_restrictive_override() {
        let profile = bundle_profile(&all_bundle_override_fields());
        for value in [
            TestResourceOverride::Cpu {
                quota_micros: 50,
                period_micros: 100,
            },
            TestResourceOverride::Memory(999),
            TestResourceOverride::Pids(9),
            TestResourceOverride::WallTime(999),
            TestResourceOverride::Stdout(999),
            TestResourceOverride::Stderr(999),
        ] {
            assert!(
                resolve_bundle_budget(&profile, Some(&value.request())).is_ok(),
                "{}",
                value.field()
            );
        }
    }

    #[test]
    fn bundle_rejects_each_override_above_its_policy_maximum() {
        let profile = bundle_profile(&all_bundle_override_fields());
        for value in [
            TestResourceOverride::Cpu {
                quota_micros: 101,
                period_micros: 100,
            },
            TestResourceOverride::Memory(1001),
            TestResourceOverride::Pids(11),
            TestResourceOverride::WallTime(1001),
            TestResourceOverride::Stdout(1001),
            TestResourceOverride::Stderr(1001),
        ] {
            let error = resolve_bundle_budget(&profile, Some(&value.request())).unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::LimitExceedsPolicy,
                "{}",
                value.field()
            );
        }
    }

    #[test]
    fn bundle_compares_cpu_ratios_exactly_without_floating_point() {
        let profile = bundle_profile(&["limits.cpuMax"]);
        let equal_ratio = TestResourceOverride::Cpu {
            quota_micros: 200,
            period_micros: 200,
        }
        .request();
        assert!(resolve_bundle_budget(&profile, Some(&equal_ratio)).is_ok());

        let above_ratio = TestResourceOverride::Cpu {
            quota_micros: 201,
            period_micros: 200,
        }
        .request();
        assert_eq!(
            resolve_bundle_budget(&profile, Some(&above_ratio))
                .unwrap_err()
                .code(),
            ErrorCode::LimitExceedsPolicy
        );
    }

    #[test]
    fn bundle_keeps_empty_and_invalid_numeric_overrides_as_profile_input_errors() {
        let profile = bundle_profile(&all_bundle_override_fields());
        let empty = ProfileResourceOverrides {
            limits: Some(PartialResourceLimits::default()),
            output: None,
        };
        assert_eq!(
            resolve_bundle_budget(&profile, Some(&empty))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );

        let zero = TestResourceOverride::Memory(0).request();
        assert_eq!(
            resolve_bundle_budget(&profile, Some(&zero))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );
    }

    #[test]
    fn deployment_maximum_is_checked_after_the_bundle_maximum() {
        let profile = bundle_profile(&["limits.memoryMaxBytes"]);
        let requested =
            resolve_bundle_budget(&profile, Some(&TestResourceOverride::Memory(900).request()))
                .unwrap();
        let deployment = DeploymentResourcePolicy::try_new(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: 100,
                    period_micros: 100,
                },
                memory_max_bytes: 800,
                pids_max: 10,
                wall_time_limit_ms: 1000,
            },
            OutputLimits {
                stdout_tail_max_bytes: 1000,
                stderr_tail_max_bytes: 1000,
            },
        )
        .unwrap();

        assert!(deployment.validate(&requested).is_err());
    }

    #[test]
    fn bundle_policy_rejection_precedes_all_execution_side_effects() {
        let profile = bundle_profile(&[]);
        let artifact_staging = Cell::new(0);
        let task_records = Cell::new(0);
        let cgroup_creations = Cell::new(0);
        let target_starts = Cell::new(0);

        let result = resolve_bundle_budget(
            &profile,
            Some(&TestResourceOverride::Memory(1000).request()),
        )
        .map(|_| {
            artifact_staging.set(artifact_staging.get() + 1);
            task_records.set(task_records.get() + 1);
            cgroup_creations.set(cgroup_creations.get() + 1);
            target_starts.set(target_starts.get() + 1);
        });

        assert_eq!(result.unwrap_err().code(), ErrorCode::LimitExceedsPolicy);
        assert_eq!(artifact_staging.get(), 0);
        assert_eq!(task_records.get(), 0);
        assert_eq!(cgroup_creations.get(), 0);
        assert_eq!(target_starts.get(), 0);
    }

    #[test]
    fn profile_slot_names_follow_the_lowercase_wire_contract() {
        assert!(validate_slot_name("retain_metadata").is_ok());
        assert!(validate_slot_name("output-2").is_ok());
        assert!(validate_slot_name("retainMetadata").is_err());
        assert!(validate_slot_name("Output").is_err());
    }

    #[test]
    fn profile_names_use_the_capsule_namespaced_contract() {
        let profile = |name: &str| ProfileIdentity {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
        };

        assert!(validate_profile_identity(&profile("ffmpeg-audio-to-wav")).is_ok());
        assert!(validate_profile_identity(&profile("media.extract-audio")).is_ok());
        for invalid in [
            "Media.extract-audio",
            ".media",
            "media.",
            "media..extract-audio",
            "media.extract_audio",
        ] {
            assert!(
                validate_profile_identity(&profile(invalid)).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn static_and_bundle_ffmpeg_profiles_share_the_sample_rate_allowlist() {
        for rate in FFMPEG_SAMPLE_RATES {
            let mut request = request();
            set_int64_input(&mut request, "sample_rate_hz", *rate);
            assert!(validate_ffmpeg_inputs(&request).is_ok(), "static {rate}");
            assert!(
                validate_ffmpeg_bundle_inputs(&request).is_ok(),
                "Bundle {rate}"
            );
        }

        let mut unsupported = request();
        set_int64_input(&mut unsupported, "sample_rate_hz", 12_345);
        assert_eq!(
            validate_ffmpeg_inputs(&unsupported).unwrap_err().code(),
            ErrorCode::InvalidProfileInput
        );
        assert_eq!(
            validate_ffmpeg_bundle_inputs(&unsupported)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );
    }

    #[test]
    fn static_and_bundle_ffmpeg_profiles_share_the_channel_allowlist() {
        for channels in FFMPEG_CHANNELS {
            let mut request = request();
            set_int64_input(&mut request, "channels", *channels);
            assert!(
                validate_ffmpeg_inputs(&request).is_ok(),
                "static {channels}"
            );
            assert!(
                validate_ffmpeg_bundle_inputs(&request).is_ok(),
                "Bundle {channels}"
            );
        }

        let mut unsupported = request();
        set_int64_input(&mut unsupported, "channels", 3);
        assert_eq!(
            validate_ffmpeg_inputs(&unsupported).unwrap_err().code(),
            ErrorCode::InvalidProfileInput
        );
        assert_eq!(
            validate_ffmpeg_bundle_inputs(&unsupported)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );
    }

    #[test]
    fn identical_ffmpeg_requests_get_the_same_static_and_bundle_input_decision() {
        for (sample_rate_hz, channels) in [
            (8_000, 1),
            (48_000, 2),
            (12_345, 1),
            (16_000, 3),
            (12_345, 3),
        ] {
            let mut request = request();
            set_int64_input(&mut request, "sample_rate_hz", sample_rate_hz);
            set_int64_input(&mut request, "channels", channels);

            let static_result = validate_ffmpeg_inputs(&request);
            let bundle_result = validate_ffmpeg_bundle_inputs(&request);
            assert_eq!(
                static_result.is_ok(),
                bundle_result.is_ok(),
                "sample_rate_hz={sample_rate_hz}, channels={channels}"
            );
            if let (Err(static_error), Err(bundle_error)) = (static_result, bundle_result) {
                assert_eq!(static_error.code(), ErrorCode::InvalidProfileInput);
                assert_eq!(bundle_error.code(), ErrorCode::InvalidProfileInput);
            }
        }
    }

    #[test]
    fn ffmpeg_inputs_reject_unsupported_values_and_wrong_slot_sets() {
        let mut unsupported_rate = request();
        unsupported_rate.inputs.insert(
            "sample_rate_hz".to_owned(),
            ProfileInputValue::Int64 { value: 96_000 },
        );
        assert_eq!(
            validate_ffmpeg_inputs(&unsupported_rate)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );

        let mut unsupported_channels = request();
        unsupported_channels
            .inputs
            .insert("channels".to_owned(), ProfileInputValue::Int64 { value: 6 });
        assert_eq!(
            validate_ffmpeg_inputs(&unsupported_channels)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidProfileInput
        );

        let mut missing = request();
        missing.inputs.remove("source");
        assert_eq!(
            validate_ffmpeg_inputs(&missing).unwrap_err().code(),
            ErrorCode::InvalidProfileInput
        );

        let mut unexpected = request();
        unexpected.inputs.insert(
            "output".to_owned(),
            ProfileInputValue::String {
                value: "caller.wav".to_owned(),
            },
        );
        assert_eq!(
            validate_ffmpeg_inputs(&unexpected).unwrap_err().code(),
            ErrorCode::InvalidProfileInput
        );
    }

    #[test]
    fn ffmpeg_argv_and_output_contract_are_daemon_owned_and_deterministic() {
        let input =
            Path::new("/var/lib/taskcage/artifacts/.taskcage/staging/task/artifacts/in/source");
        let output = Path::new(
            "/var/lib/taskcage/artifacts/.taskcage/staging/task/artifacts/out/result.wav",
        );
        let expected_argv = vec![
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-i",
            input.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "pcm_s16le",
            "-ar",
            "16000",
            "-ac",
            "1",
            output.to_str().unwrap(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        assert_eq!(ffmpeg_arguments(input, 16_000, 1, output), expected_argv);

        let bundle_profile = ffmpeg_bundle_profile();
        let verified = verify_bundle_request(&bundle_profile, &request()).unwrap();
        let materialized = crate::profile_mapper::materialize_invocation(
            verified,
            crate::profile_mapper::ArtifactBindings::new(
                [("source", input.to_path_buf())],
                [("audio", output.to_path_buf())],
                PathBuf::from("/var/lib/taskcage/artifacts/.taskcage/staging/task"),
            ),
        )
        .unwrap();
        assert_eq!(materialized.arguments(), expected_argv);

        let declared =
            DeclaredOutputArtifact::new(FFMPEG_OUTPUT_FILE, FFMPEG_OUTPUT_MEDIA_TYPE, 1024)
                .unwrap();
        assert_eq!(FFMPEG_OUTPUT_SLOT, "audio");
        assert_eq!(declared.file_name(), "result.wav");
        assert_eq!(declared.media_type(), "audio/wav");
        assert_eq!(bundle_profile.output.name, FFMPEG_OUTPUT_SLOT);
        assert_eq!(bundle_profile.output.file_name, FFMPEG_OUTPUT_FILE);
        assert_eq!(bundle_profile.output.media_type, FFMPEG_OUTPUT_MEDIA_TYPE);
    }

    #[test]
    fn typed_bundle_arguments_bind_request_values_without_json_shape_matching() {
        let profile: BundleProfile = serde_json::from_slice(&file_copy_bundle_profile()).unwrap();
        let arguments = verify_bundle_request(&profile, &file_copy_request())
            .unwrap()
            .arguments()
            .to_vec();

        assert_eq!(
            arguments,
            vec![
                VerifiedArgument::Literal("bundle-copy".to_owned()),
                VerifiedArgument::InputArtifactPath {
                    slot: "source".to_owned(),
                },
                VerifiedArgument::Literal("copy".to_owned()),
                VerifiedArgument::Literal("false".to_owned()),
                VerifiedArgument::Literal("50".to_owned()),
                VerifiedArgument::OutputArtifactPath {
                    slot: "bundle-result".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn protocol_adapter_and_direct_domain_call_have_identical_verification_meaning() {
        let profile = ffmpeg_bundle_profile();
        let capsule = compile_bundle_profile(&profile);
        let wire = request();
        let adapted = verify_profile_call(&capsule, protocol_mapper::profile_call(&wire)).unwrap();
        let direct = verify_profile_call(
            &capsule,
            ProfileCall::new(
                InvocationProfileIdentity::new(FFMPEG_PROFILE_NAME, FFMPEG_PROFILE_VERSION),
                vec![
                    (
                        "source",
                        InvocationProfileValue::LocalInput {
                            path: "jobs/42/source.mp3".to_owned(),
                            digest: format!("sha256:{}", "0".repeat(64)),
                            size_bytes: 128,
                        },
                    ),
                    ("sample_rate_hz", InvocationProfileValue::Int64(16_000)),
                    ("channels", InvocationProfileValue::Int64(1)),
                ],
            ),
        )
        .unwrap();

        assert_eq!(adapted.profile_identity(), direct.profile_identity());
        assert_eq!(adapted.values(), direct.values());
        assert_eq!(adapted.input_artifacts(), direct.input_artifacts());
        assert_eq!(adapted.arguments(), direct.arguments());
        assert_eq!(
            adapted.effective_resources().protocol_limits(),
            direct.effective_resources().protocol_limits()
        );
        assert_eq!(
            adapted.effective_resources().protocol_output(),
            direct.effective_resources().protocol_output()
        );
    }

    #[test]
    fn capsule_adapter_preserves_invalid_artifact_path_wire_classification() {
        let profile = ffmpeg_bundle_profile();
        let capsule = compile_bundle_profile(&profile);
        let mut wire = request();
        wire.inputs.insert(
            "source".to_owned(),
            ProfileInputValue::LocalInput {
                path: "../source.mp3".to_owned(),
                digest: format!("sha256:{}", "0".repeat(64)),
                size_bytes: 128,
            },
        );

        let error = verify_profile_call(&capsule, protocol_mapper::profile_call(&wire))
            .map_err(profile_invocation_error)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArtifactPath);
    }

    #[test]
    fn ffmpeg_registration_rejects_missing_corrupt_and_wrong_contract_packages() {
        let fixture = TestDirectory::new("registration");

        let missing_cache = cache_root(fixture.path(), "missing");
        let missing_digest = Sha256Digest::from_bytes([7; 32]);
        assert!(matches!(
            ProfileRegistry::open(1024, budget(), Some((&missing_cache, missing_digest)), None,),
            Err(ProfileStartupError::RuntimePackage(_))
        ));

        let (wrong_id_cache, wrong_id_digest) = import_package(
            fixture.path(),
            "wrong-id",
            "org.taskcage.not-ffmpeg",
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"wrong-id-package",
        );
        assert!(matches!(
            ProfileRegistry::open(
                1024,
                budget(),
                Some((&wrong_id_cache, wrong_id_digest)),
                None,
            ),
            Err(ProfileStartupError::FfmpegPackageContract(_))
        ));

        let (wrong_entry_cache, wrong_entry_digest) = import_package(
            fixture.path(),
            "wrong-entry",
            FFMPEG_PACKAGE_ID,
            "bin/not-ffmpeg",
            b"wrong-entry-package",
        );
        assert!(matches!(
            ProfileRegistry::open(
                1024,
                budget(),
                Some((&wrong_entry_cache, wrong_entry_digest)),
                None,
            ),
            Err(ProfileStartupError::FfmpegPackageContract(_))
        ));

        let (corrupt_cache, corrupt_digest) = import_package(
            fixture.path(),
            "corrupt",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"verified-package",
        );
        let cached_entrypoint = corrupt_cache
            .join("packages/sha256")
            .join(corrupt_digest.hex())
            .join("rootfs")
            .join(FFMPEG_PACKAGE_ENTRYPOINT);
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&cached_entrypoint, b"corrupted-package").unwrap();
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(matches!(
            ProfileRegistry::open(1024, budget(), Some((&corrupt_cache, corrupt_digest)), None,),
            Err(ProfileStartupError::RuntimePackage(_))
        ));
    }

    #[test]
    fn ffmpeg_package_is_reverified_for_each_new_task() {
        let fixture = TestDirectory::new("reresolve");
        let (cache, digest) = import_package(
            fixture.path(),
            "reresolve",
            FFMPEG_PACKAGE_ID,
            FFMPEG_PACKAGE_ENTRYPOINT,
            b"verified-package",
        );
        let registry = ProfileRegistry::open(1024, budget(), Some((&cache, digest)), None).unwrap();
        assert!(registry.resolve(&request()).is_ok());

        let cached_entrypoint = cache
            .join("packages/sha256")
            .join(digest.hex())
            .join("rootfs")
            .join(FFMPEG_PACKAGE_ENTRYPOINT);
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&cached_entrypoint, b"corrupted-package").unwrap();
        fs::set_permissions(&cached_entrypoint, fs::Permissions::from_mode(0o555)).unwrap();
        let error = registry.resolve(&request()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::EnvironmentUnavailable);
    }
}
