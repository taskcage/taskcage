//! Deterministic, side-effect-free materialization of a verified Profile invocation.
//!
//! Runtime code supplies daemon-owned staged paths through `ArtifactBindings`. This module binds
//! those paths to typed argument placeholders without opening artifacts, resolving Runtime
//! Packages, inheriting environment, creating an execution plan, or touching process state.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::capsule::{CapsuleIdentity, CapsuleOutput, CapsuleRuntimeReference};
use crate::profile_invocation::{ProfileIdentity, VerifiedArgument, VerifiedInvocation};
use crate::resource_budget::ResourceBudget;

/// Logical namespace of a runtime-owned Artifact binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactBindingKind {
    Input,
    Output,
}

impl fmt::Display for ArtifactBindingKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Input => "input",
            Self::Output => "output",
        })
    }
}

/// Runtime-owned staged paths supplied after Artifact snapshot/staging.
///
/// The caller is responsible for constructing every path from the runtime staging capability;
/// this side-effect-free value does not inspect the filesystem or establish path provenance.
/// Pairs retain their original multiplicity so the materializer can reject duplicate logical
/// slots instead of silently overwriting them in a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBindings {
    input_paths: Vec<(String, PathBuf)>,
    output_paths: Vec<(String, PathBuf)>,
    working_directory: PathBuf,
}

impl ArtifactBindings {
    pub fn new<I, O, IS, IP, OS, OP, W>(
        input_paths: I,
        output_paths: O,
        working_directory: W,
    ) -> Self
    where
        I: IntoIterator<Item = (IS, IP)>,
        O: IntoIterator<Item = (OS, OP)>,
        IS: Into<String>,
        IP: Into<PathBuf>,
        OS: Into<String>,
        OP: Into<PathBuf>,
        W: Into<PathBuf>,
    {
        Self {
            input_paths: input_paths
                .into_iter()
                .map(|(slot, path)| (slot.into(), path.into()))
                .collect(),
            output_paths: output_paths
                .into_iter()
                .map(|(slot, path)| (slot.into(), path.into()))
                .collect(),
            working_directory: working_directory.into(),
        }
    }

    pub fn input_paths(&self) -> impl ExactSizeIterator<Item = (&str, &Path)> {
        self.input_paths
            .iter()
            .map(|(slot, path)| (slot.as_str(), path.as_path()))
    }

    pub fn output_paths(&self) -> impl ExactSizeIterator<Item = (&str, &Path)> {
        self.output_paths
            .iter()
            .map(|(slot, path)| (slot.as_str(), path.as_path()))
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

/// Fully bound Profile command data, still independent of executable pinning and execution.
///
/// `runtime` preserves the Capsule Runtime Package ID and digest. Bundle v0alpha1 has no separate
/// entrypoint string; the package digest transitively commits to the manifest entrypoint that the
/// Developer B adapter must resolve and pin.
#[derive(Debug, Clone)]
pub struct MaterializedInvocation {
    capsule_identity: CapsuleIdentity,
    profile_identity: ProfileIdentity,
    runtime: CapsuleRuntimeReference,
    argv0: OsString,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    effective_resources: ResourceBudget,
    declared_output: CapsuleOutput,
}

impl MaterializedInvocation {
    pub fn capsule_identity(&self) -> &CapsuleIdentity {
        &self.capsule_identity
    }

    pub fn profile_identity(&self) -> &ProfileIdentity {
        &self.profile_identity
    }

    pub fn runtime(&self) -> &CapsuleRuntimeReference {
        &self.runtime
    }

    pub fn argv0(&self) -> &OsStr {
        &self.argv0
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub fn effective_resources(&self) -> &ResourceBudget {
        &self.effective_resources
    }

    pub fn declared_output(&self) -> &CapsuleOutput {
        &self.declared_output
    }

    /// Transfers the owned command components required by Developer B's pinned-plan adapter.
    pub fn into_execution_parts(self) -> (OsString, Vec<OsString>, PathBuf, ResourceBudget) {
        (
            self.argv0,
            self.arguments,
            self.working_directory,
            self.effective_resources,
        )
    }
}

/// Stable domain classification for failed staged-path materialization.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MaterializationError {
    #[error("duplicate {kind} Artifact binding for slot {slot}")]
    DuplicateBinding {
        kind: ArtifactBindingKind,
        slot: String,
    },
    #[error(
        "Artifact binding set mismatch: missingInputs={missing_inputs:?}, unknownInputs={unknown_inputs:?}, missingOutputs={missing_outputs:?}, unknownOutputs={unknown_outputs:?}"
    )]
    BindingSetMismatch {
        missing_inputs: Vec<String>,
        unknown_inputs: Vec<String>,
        missing_outputs: Vec<String>,
        unknown_outputs: Vec<String>,
    },
    #[error("{kind} Artifact binding for slot {slot} must be an absolute path")]
    NonAbsoluteBinding {
        kind: ArtifactBindingKind,
        slot: String,
    },
    #[error("{kind} Artifact binding for slot {slot} contains NUL")]
    NulBinding {
        kind: ArtifactBindingKind,
        slot: String,
    },
    #[error("working directory must be an absolute path")]
    NonAbsoluteWorkingDirectory,
    #[error("argv0 contains NUL")]
    NulArgv0,
    #[error("argument token at index {index} contains NUL")]
    NulArgument { index: usize },
    #[error("working directory contains NUL")]
    NulWorkingDirectory,
    #[error("VerifiedInvocation invariant is invalid: {reason}")]
    InvalidVerifiedInvocation { reason: String },
}

