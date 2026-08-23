//! Protocol-independent ProfileCall validation against an immutable CompiledCapsule.
//!
//! This module performs no filesystem access, package resolution, artifact staging, cgroup
//! creation, task registration, or process execution. Runtime-owned artifact paths therefore do
//! not exist in `VerifiedInvocation`; path placeholders retain only their declared slot names.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

pub use taskcage_core::capsule::{
    CpuMaxOverride, ProfileCall, ProfileIdentity, ProfileResourceOverrides, ProfileValue,
    VerifiedArgument,
};
use thiserror::Error;

use crate::artifact::{ArtifactPath, LocalInputArtifact};
use crate::bundle::valid_capsule_name;
use crate::capsule::{
    CapsuleArgument, CapsuleIdentity, CapsuleInput, CapsuleInputKind, CapsuleInputValueKind,
    CapsuleOutput, CapsuleRuntimeReference, CompiledCapsule, CompiledCapsuleProfile,
};
use crate::digest::Sha256Digest;
use crate::resource_budget::ResourceBudget;

/// Side-effect-free result that can only be constructed by `verify_profile_call`.
///
/// Private fields preserve the relationship between the compiled contract, validated values,
/// parsed artifact declarations, argument template, and effective resource budget.
#[derive(Debug, Clone)]
pub struct VerifiedInvocation {
    capsule: CompiledCapsule,
    profile_identity: ProfileIdentity,
    values: BTreeMap<String, ProfileValue>,
    input_artifacts: BTreeMap<String, LocalInputArtifact>,
    arguments: Vec<VerifiedArgument>,
    effective_resources: ResourceBudget,
}

impl VerifiedInvocation {
    pub fn capsule_identity(&self) -> &CapsuleIdentity {
        self.capsule.identity()
    }

    pub fn profile_identity(&self) -> &ProfileIdentity {
        &self.profile_identity
    }

    pub fn runtime(&self) -> &CapsuleRuntimeReference {
        self.capsule.runtime()
    }

    pub fn values(&self) -> &BTreeMap<String, ProfileValue> {
        &self.values
    }

    pub fn input_artifacts(&self) -> &BTreeMap<String, LocalInputArtifact> {
        &self.input_artifacts
    }

    pub fn arguments(&self) -> &[VerifiedArgument] {
        &self.arguments
    }

    pub fn declared_inputs(&self) -> &[CapsuleInput] {
        self.capsule.profile().inputs()
    }

    pub fn declared_output(&self) -> &CapsuleOutput {
        self.capsule.profile().output()
    }

    pub fn effective_resources(&self) -> &ResourceBudget {
        &self.effective_resources
    }
}

/// Stable reason for rejecting a value whose kind matched its declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InputValueRejection {
    #[error("value is outside the declared INT64 contract")]
    Int64Constraint,
    #[error("STRING must contain 1..=4096 non-NUL UTF-8 bytes")]
    StringConstraint,
    #[error("LOCAL_INPUT path declaration is invalid")]
    ArtifactPath,
    #[error("LOCAL_INPUT digest declaration is invalid")]
    ArtifactDigest,
}

/// Stable domain classification for failed ProfileCall verification.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvocationError {
    #[error("invalid Profile identity: {reason}")]
    InvalidProfileIdentity { reason: &'static str },
    #[error("Capsule/Profile {name}@{version} is not the compiled exact identity")]
    CapsuleProfileNotFound { name: String, version: String },
    #[error("input {input} was supplied more than once")]
    DuplicateInput { input: String },
    #[error("Profile input set mismatch: missing={missing:?}, unknown={unknown:?}")]
    InputSetMismatch {
        missing: Vec<String>,
        unknown: Vec<String>,
    },
    #[error("input {input} must be {expected}, actual={actual}")]
    InputTypeMismatch {
        input: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("input {input} was rejected: {rejection}")]
    InputValueRejected {
        input: String,
        rejection: InputValueRejection,
    },
    #[error("invalid resource override: {reason}")]
    InvalidResourceOverride { reason: String },
    #[error("resource override rejected by Capsule policy: {reason}")]
    PolicyRejected { reason: String },
    #[error("invalid compiled Capsule contract: {reason}")]
    InvalidCompiledContract { reason: String },
}