pub type MaterializationResult<T> = std::result::Result<T, MaterializationError>;

/// Binds runtime-owned Artifact paths to one verified argument template in exact mapping order.
pub fn materialize_invocation(
    invocation: VerifiedInvocation,
    bindings: ArtifactBindings,
) -> MaterializationResult<MaterializedInvocation> {
    let ArtifactBindings {
        input_paths,
        output_paths,
        working_directory,
    } = bindings;
    let input_paths = collect_bindings(ArtifactBindingKind::Input, input_paths)?;
    let output_paths = collect_bindings(ArtifactBindingKind::Output, output_paths)?;
    validate_binding_sets(&invocation, &input_paths, &output_paths)?;
    validate_absolute_paths(ArtifactBindingKind::Input, &input_paths)?;
    validate_absolute_paths(ArtifactBindingKind::Output, &output_paths)?;
    if !working_directory.is_absolute() {
        return Err(MaterializationError::NonAbsoluteWorkingDirectory);
    }
    if contains_nul(working_directory.as_os_str()) {
        return Err(MaterializationError::NulWorkingDirectory);
    }

    let argv0 = OsString::from(invocation.profile_identity().name());
    if contains_nul(&argv0) {
        return Err(MaterializationError::NulArgv0);
    }

    let mut arguments = Vec::with_capacity(invocation.arguments().len());
    for (index, argument) in invocation.arguments().iter().enumerate() {
        let token = match argument {
            VerifiedArgument::Literal(value) => OsString::from(value),
            VerifiedArgument::InputArtifactPath { slot } => input_paths
                .get(slot)
                .ok_or_else(|| invalid_invocation(format!("missing verified input slot {slot}")))?
                .as_os_str()
                .to_owned(),
            VerifiedArgument::OutputArtifactPath { slot } => output_paths
                .get(slot)
                .ok_or_else(|| invalid_invocation(format!("missing verified output slot {slot}")))?
                .as_os_str()
                .to_owned(),
        };
        if contains_nul(&token) {
            return Err(MaterializationError::NulArgument { index });
        }
        arguments.push(token);
    }

    Ok(MaterializedInvocation {
        capsule_identity: invocation.capsule_identity().clone(),
        profile_identity: invocation.profile_identity().clone(),
        runtime: invocation.runtime().clone(),
        argv0,
        arguments,
        working_directory,
        effective_resources: invocation.effective_resources().clone(),
        declared_output: invocation.declared_output().clone(),
    })
}

fn collect_bindings(
    kind: ArtifactBindingKind,
    bindings: Vec<(String, PathBuf)>,
) -> MaterializationResult<BTreeMap<String, PathBuf>> {
    let mut collected = BTreeMap::new();
    for (slot, path) in bindings {
        if collected.insert(slot.clone(), path).is_some() {
            return Err(MaterializationError::DuplicateBinding { kind, slot });
        }
    }
    Ok(collected)
}

fn validate_binding_sets(
    invocation: &VerifiedInvocation,
    input_paths: &BTreeMap<String, PathBuf>,
    output_paths: &BTreeMap<String, PathBuf>,
) -> MaterializationResult<()> {
    let expected_inputs = invocation
        .input_artifacts()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let supplied_inputs = input_paths
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_outputs = [invocation.declared_output().name.as_str()]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let supplied_outputs = output_paths
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    let missing_inputs = set_difference(&expected_inputs, &supplied_inputs);
    let unknown_inputs = set_difference(&supplied_inputs, &expected_inputs);
    let missing_outputs = set_difference(&expected_outputs, &supplied_outputs);
    let unknown_outputs = set_difference(&supplied_outputs, &expected_outputs);
    if missing_inputs.is_empty()
        && unknown_inputs.is_empty()
        && missing_outputs.is_empty()
        && unknown_outputs.is_empty()
    {
        Ok(())
    } else {
        Err(MaterializationError::BindingSetMismatch {
            missing_inputs,
            unknown_inputs,
            missing_outputs,
            unknown_outputs,
        })
    }
}

fn set_difference(left: &BTreeSet<&str>, right: &BTreeSet<&str>) -> Vec<String> {
    left.difference(right)
        .map(|slot| (*slot).to_owned())
        .collect()
}

fn validate_absolute_paths(
    kind: ArtifactBindingKind,
    paths: &BTreeMap<String, PathBuf>,
) -> MaterializationResult<()> {
    for (slot, path) in paths {
        if !path.is_absolute() {
            return Err(MaterializationError::NonAbsoluteBinding {
                kind,
                slot: slot.clone(),
            });
        }
        if contains_nul(path.as_os_str()) {
            return Err(MaterializationError::NulBinding {
                kind,
                slot: slot.clone(),
            });
        }
    }
    Ok(())
}

fn contains_nul(value: &OsStr) -> bool {
    value.as_bytes().contains(&0)
}