pub type InvocationResult<T> = std::result::Result<T, InvocationError>;

/// Validates one exact ProfileCall without opening a Package or touching runtime state.
pub fn verify_profile_call(
    capsule: &CompiledCapsule,
    call: ProfileCall,
) -> InvocationResult<VerifiedInvocation> {
    validate_requested_identity(call.identity())?;
    validate_compiled_structure(capsule)?;

    let profile = capsule.profile();
    if call.identity().name() != capsule.identity().name()
        || call.identity().version() != capsule.identity().version()
        || call.identity().name() != profile.domain_identity().name()
        || call.identity().version() != profile.domain_identity().version()
    {
        return Err(InvocationError::CapsuleProfileNotFound {
            name: call.identity().name().to_owned(),
            version: call.identity().version().to_owned(),
        });
    }

    let (identity, inputs, resource_overrides) = call.into_parts();
    let values = collect_inputs(inputs)?;
    validate_exact_input_set(profile.inputs(), &values)?;
    let input_artifacts = validate_input_values(profile.inputs(), &values)?;
    let arguments = bind_arguments(profile, &values, &input_artifacts)?;
    let effective_resources = resolve_resources(profile, resource_overrides.as_ref())?;

    Ok(VerifiedInvocation {
        capsule: capsule.clone(),
        profile_identity: identity,
        values,
        input_artifacts,
        arguments,
        effective_resources,
    })
}

fn validate_requested_identity(identity: &ProfileIdentity) -> InvocationResult<()> {
    if !valid_capsule_name(identity.name()) {
        return Err(InvocationError::InvalidProfileIdentity {
            reason: "name must use dot-separated [a-z][a-z0-9-]* segments (maximum 63 bytes)",
        });
    }
    if !valid_profile_version(identity.version()) {
        return Err(InvocationError::InvalidProfileIdentity {
            reason: "version must be strict MAJOR.MINOR.PATCH",
        });
    }
    Ok(())
}

fn validate_compiled_structure(capsule: &CompiledCapsule) -> InvocationResult<()> {
    let profile = capsule.profile();
    if capsule.identity().name() != profile.domain_identity().name()
        || capsule.identity().version() != profile.domain_identity().version()
    {
        return Err(invalid_contract(
            "Capsule and Profile identities do not match",
        ));
    }
    if profile.inputs().is_empty() || profile.inputs().len() > 32 {
        return Err(invalid_contract("Profile must declare 1..=32 inputs"));
    }

    let mut names = BTreeSet::new();
    let mut local_inputs = 0_usize;
    for input in profile.inputs() {
        if !input.required() || !valid_slot_name(input.name()) || !names.insert(input.name()) {
            return Err(invalid_contract(
                "Profile inputs must be required, valid, and unique",
            ));
        }
        if matches!(input.kind(), CapsuleInputKind::LocalInput) {
            local_inputs += 1;
        }
    }
    if local_inputs != 1 {
        return Err(invalid_contract(
            "Profile must declare exactly one LOCAL_INPUT",
        ));
    }
    if !valid_slot_name(&profile.output().name) {
        return Err(invalid_contract("Profile output slot is invalid"));
    }
    if profile.arguments().is_empty() || profile.arguments().len() > 128 {
        return Err(invalid_contract("Profile argv must contain 1..=128 nodes"));
    }

    let mut allowed_overrides = BTreeSet::new();
    for field in profile.allowed_overrides() {
        if !known_override_field(field) || !allowed_overrides.insert(field.as_str()) {
            return Err(invalid_contract(
                "resource override allowlist contains an invalid field",
            ));
        }
    }
    Ok(())
}