fn invalid_invocation(reason: impl Into<String>) -> MaterializationError {
    MaterializationError::InvalidVerifiedInvocation {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use serde_json::json;

    use crate::bundle::test_support::{
        bundle_bytes, catalog_with_runtime_package, profile_bytes, signed_bundle_archive,
        signed_entries, write_archive,
    };
    use crate::bundle::{BundleError, BundleProfile};
    use crate::capsule::{CapsuleCatalog, CapsuleError, CompiledCapsule};
    use crate::digest::Sha256Digest;
    use crate::profile_invocation::{
        InputValueRejection, InvocationError, ProfileCall, ProfileValue, verify_profile_call,
    };

    use super::*;

    const FFMPEG_NAME: &str = "ffmpeg-audio-to-wav";
    const EXTRACT_AUDIO_CAPSULE_NAME: &str = "media.extract-audio";
    const RUNTIME_PACKAGE_ID: &str = "org.taskcage.ffmpeg";
    const VERSION: &str = "1.0.0";
    const CALLER_PATH: &str = "jobs/42/original source.mp3";
    const STAGED_INPUT: &str = "/var/lib/taskcage/staging/task/artifacts/in/source";
    const STAGED_OUTPUT: &str = "/var/lib/taskcage/staging/task/artifacts/out/result.wav";
    const WORKING_DIRECTORY: &str = "/var/lib/taskcage/staging/task";
    const EXTRACT_AUDIO_MAXIMUM_BYTES: u64 = 1_048_576;

    fn compile_profile(bytes: &[u8]) -> CompiledCapsule {
        let profile: BundleProfile = serde_json::from_slice(bytes).unwrap();
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(bytes, package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        CapsuleCatalog::new(&catalog)
            .compile(&profile.name, &profile.version)
            .unwrap()
    }

    fn extract_audio_capsule_profile_bytes() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion": "taskcage.profile/v0alpha1",
            "name": EXTRACT_AUDIO_CAPSULE_NAME,
            "version": VERSION,
            "inputs": [
                {"name":"source","kind":"LOCAL_INPUT","required":true},
                {"name":"sample_rate_hz","kind":"INT64","required":true,"allowedValues":[8000,16000,22050,44100,48000]},
                {"name":"channels","kind":"INT64","required":true,"allowedValues":[1,2]}
            ],
            "output": {
                "name":"audio",
                "fileName":"result.wav",
                "mediaType":"audio/wav",
                "maximumBytes":EXTRACT_AUDIO_MAXIMUM_BYTES
            },
            // This Capsule contract intentionally differs from the legacy ffmpeg profile.
            "argv": [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                {"input":"source"},
                "-vn",
                "-c:a",
                "pcm_s16le",
                "-ar",
                {"int64":"sample_rate_hz"},
                "-ac",
                {"int64":"channels"},
                {"output":"audio"}
            ],
            "policy": {
                "limits": {
                    "cpuMax":{"quotaMicros":1,"periodMicros":1},
                    "memoryMaxBytes":1,
                    "pidsMax":1,
                    "wallTimeLimitMs":1
                },
                "output":{"stdoutTailMaxBytes":1,"stderrTailMaxBytes":1}
            },
            "allowedOverrides":[]
        }))
        .unwrap()
    }

    fn compile_extract_audio_capsule() -> (CompiledCapsule, Sha256Digest) {
        let profile = extract_audio_capsule_profile_bytes();
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(&profile, package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        let capsule = CapsuleCatalog::new(&catalog)
            .compile(EXTRACT_AUDIO_CAPSULE_NAME, VERSION)
            .unwrap();
        (capsule, package_digest)
    }

    fn extract_audio_inputs(sample_rate_hz: i64, channels: i64) -> Vec<(String, ProfileValue)> {
        vec![
            (
                "source".to_owned(),
                ProfileValue::LocalInput {
                    path: CALLER_PATH.to_owned(),
                    digest: format!("sha256:{}", "0".repeat(64)),
                    size_bytes: 128,
                },
            ),
            (
                "sample_rate_hz".to_owned(),
                ProfileValue::Int64(sample_rate_hz),
            ),
            ("channels".to_owned(), ProfileValue::Int64(channels)),
        ]
    }

    fn extract_audio_call(version: &str, sample_rate_hz: i64, channels: i64) -> ProfileCall {
        ProfileCall::new(
            ProfileIdentity::new(EXTRACT_AUDIO_CAPSULE_NAME, version),
            extract_audio_inputs(sample_rate_hz, channels),
        )
    }

    fn extract_audio_bindings() -> ArtifactBindings {
        ArtifactBindings::new(
            [("source", PathBuf::from(STAGED_INPUT))],
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        )
    }

    fn ffmpeg_call() -> ProfileCall {
        ProfileCall::new(
            ProfileIdentity::new(FFMPEG_NAME, VERSION),
            vec![
                (
                    "source",
                    ProfileValue::LocalInput {
                        path: CALLER_PATH.to_owned(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        size_bytes: 128,
                    },
                ),
                ("sample_rate_hz", ProfileValue::Int64(16_000)),
                ("channels", ProfileValue::Int64(1)),
            ],
        )
    }

    fn ffmpeg_invocation() -> VerifiedInvocation {
        let capsule = compile_profile(&profile_bytes());
        verify_profile_call(&capsule, ffmpeg_call()).unwrap()
    }

    fn ffmpeg_bindings() -> ArtifactBindings {
        ArtifactBindings::new(
            [("source", PathBuf::from(STAGED_INPUT))],
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        )
    }

    fn token_profile_bytes() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion": "taskcage.profile/v0alpha1",
            "name": "mapper-token-profile",
            "version": VERSION,
            "inputs": [
                {"name":"source","kind":"LOCAL_INPUT","required":true},
                {"name":"count","kind":"INT64","required":true,"minimum":-100,"maximum":100},
                {"name":"flag","kind":"BOOLEAN","required":true},
                {"name":"label","kind":"STRING","required":true}
            ],
            "output": {
                "name":"result",
                "fileName":"result.bin",
                "mediaType":"application/octet-stream",
                "maximumBytes":1024
            },
            "argv": [
                "literal-first",
                {"string":"label"},
                {"boolean":"flag"},
                {"int64":"count"},
                {"input":"source"},
                "literal-last",
                {"output":"result"}
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

    fn token_call(label: String) -> ProfileCall {
        ProfileCall::new(
            ProfileIdentity::new("mapper-token-profile", VERSION),
            vec![
                (
                    "source",
                    ProfileValue::LocalInput {
                        path: CALLER_PATH.to_owned(),
                        digest: format!("sha256:{}", "0".repeat(64)),
                        size_bytes: 128,
                    },
                ),
                ("count", ProfileValue::Int64(-7)),
                ("flag", ProfileValue::Boolean(true)),
                ("label", ProfileValue::String(label)),
            ],
        )
    }

    fn token_invocation(label: String) -> Result<VerifiedInvocation, InvocationError> {
        let capsule = compile_profile(&token_profile_bytes());
        verify_profile_call(&capsule, token_call(label))
    }

    fn token_bindings() -> ArtifactBindings {
        ArtifactBindings::new(
            [("source", PathBuf::from("/staged/in/source.bin"))],
            [("result", PathBuf::from("/staged/out/result.bin"))],
            PathBuf::from("/staged/work"),
        )
    }

    #[test]
    fn signed_extract_audio_capsule_materializes_the_complete_developer_b_handoff_contract() {
        let (capsule, package_digest) = compile_extract_audio_capsule();
        assert_eq!(capsule.identity().name(), EXTRACT_AUDIO_CAPSULE_NAME);
        assert_eq!(capsule.identity().version(), VERSION);
        assert_eq!(capsule.provenance().signing_key_id(), "test-release");

        let invocation =
            verify_profile_call(&capsule, extract_audio_call(VERSION, 16_000, 1)).unwrap();
        assert_eq!(
            invocation.capsule_identity().name(),
            EXTRACT_AUDIO_CAPSULE_NAME
        );
        assert_eq!(
            invocation.profile_identity().name(),
            EXTRACT_AUDIO_CAPSULE_NAME
        );

        let materialized = materialize_invocation(invocation, extract_audio_bindings()).unwrap();
        let expected_arguments = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-nostdin"),
            OsString::from("-i"),
            OsString::from(STAGED_INPUT),
            OsString::from("-vn"),
            OsString::from("-c:a"),
            OsString::from("pcm_s16le"),
            OsString::from("-ar"),
            OsString::from("16000"),
            OsString::from("-ac"),
            OsString::from("1"),
            OsString::from(STAGED_OUTPUT),
        ];

        assert_eq!(
            materialized.capsule_identity().name(),
            EXTRACT_AUDIO_CAPSULE_NAME
        );
        assert_eq!(materialized.capsule_identity().version(), VERSION);
        assert_eq!(
            materialized.profile_identity().name(),
            EXTRACT_AUDIO_CAPSULE_NAME
        );
        assert_eq!(materialized.profile_identity().version(), VERSION);
        assert_eq!(materialized.runtime().package_id(), RUNTIME_PACKAGE_ID);
        // The package digest transitively references the signed fixture manifest entrypoint.
        assert_eq!(materialized.runtime().package_digest(), package_digest);
        assert_eq!(materialized.argv0(), OsStr::new(EXTRACT_AUDIO_CAPSULE_NAME));
        assert_eq!(materialized.arguments(), expected_arguments);
        assert_eq!(
            materialized.working_directory(),
            Path::new(WORKING_DIRECTORY)
        );
        assert!(
            materialized
                .arguments()
                .iter()
                .all(|argument| argument != CALLER_PATH)
        );

        let declared_output = materialized.declared_output();
        assert_eq!(declared_output.name, "audio");
        assert_eq!(declared_output.file_name, "result.wav");
        assert_eq!(declared_output.media_type, "audio/wav");
        assert_eq!(declared_output.maximum_bytes, EXTRACT_AUDIO_MAXIMUM_BYTES);
        assert!(!declared_output.file_name.is_empty());
        assert!(!declared_output.media_type.is_empty());
        assert!(declared_output.maximum_bytes > 0);

        let limits = materialized.effective_resources().protocol_limits();
        assert_eq!(limits.cpu_max.quota_micros, 1);
        assert_eq!(limits.cpu_max.period_micros, 1);
        assert_eq!(limits.memory_max_bytes, 1);
        assert_eq!(limits.pids_max, 1);
        assert_eq!(limits.wall_time_limit_ms, 1);
        let output_limits = materialized.effective_resources().protocol_output();
        assert_eq!(output_limits.stdout_tail_max_bytes, 1);
        assert_eq!(output_limits.stderr_tail_max_bytes, 1);

        let runtime = materialized.runtime().clone();
        let output = materialized.declared_output().clone();
        let (argv0, arguments, working_directory, budget) = materialized.into_execution_parts();
        assert_eq!(argv0, OsString::from(EXTRACT_AUDIO_CAPSULE_NAME));
        assert_eq!(arguments, expected_arguments);
        assert_eq!(working_directory, PathBuf::from(WORKING_DIRECTORY));
        assert_eq!(runtime.package_id(), RUNTIME_PACKAGE_ID);
        assert_eq!(runtime.package_digest(), package_digest);
        assert_eq!(output.name, "audio");
        assert_eq!(budget.protocol_limits(), limits);
        assert_eq!(budget.protocol_output(), output_limits);
    }

    #[test]
    fn bad_signature_never_compiles_the_extract_audio_capsule() {
        let profile = extract_audio_capsule_profile_bytes();
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let bundle = bundle_bytes(&profile, &package_digest.to_string());
        let (mut entries, keys) = signed_entries(bundle, profile);
        let signature = &mut entries
            .iter_mut()
            .find(|(name, _)| *name == "signature.sig")
            .unwrap()
            .1;
        signature[0] = if signature[0] == b'A' { b'B' } else { b'A' };
        let archive = write_archive(entries);

        assert!(matches!(
            catalog.import(archive.path(), &keys),
            Err(BundleError::Signature(_))
        ));
        assert!(matches!(
            CapsuleCatalog::new(&catalog).compile(EXTRACT_AUDIO_CAPSULE_NAME, VERSION),
            Err(CapsuleError::LegacyBundle(BundleError::NotFound { name, version }))
                if name == EXTRACT_AUDIO_CAPSULE_NAME && version == VERSION
        ));
    }

    #[test]
    fn extract_audio_capsule_profile_call_errors_are_stable() {
        let (capsule, _) = compile_extract_audio_capsule();

        assert_eq!(
            verify_profile_call(&capsule, extract_audio_call("1.0.1", 16_000, 1)).unwrap_err(),
            InvocationError::CapsuleProfileNotFound {
                name: EXTRACT_AUDIO_CAPSULE_NAME.to_owned(),
                version: "1.0.1".to_owned(),
            }
        );

        let mut missing_source = extract_audio_inputs(16_000, 1);
        missing_source.retain(|(name, _)| name != "source");
        let missing_source = ProfileCall::new(
            ProfileIdentity::new(EXTRACT_AUDIO_CAPSULE_NAME, VERSION),
            missing_source,
        );
        assert_eq!(
            verify_profile_call(&capsule, missing_source).unwrap_err(),
            InvocationError::InputSetMismatch {
                missing: vec!["source".to_owned()],
                unknown: vec![],
            }
        );

        assert_eq!(
            verify_profile_call(&capsule, extract_audio_call(VERSION, 12_000, 1)).unwrap_err(),
            InvocationError::InputValueRejected {
                input: "sample_rate_hz".to_owned(),
                rejection: InputValueRejection::Int64Constraint,
            }
        );
        assert_eq!(
            verify_profile_call(&capsule, extract_audio_call(VERSION, 16_000, 3)).unwrap_err(),
            InvocationError::InputValueRejected {
                input: "channels".to_owned(),
                rejection: InputValueRejection::Int64Constraint,
            }
        );
    }

    #[test]
    fn extract_audio_capsule_artifact_binding_errors_are_stable() {
        let (capsule, _) = compile_extract_audio_capsule();
        let invocation =
            verify_profile_call(&capsule, extract_audio_call(VERSION, 16_000, 1)).unwrap();
        let missing_source = ArtifactBindings::new(
            std::iter::empty::<(String, PathBuf)>(),
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(invocation, missing_source).unwrap_err(),
            MaterializationError::BindingSetMismatch {
                missing_inputs: vec!["source".to_owned()],
                unknown_inputs: vec![],
                missing_outputs: vec![],
                unknown_outputs: vec![],
            }
        );

        let invocation =
            verify_profile_call(&capsule, extract_audio_call(VERSION, 16_000, 1)).unwrap();
        let wrong_output = ArtifactBindings::new(
            [("source", PathBuf::from(STAGED_INPUT))],
            [("result", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(invocation, wrong_output).unwrap_err(),
            MaterializationError::BindingSetMismatch {
                missing_inputs: vec![],
                unknown_inputs: vec![],
                missing_outputs: vec!["audio".to_owned()],
                unknown_outputs: vec!["result".to_owned()],
            }
        );
    }

    #[test]
    fn signed_extract_audio_capsule_mapping_is_deterministic() {
        let (first_capsule, first_package_digest) = compile_extract_audio_capsule();
        let (second_capsule, second_package_digest) = compile_extract_audio_capsule();
        assert_eq!(first_package_digest, second_package_digest);
        assert_eq!(first_capsule, second_capsule);

        let first_invocation =
            verify_profile_call(&first_capsule, extract_audio_call(VERSION, 16_000, 1)).unwrap();
        let second_invocation =
            verify_profile_call(&second_capsule, extract_audio_call(VERSION, 16_000, 1)).unwrap();
        let first = materialize_invocation(first_invocation, extract_audio_bindings()).unwrap();
        let second = materialize_invocation(second_invocation, extract_audio_bindings()).unwrap();

        assert_eq!(first.capsule_identity(), second.capsule_identity());
        assert_eq!(first.profile_identity(), second.profile_identity());
        assert_eq!(first.runtime(), second.runtime());
        assert_eq!(first.argv0(), second.argv0());
        assert_eq!(first.arguments(), second.arguments());
        assert_eq!(first.working_directory(), second.working_directory());
        assert_eq!(first.declared_output(), second.declared_output());
        assert_eq!(
            first.effective_resources().protocol_limits(),
            second.effective_resources().protocol_limits()
        );
        assert_eq!(
            first.effective_resources().protocol_output(),
            second.effective_resources().protocol_output()
        );
    }

    #[test]
    fn fake_staged_paths_produce_the_exact_ordered_ffmpeg_argv() {
        let materialized = materialize_invocation(ffmpeg_invocation(), ffmpeg_bindings()).unwrap();
        let input = PathBuf::from(STAGED_INPUT);
        let output = PathBuf::from(STAGED_OUTPUT);
        let expected = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-nostdin"),
            OsString::from("-i"),
            input.as_os_str().to_owned(),
            OsString::from("-map"),
            OsString::from("0:a:0"),
            OsString::from("-vn"),
            OsString::from("-c:a"),
            OsString::from("pcm_s16le"),
            OsString::from("-ar"),
            OsString::from("16000"),
            OsString::from("-ac"),
            OsString::from("1"),
            output.as_os_str().to_owned(),
        ];

        assert_eq!(materialized.argv0(), OsStr::new(FFMPEG_NAME));
        assert_eq!(materialized.arguments(), expected);
        assert_eq!(
            materialized.working_directory(),
            Path::new(WORKING_DIRECTORY)
        );
        assert_eq!(materialized.declared_output().name, "audio");
        assert!(
            materialized
                .arguments()
                .iter()
                .all(|value| value != CALLER_PATH)
        );
    }

    #[test]
    fn literals_and_scalar_tokens_preserve_mapping_order_without_shell_processing() {
        let label = "one token with spaces; $HOME *.wav 'quotes'".to_owned();
        let materialized =
            materialize_invocation(token_invocation(label.clone()).unwrap(), token_bindings())
                .unwrap();

        assert_eq!(
            materialized.arguments(),
            [
                OsString::from("literal-first"),
                OsString::from(label),
                OsString::from("true"),
                OsString::from("-7"),
                OsString::from("/staged/in/source.bin"),
                OsString::from("literal-last"),
                OsString::from("/staged/out/result.bin"),
            ]
        );
    }

    #[test]
    fn missing_input_and_output_bindings_are_rejected_separately() {
        let missing_input = ArtifactBindings::new(
            std::iter::empty::<(String, PathBuf)>(),
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), missing_input).unwrap_err(),
            MaterializationError::BindingSetMismatch {
                missing_inputs: vec!["source".to_owned()],
                unknown_inputs: vec![],
                missing_outputs: vec![],
                unknown_outputs: vec![],
            }
        );

        let missing_output = ArtifactBindings::new(
            [("source", PathBuf::from(STAGED_INPUT))],
            std::iter::empty::<(String, PathBuf)>(),
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), missing_output).unwrap_err(),
            MaterializationError::BindingSetMismatch {
                missing_inputs: vec![],
                unknown_inputs: vec![],
                missing_outputs: vec!["audio".to_owned()],
                unknown_outputs: vec![],
            }
        );
    }

    #[test]
    fn unknown_input_and_output_bindings_are_rejected() {
        let bindings = ArtifactBindings::new(
            [
                ("source", PathBuf::from(STAGED_INPUT)),
                ("unknown-input", PathBuf::from("/staged/in/unknown")),
            ],
            [
                ("audio", PathBuf::from(STAGED_OUTPUT)),
                ("unknown-output", PathBuf::from("/staged/out/unknown")),
            ],
            PathBuf::from(WORKING_DIRECTORY),
        );

        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), bindings).unwrap_err(),
            MaterializationError::BindingSetMismatch {
                missing_inputs: vec![],
                unknown_inputs: vec!["unknown-input".to_owned()],
                missing_outputs: vec![],
                unknown_outputs: vec!["unknown-output".to_owned()],
            }
        );
    }

    #[test]
    fn duplicate_input_and_output_bindings_are_rejected_before_set_matching() {
        let duplicate_input = ArtifactBindings::new(
            [
                ("source", PathBuf::from(STAGED_INPUT)),
                ("source", PathBuf::from("/staged/in/second")),
            ],
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), duplicate_input).unwrap_err(),
            MaterializationError::DuplicateBinding {
                kind: ArtifactBindingKind::Input,
                slot: "source".to_owned(),
            }
        );

        let duplicate_output = ArtifactBindings::new(
            [("source", PathBuf::from(STAGED_INPUT))],
            [
                ("audio", PathBuf::from(STAGED_OUTPUT)),
                ("audio", PathBuf::from("/staged/out/second")),
            ],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), duplicate_output).unwrap_err(),
            MaterializationError::DuplicateBinding {
                kind: ArtifactBindingKind::Output,
                slot: "audio".to_owned(),
            }
        );
    }

    #[test]
    fn nul_scalar_is_rejected_before_materialization_and_nul_binding_is_rejected() {
        assert_eq!(
            token_invocation("before\0after".to_owned()).unwrap_err(),
            InvocationError::InputValueRejected {
                input: "label".to_owned(),
                rejection: InputValueRejection::StringConstraint,
            }
        );

        let nul_path = PathBuf::from(OsString::from_vec(b"/staged/in/before\0after".to_vec()));
        let bindings = ArtifactBindings::new(
            [("source", nul_path)],
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), bindings).unwrap_err(),
            MaterializationError::NulBinding {
                kind: ArtifactBindingKind::Input,
                slot: "source".to_owned(),
            }
        );
    }

    #[test]
    fn nul_binding_is_rejected_even_when_profile_does_not_emit_its_path() {
        let mut profile: serde_json::Value =
            serde_json::from_slice(&token_profile_bytes()).unwrap();
        profile["argv"] = json!(["literal-only"]);
        let capsule = compile_profile(&serde_json::to_vec(&profile).unwrap());
        let invocation = verify_profile_call(&capsule, token_call("label".to_owned())).unwrap();
        let nul_output = PathBuf::from(OsString::from_vec(b"/staged/out/before\0after".to_vec()));
        let bindings = ArtifactBindings::new(
            [("source", PathBuf::from("/staged/in/source.bin"))],
            [("result", nul_output)],
            PathBuf::from("/staged/work"),
        );

        assert_eq!(
            materialize_invocation(invocation, bindings).unwrap_err(),
            MaterializationError::NulBinding {
                kind: ArtifactBindingKind::Output,
                slot: "result".to_owned(),
            }
        );
    }

    #[test]
    fn non_utf8_staged_paths_are_preserved_as_os_strings() {
        let input = PathBuf::from(OsString::from_vec(b"/staged/in/source-\xff".to_vec()));
        let output = PathBuf::from(OsString::from_vec(b"/staged/out/result-\xfe".to_vec()));
        let bindings = ArtifactBindings::new(
            [("source", input.clone())],
            [("audio", output.clone())],
            PathBuf::from(WORKING_DIRECTORY),
        );
        let materialized = materialize_invocation(ffmpeg_invocation(), bindings).unwrap();

        assert_eq!(materialized.arguments()[5], input.as_os_str());
        assert_eq!(materialized.arguments()[15], output.as_os_str());
    }

    #[test]
    fn identical_verified_invocation_and_bindings_are_deterministic() {
        let invocation = ffmpeg_invocation();
        let bindings = ffmpeg_bindings();
        let first = materialize_invocation(invocation.clone(), bindings.clone()).unwrap();
        let second = materialize_invocation(invocation, bindings).unwrap();

        assert_eq!(first.argv0(), second.argv0());
        assert_eq!(first.arguments(), second.arguments());
        assert_eq!(first.working_directory(), second.working_directory());
        assert_eq!(first.runtime(), second.runtime());
        assert_eq!(first.declared_output(), second.declared_output());
        assert_eq!(
            first.effective_resources().protocol_limits(),
            second.effective_resources().protocol_limits()
        );
        assert_eq!(
            first.effective_resources().protocol_output(),
            second.effective_resources().protocol_output()
        );
    }

    #[test]
    fn relative_runtime_paths_are_rejected_without_filesystem_access() {
        let relative_input = ArtifactBindings::new(
            [("source", PathBuf::from("relative/input"))],
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from(WORKING_DIRECTORY),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), relative_input).unwrap_err(),
            MaterializationError::NonAbsoluteBinding {
                kind: ArtifactBindingKind::Input,
                slot: "source".to_owned(),
            }
        );

        let relative_working_directory = ArtifactBindings::new(
            [("source", PathBuf::from(STAGED_INPUT))],
            [("audio", PathBuf::from(STAGED_OUTPUT))],
            PathBuf::from("relative/work"),
        );
        assert_eq!(
            materialize_invocation(ffmpeg_invocation(), relative_working_directory).unwrap_err(),
            MaterializationError::NonAbsoluteWorkingDirectory
        );
    }
}