fn collect_inputs(
    inputs: Vec<(String, ProfileValue)>,
) -> InvocationResult<BTreeMap<String, ProfileValue>> {
    let mut values = BTreeMap::new();
    for (name, value) in inputs {
        if values.insert(name.clone(), value).is_some() {
            return Err(InvocationError::DuplicateInput { input: name });
        }
    }
    Ok(values)
}

fn validate_exact_input_set(
    declarations: &[CapsuleInput],
    values: &BTreeMap<String, ProfileValue>,
) -> InvocationResult<()> {
    let declared = declarations
        .iter()
        .map(|input| input.name())
        .collect::<BTreeSet<_>>();
    let supplied = values.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let missing = declared
        .difference(&supplied)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let unknown = supplied
        .difference(&declared)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    if missing.is_empty() && unknown.is_empty() {
        Ok(())
    } else {
        Err(InvocationError::InputSetMismatch { missing, unknown })
    }
}

fn validate_input_values(
    declarations: &[CapsuleInput],
    values: &BTreeMap<String, ProfileValue>,
) -> InvocationResult<BTreeMap<String, LocalInputArtifact>> {
    let mut artifacts = BTreeMap::new();
    for input in declarations {
        let value = values
            .get(input.name())
            .ok_or_else(|| invalid_contract("exact input set validation lost a declared input"))?;
        match (input.kind(), value) {
            (
                CapsuleInputKind::LocalInput,
                ProfileValue::LocalInput {
                    path,
                    digest,
                    size_bytes,
                },
            ) => {
                let path = ArtifactPath::parse(path.clone()).map_err(|_| {
                    InvocationError::InputValueRejected {
                        input: input.name().to_owned(),
                        rejection: InputValueRejection::ArtifactPath,
                    }
                })?;
                let digest = Sha256Digest::from_str(digest).map_err(|_| {
                    InvocationError::InputValueRejected {
                        input: input.name().to_owned(),
                        rejection: InputValueRejection::ArtifactDigest,
                    }
                })?;
                artifacts.insert(
                    input.name().to_owned(),
                    LocalInputArtifact::new(path, digest, *size_bytes),
                );
            }
            (CapsuleInputKind::Int64(constraint), ProfileValue::Int64(value)) => {
                if !constraint.allows(*value) {
                    return Err(InvocationError::InputValueRejected {
                        input: input.name().to_owned(),
                        rejection: InputValueRejection::Int64Constraint,
                    });
                }
            }
            (CapsuleInputKind::String, ProfileValue::String(value)) => {
                if value.is_empty() || value.len() > 4_096 || value.contains('\0') {
                    return Err(InvocationError::InputValueRejected {
                        input: input.name().to_owned(),
                        rejection: InputValueRejection::StringConstraint,
                    });
                }
            }
            (CapsuleInputKind::Boolean, ProfileValue::Boolean(_)) => {}
            (expected, actual) => {
                return Err(InvocationError::InputTypeMismatch {
                    input: input.name().to_owned(),
                    expected: expected.schema_name(),
                    actual: actual.kind_name(),
                });
            }
        }
    }
    Ok(artifacts)
}

fn bind_arguments(
    profile: &CompiledCapsuleProfile,
    values: &BTreeMap<String, ProfileValue>,
    artifacts: &BTreeMap<String, LocalInputArtifact>,
) -> InvocationResult<Vec<VerifiedArgument>> {
    let declarations = profile
        .inputs()
        .iter()
        .map(|input| (input.name(), input.kind()))
        .collect::<BTreeMap<_, _>>();
    let mut arguments = Vec::with_capacity(profile.arguments().len());

    for argument in profile.arguments() {
        let verified = match argument {
            CapsuleArgument::Literal(value) => {
                if value.is_empty() || value.len() > 4_096 || value.contains('\0') {
                    return Err(invalid_contract("argv literal is invalid"));
                }
                VerifiedArgument::Literal(value.clone())
            }
            CapsuleArgument::InputPath { slot } => {
                if !matches!(
                    declarations.get(slot.as_str()),
                    Some(CapsuleInputKind::LocalInput)
                ) || !artifacts.contains_key(slot)
                {
                    return Err(invalid_contract(format!(
                        "argv input path references invalid slot {slot}"
                    )));
                }
                VerifiedArgument::InputArtifactPath { slot: slot.clone() }
            }
            CapsuleArgument::InputValue { kind, slot } => {
                if !argument_kind_matches_declaration(*kind, declarations.get(slot.as_str())) {
                    return Err(invalid_contract(format!(
                        "argv scalar kind does not match input {slot}"
                    )));
                }
                let value = values.get(slot).ok_or_else(|| {
                    invalid_contract(format!("argv references missing input {slot}"))
                })?;
                let literal = match (kind, value) {
                    (CapsuleInputValueKind::String, ProfileValue::String(value)) => value.clone(),
                    (CapsuleInputValueKind::Int64, ProfileValue::Int64(value)) => value.to_string(),
                    (CapsuleInputValueKind::Boolean, ProfileValue::Boolean(value)) => {
                        value.to_string()
                    }
                    _ => {
                        return Err(invalid_contract(format!(
                            "argv scalar value does not match input {slot}"
                        )));
                    }
                };
                VerifiedArgument::Literal(literal)
            }
            CapsuleArgument::OutputPath { slot } => {
                if slot != &profile.output().name {
                    return Err(invalid_contract(format!(
                        "argv output path references invalid slot {slot}"
                    )));
                }
                VerifiedArgument::OutputArtifactPath { slot: slot.clone() }
            }
        };
        arguments.push(verified);
    }
    Ok(arguments)
}

fn argument_kind_matches_declaration(
    argument_kind: CapsuleInputValueKind,
    declaration: Option<&&CapsuleInputKind>,
) -> bool {
    matches!(
        (argument_kind, declaration),
        (
            CapsuleInputValueKind::String,
            Some(CapsuleInputKind::String)
        ) | (
            CapsuleInputValueKind::Int64,
            Some(CapsuleInputKind::Int64(_))
        ) | (
            CapsuleInputValueKind::Boolean,
            Some(CapsuleInputKind::Boolean)
        )
    )
}

fn resolve_resources(
    profile: &CompiledCapsuleProfile,
    overrides: Option<&ProfileResourceOverrides>,
) -> InvocationResult<ResourceBudget> {
    let policy = profile.resource_policy();
    let maximum = ResourceBudget::try_new(
        policy.limits.cpu_max.quota_micros,
        policy.limits.cpu_max.period_micros,
        policy.limits.memory_max_bytes,
        policy.limits.pids_max,
        policy.limits.wall_time_limit_ms,
        policy.output.stdout_tail_max_bytes,
        policy.output.stderr_tail_max_bytes,
    )
    .map_err(|error| invalid_contract(format!("resource policy is invalid: {error}")))?;
    let Some(overrides) = overrides else {
        return Ok(maximum);
    };
    if overrides.is_empty() {
        return Err(InvocationError::InvalidResourceOverride {
            reason: "at least one resource field is required".to_owned(),
        });
    }

    let cpu = overrides.cpu_max();
    let requested = ResourceBudget::try_new(
        cpu.map_or(policy.limits.cpu_max.quota_micros, |value| {
            value.quota_micros()
        }),
        cpu.map_or(policy.limits.cpu_max.period_micros, |value| {
            value.period_micros()
        }),
        overrides
            .memory_max_bytes()
            .unwrap_or(policy.limits.memory_max_bytes),
        overrides.pids_max().unwrap_or(policy.limits.pids_max),
        overrides
            .wall_time_limit_ms()
            .unwrap_or(policy.limits.wall_time_limit_ms),
        overrides
            .stdout_tail_max_bytes()
            .unwrap_or(policy.output.stdout_tail_max_bytes),
        overrides
            .stderr_tail_max_bytes()
            .unwrap_or(policy.output.stderr_tail_max_bytes),
    )
    .map_err(|error| InvocationError::InvalidResourceOverride {
        reason: error.to_string(),
    })?;
    validate_override_allowlist(profile.allowed_overrides(), overrides)?;
    requested
        .validate_within_maximum(&maximum)
        .map_err(|error| InvocationError::PolicyRejected {
            reason: error.to_string(),
        })?;
    Ok(requested)
}

fn validate_override_allowlist(
    allowed: &[String],
    overrides: &ProfileResourceOverrides,
) -> InvocationResult<()> {
    let requested = [
        ("limits.cpuMax", overrides.cpu_max().is_some()),
        (
            "limits.memoryMaxBytes",
            overrides.memory_max_bytes().is_some(),
        ),
        ("limits.pidsMax", overrides.pids_max().is_some()),
        (
            "limits.wallTimeLimitMs",
            overrides.wall_time_limit_ms().is_some(),
        ),
        (
            "output.stdoutTailMaxBytes",
            overrides.stdout_tail_max_bytes().is_some(),
        ),
        (
            "output.stderrTailMaxBytes",
            overrides.stderr_tail_max_bytes().is_some(),
        ),
    ];
    if let Some((field, _)) = requested
        .into_iter()
        .find(|(field, requested)| *requested && !allowed.iter().any(|item| item == field))
    {
        return Err(InvocationError::PolicyRejected {
            reason: format!("field {field} is not allowed"),
        });
    }
    Ok(())
}

fn known_override_field(value: &str) -> bool {
    matches!(
        value,
        "limits.cpuMax"
            | "limits.memoryMaxBytes"
            | "limits.pidsMax"
            | "limits.wallTimeLimitMs"
            | "output.stdoutTailMaxBytes"
            | "output.stderrTailMaxBytes"
    )
}

fn valid_profile_version(value: &str) -> bool {
    let mut components = value.split('.');
    let valid = (&mut components).take(3).all(|component| {
        !component.is_empty()
            && component.bytes().all(|byte| byte.is_ascii_digit())
            && (component == "0" || !component.starts_with('0'))
    });
    valid && components.next().is_none() && value.split('.').count() == 3
}

fn valid_slot_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'_' | b'-')
        })
}

fn invalid_contract(reason: impl Into<String>) -> InvocationError {
    InvocationError::InvalidCompiledContract {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::bundle::test_support::{
        catalog_with_runtime_package, profile_bytes, signed_bundle_archive,
    };
    use crate::capsule::CapsuleCatalog;

    use super::*;

    const PROFILE_NAME: &str = "ffmpeg-audio-to-wav";
    const PROFILE_VERSION: &str = "1.0.0";
    const CALLER_PATH: &str = "jobs/42/source.mp3";

    fn compiled_capsule_without_live_package_cache() -> CompiledCapsule {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let (archive, keys) = signed_bundle_archive(&profile_bytes(), package_digest);
        catalog.import(archive.path(), &keys).unwrap();
        CapsuleCatalog::new(&catalog)
            .compile(PROFILE_NAME, PROFILE_VERSION)
            .unwrap()
    }

    fn valid_inputs() -> Vec<(String, ProfileValue)> {
        vec![
            (
                "source".to_owned(),
                ProfileValue::LocalInput {
                    path: CALLER_PATH.to_owned(),
                    digest: format!("sha256:{}", "0".repeat(64)),
                    size_bytes: 128,
                },
            ),
            ("sample_rate_hz".to_owned(), ProfileValue::Int64(16_000)),
            ("channels".to_owned(), ProfileValue::Int64(1)),
        ]
    }

    fn call(inputs: Vec<(String, ProfileValue)>) -> ProfileCall {
        ProfileCall::new(ProfileIdentity::new(PROFILE_NAME, PROFILE_VERSION), inputs)
    }

    fn replace_input(inputs: &mut [(String, ProfileValue)], slot: &str, value: ProfileValue) {
        inputs.iter_mut().find(|(name, _)| name == slot).unwrap().1 = value;
    }

    #[test]
    fn valid_call_produces_a_private_invariant_invocation_without_package_resolution() {
        // The fixture TempDir and its Runtime Package disappear before verification begins.
        let capsule = compiled_capsule_without_live_package_cache();
        let invocation = verify_profile_call(&capsule, call(valid_inputs())).unwrap();

        assert_eq!(invocation.profile_identity().name(), PROFILE_NAME);
        assert_eq!(invocation.profile_identity().version(), PROFILE_VERSION);
        assert_eq!(invocation.capsule_identity().name(), PROFILE_NAME);
        assert_eq!(invocation.declared_inputs().len(), 3);
        assert_eq!(invocation.declared_output().name, "audio");
        assert_eq!(
            invocation
                .input_artifacts()
                .get("source")
                .unwrap()
                .path()
                .as_str(),
            CALLER_PATH
        );
        assert!(invocation.effective_resources().wall_timeout().as_millis() > 0);
        assert!(invocation.arguments().iter().any(|argument| {
            matches!(
                argument,
                VerifiedArgument::InputArtifactPath { slot } if slot == "source"
            )
        }));
        assert!(invocation.arguments().iter().any(|argument| {
            matches!(
                argument,
                VerifiedArgument::OutputArtifactPath { slot } if slot == "audio"
            )
        }));
    }

    #[test]
    fn missing_unknown_and_duplicate_inputs_have_distinct_domain_errors() {
        let capsule = compiled_capsule_without_live_package_cache();

        let mut missing = valid_inputs();
        missing.retain(|(name, _)| name != "source");
        assert_eq!(
            verify_profile_call(&capsule, call(missing)).unwrap_err(),
            InvocationError::InputSetMismatch {
                missing: vec!["source".to_owned()],
                unknown: vec![],
            }
        );

        let mut unknown = valid_inputs();
        unknown.push(("unexpected".to_owned(), ProfileValue::Boolean(false)));
        assert_eq!(
            verify_profile_call(&capsule, call(unknown)).unwrap_err(),
            InvocationError::InputSetMismatch {
                missing: vec![],
                unknown: vec!["unexpected".to_owned()],
            }
        );

        let mut duplicate = valid_inputs();
        duplicate.push((
            "source".to_owned(),
            ProfileValue::LocalInput {
                path: "jobs/42/other.mp3".to_owned(),
                digest: format!("sha256:{}", "1".repeat(64)),
                size_bytes: 64,
            },
        ));
        assert_eq!(
            verify_profile_call(&capsule, call(duplicate)).unwrap_err(),
            InvocationError::DuplicateInput {
                input: "source".to_owned(),
            }
        );
    }

    #[test]
    fn wrong_input_kind_is_rejected_before_artifact_declaration_parsing() {
        let capsule = compiled_capsule_without_live_package_cache();
        let mut inputs = valid_inputs();
        replace_input(
            &mut inputs,
            "source",
            ProfileValue::String("../not-an-artifact".to_owned()),
        );

        assert_eq!(
            verify_profile_call(&capsule, call(inputs)).unwrap_err(),
            InvocationError::InputTypeMismatch {
                input: "source".to_owned(),
                expected: "LOCAL_INPUT",
                actual: "STRING",
            }
        );
    }

    #[test]
    fn sample_rate_and_channels_preserve_the_compiled_int64_domain() {
        let capsule = compiled_capsule_without_live_package_cache();
        for (slot, value) in [("sample_rate_hz", 12_345), ("channels", 3)] {
            let mut inputs = valid_inputs();
            replace_input(&mut inputs, slot, ProfileValue::Int64(value));
            assert_eq!(
                verify_profile_call(&capsule, call(inputs)).unwrap_err(),
                InvocationError::InputValueRejected {
                    input: slot.to_owned(),
                    rejection: InputValueRejection::Int64Constraint,
                }
            );
        }
    }

    #[test]
    fn exact_profile_version_is_required() {
        let capsule = compiled_capsule_without_live_package_cache();
        let call = ProfileCall::new(ProfileIdentity::new(PROFILE_NAME, "1.0.1"), valid_inputs());

        assert_eq!(
            verify_profile_call(&capsule, call).unwrap_err(),
            InvocationError::CapsuleProfileNotFound {
                name: PROFILE_NAME.to_owned(),
                version: "1.0.1".to_owned(),
            }
        );
    }

    #[test]
    fn caller_artifact_path_never_becomes_a_literal_argument() {
        let capsule = compiled_capsule_without_live_package_cache();
        let invocation = verify_profile_call(&capsule, call(valid_inputs())).unwrap();

        assert!(invocation.arguments().iter().all(|argument| {
            !matches!(argument, VerifiedArgument::Literal(value) if value == CALLER_PATH)
        }));
        assert!(matches!(
            invocation
                .arguments()
                .iter()
                .find(|argument| matches!(argument, VerifiedArgument::InputArtifactPath { .. })),
            Some(VerifiedArgument::InputArtifactPath { slot }) if slot == "source"
        ));
    }

    #[test]
    fn invalid_artifact_declarations_are_classified_without_opening_a_path() {
        let capsule = compiled_capsule_without_live_package_cache();

        let mut invalid_path = valid_inputs();
        replace_input(
            &mut invalid_path,
            "source",
            ProfileValue::LocalInput {
                path: "../source.mp3".to_owned(),
                digest: format!("sha256:{}", "0".repeat(64)),
                size_bytes: 128,
            },
        );
        assert_eq!(
            verify_profile_call(&capsule, call(invalid_path)).unwrap_err(),
            InvocationError::InputValueRejected {
                input: "source".to_owned(),
                rejection: InputValueRejection::ArtifactPath,
            }
        );

        let mut invalid_digest = valid_inputs();
        replace_input(
            &mut invalid_digest,
            "source",
            ProfileValue::LocalInput {
                path: CALLER_PATH.to_owned(),
                digest: "sha256:not-a-digest".to_owned(),
                size_bytes: 128,
            },
        );
        assert_eq!(
            verify_profile_call(&capsule, call(invalid_digest)).unwrap_err(),
            InvocationError::InputValueRejected {
                input: "source".to_owned(),
                rejection: InputValueRejection::ArtifactDigest,
            }
        );
    }

    #[test]
    fn failed_verification_cannot_reach_runtime_side_effects() {
        let capsule = compiled_capsule_without_live_package_cache();
        let mut inputs = valid_inputs();
        replace_input(&mut inputs, "channels", ProfileValue::Int64(8));
        let artifact_staging = Cell::new(0);
        let task_records = Cell::new(0);
        let cgroup_creations = Cell::new(0);
        let process_starts = Cell::new(0);

        let result = verify_profile_call(&capsule, call(inputs)).map(|_| {
            artifact_staging.set(artifact_staging.get() + 1);
            task_records.set(task_records.get() + 1);
            cgroup_creations.set(cgroup_creations.get() + 1);
            process_starts.set(process_starts.get() + 1);
        });

        assert!(matches!(
            result,
            Err(InvocationError::InputValueRejected { .. })
        ));
        assert_eq!(artifact_staging.get(), 0);
        assert_eq!(task_records.get(), 0);
        assert_eq!(cgroup_creations.get(), 0);
        assert_eq!(process_starts.get(), 0);
    }
}
