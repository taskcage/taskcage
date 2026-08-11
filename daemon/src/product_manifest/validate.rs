use std::collections::{BTreeSet, HashMap, HashSet};

use semver::Version;

use crate::digest::Sha256Digest;

use super::{
    ArgvToken, BundleManifest, ExecutionProfileManifest, ManifestError, ManifestOutputLimits,
    ManifestResourceLimits, ResourcePolicyValues, RuntimePackageManifest, ScalarSchema,
    ScalarValue, ValidatedRuntimePackage,
};

const PROFILE_SCHEMA: &str = "taskcage.execution-profile/v0alpha1";
const PACKAGE_SCHEMA: &str = "taskcage.runtime-package/v0alpha1";
const BUNDLE_SCHEMA: &str = "taskcage.bundle/v0alpha1";

const MAX_ID_BYTES: usize = 255;
const MAX_SLOT_COUNT: usize = 64;
const MAX_ENUM_VALUES: usize = 256;
const MAX_MEDIA_TYPES: usize = 64;
const MAX_ARGV_TOKENS: usize = 256;
const MAX_ARGV_TOKEN_BYTES: usize = 4_096;
const MAX_ARGV_BYTES: usize = 65_536;
const MAX_ENVIRONMENT_VARIABLES: usize = 64;
const MAX_ENVIRONMENT_BYTES: usize = 65_536;
const MAX_PACKAGE_FILES: usize = 4_096;
const MAX_LIBRARY_PATHS: usize = 64;
const MAX_LICENSES: usize = 256;
const MAX_OUTPUT_TAIL_BYTES: u64 = 65_536;
const MAX_TOTAL_OUTPUT_BYTES: u64 = 131_072;

pub(super) fn validate_execution_profile(
    profile: &ExecutionProfileManifest,
) -> Result<(), ManifestError> {
    require_exact("schemaVersion", &profile.schema_version, PROFILE_SCHEMA)?;
    validate_identity("id", &profile.id)?;
    validate_semver("version", &profile.version)?;
    validate_relative_path("entrypoint", &profile.entrypoint)?;

    if profile.input_schema.additional_properties {
        return Err(ManifestError::invalid(
            "inputSchema.additionalProperties",
            "false여야 합니다",
        ));
    }
    if profile.output_schema.additional_properties {
        return Err(ManifestError::invalid(
            "outputSchema.additionalProperties",
            "false여야 합니다",
        ));
    }

    let input_count = profile
        .input_schema
        .scalars
        .len()
        .checked_add(profile.input_schema.artifacts.len())
        .ok_or_else(|| ManifestError::invalid("inputSchema", "slot 수가 overflow했습니다"))?;
    if input_count > MAX_SLOT_COUNT {
        return Err(ManifestError::invalid(
            "inputSchema",
            format!("slot은 최대 {MAX_SLOT_COUNT}개입니다"),
        ));
    }
    if profile.input_schema.artifacts.is_empty() {
        return Err(ManifestError::invalid(
            "inputSchema.artifacts",
            "input Artifact가 하나 이상 필요합니다",
        ));
    }
    if profile.output_schema.artifacts.len() != 1 {
        return Err(ManifestError::invalid(
            "outputSchema.artifacts",
            "required output Artifact가 정확히 하나여야 합니다",
        ));
    }
    if profile
        .output_schema
        .artifacts
        .keys()
        .any(|name| profile.input_schema.artifacts.contains_key(name))
    {
        return Err(ManifestError::invalid(
            "inputSchema/outputSchema.artifacts",
            "argv에서 방향이 모호해지므로 input과 output slot 이름은 겹칠 수 없습니다",
        ));
    }

    for (name, schema) in &profile.input_schema.scalars {
        validate_slot_name("inputSchema.scalars", name)?;
        validate_scalar_schema(schema)?;
    }
    for (name, schema) in &profile.input_schema.artifacts {
        validate_slot_name("inputSchema.artifacts", name)?;
        if !schema.required {
            return Err(ManifestError::invalid(
                "inputSchema.artifacts.required",
                "v0.2 input Artifact는 required여야 합니다",
            ));
        }
        require_count(
            "inputSchema.artifacts.mediaTypes",
            schema.media_types.len(),
            1,
            MAX_MEDIA_TYPES,
        )?;
        require_unique_strings("inputSchema.artifacts.mediaTypes", &schema.media_types)?;
        for media_type in &schema.media_types {
            validate_media_type("inputSchema.artifacts.mediaTypes", media_type)?;
        }
        require_positive("inputSchema.artifacts.maxSizeBytes", schema.max_size_bytes)?;
    }

    let (output_name, output) = profile
        .output_schema
        .artifacts
        .first_key_value()
        .expect("output count was checked above");
    validate_slot_name("outputSchema.artifacts", output_name)?;
    if !output.required {
        return Err(ManifestError::invalid(
            "outputSchema.artifacts.required",
            "v0.2 output Artifact는 required여야 합니다",
        ));
    }
    require_positive("outputSchema.artifacts.maxSizeBytes", output.max_size_bytes)?;
    validate_media_type("outputSchema.artifacts.mediaType", &output.media_type)?;
    validate_path_segment("outputSchema.artifacts.fileName", &output.file_name)?;

    validate_argv(profile)?;
    validate_environment(&profile.environment)?;
    validate_resource_policy(
        &profile.resource_policy.defaults,
        &profile.resource_policy.max_overrides,
    )?;
    Ok(())
}

fn validate_scalar_schema(schema: &ScalarSchema) -> Result<(), ManifestError> {
    if !schema.required() {
        return Err(ManifestError::invalid(
            "inputSchema.scalars.required",
            "v0.2 scalar input은 required여야 합니다",
        ));
    }

    match schema {
        ScalarSchema::String {
            allowed_values,
            min_length,
            max_length,
            ..
        } => {
            if min_length.is_some_and(|minimum| minimum > MAX_ARGV_TOKEN_BYTES as u64) {
                return Err(ManifestError::invalid(
                    "inputSchema.scalars.minLength",
                    "문자열의 전역 4,096 bytes 상한을 넘을 수 없습니다",
                ));
            }
            if max_length.is_some_and(|maximum| maximum > MAX_ARGV_TOKEN_BYTES as u64) {
                return Err(ManifestError::invalid(
                    "inputSchema.scalars.maxLength",
                    "문자열의 전역 4,096 bytes 상한을 넘을 수 없습니다",
                ));
            }
            if let (Some(minimum), Some(maximum)) = (min_length, max_length) {
                if minimum > maximum {
                    return Err(ManifestError::invalid(
                        "inputSchema.scalars",
                        "minLength는 maxLength 이하여야 합니다",
                    ));
                }
            }
            if let Some(values) = allowed_values {
                require_count("inputSchema.scalars.enum", values.len(), 1, MAX_ENUM_VALUES)?;
                require_unique_strings("inputSchema.scalars.enum", values)?;
                for value in values {
                    validate_token_text("inputSchema.scalars.enum", value)?;
                    let length = value.chars().count() as u64;
                    if min_length.is_some_and(|minimum| length < minimum)
                        || max_length.is_some_and(|maximum| length > maximum)
                    {
                        return Err(ManifestError::invalid(
                            "inputSchema.scalars.enum",
                            "enum 문자열이 선언된 length 범위 밖입니다",
                        ));
                    }
                }
            }
        }
        ScalarSchema::Integer {
            allowed_values,
            minimum,
            maximum,
            ..
        } => {
            if let (Some(minimum), Some(maximum)) = (minimum, maximum) {
                if minimum > maximum {
                    return Err(ManifestError::invalid(
                        "inputSchema.scalars",
                        "minimum은 maximum 이하여야 합니다",
                    ));
                }
            }
            if let Some(values) = allowed_values {
                require_count("inputSchema.scalars.enum", values.len(), 1, MAX_ENUM_VALUES)?;
                let unique: BTreeSet<_> = values.iter().copied().collect();
                if unique.len() != values.len() {
                    return Err(ManifestError::invalid(
                        "inputSchema.scalars.enum",
                        "중복 값을 허용하지 않습니다",
                    ));
                }
                if values.iter().any(|value| {
                    minimum.is_some_and(|minimum| *value < minimum)
                        || maximum.is_some_and(|maximum| *value > maximum)
                }) {
                    return Err(ManifestError::invalid(
                        "inputSchema.scalars.enum",
                        "enum 정수가 선언된 minimum/maximum 범위 밖입니다",
                    ));
                }
            }
        }
        ScalarSchema::Boolean { allowed_values, .. } => {
            if let Some(values) = allowed_values {
                require_count("inputSchema.scalars.enum", values.len(), 1, 2)?;
                let unique: BTreeSet<_> = values.iter().copied().collect();
                if unique.len() != values.len() {
                    return Err(ManifestError::invalid(
                        "inputSchema.scalars.enum",
                        "중복 값을 허용하지 않습니다",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_argv(profile: &ExecutionProfileManifest) -> Result<(), ManifestError> {
    require_count("argv", profile.argv.len(), 1, MAX_ARGV_TOKENS)?;

    let mut scalar_references: HashSet<&str> = HashSet::new();
    let mut artifact_references: HashMap<&str, usize> = HashMap::new();
    let mut total_bytes = 0_usize;
    for token in &profile.argv {
        let maximum_bytes = match token {
            ArgvToken::Literal { value } => {
                validate_token_text("argv.literal.value", value)?;
                value.len()
            }
            ArgvToken::Artifact { slot } => {
                validate_slot_name("argv.artifact.slot", slot)?;
                if !profile.input_schema.artifacts.contains_key(slot)
                    && !profile.output_schema.artifacts.contains_key(slot)
                {
                    return Err(ManifestError::invalid(
                        "argv.artifact.slot",
                        "선언되지 않은 Artifact slot을 참조합니다",
                    ));
                }
                *artifact_references.entry(slot).or_default() += 1;
                generated_artifact_path_length(
                    slot,
                    profile.output_schema.artifacts.contains_key(slot),
                )
            }
            ArgvToken::Choice { input, cases } => {
                validate_slot_name("argv.choice.input", input)?;
                let schema = profile.input_schema.scalars.get(input).ok_or_else(|| {
                    ManifestError::invalid(
                        "argv.choice.input",
                        "선언되지 않은 scalar input을 참조합니다",
                    )
                })?;
                require_count("argv.choice.cases", cases.len(), 1, MAX_ENUM_VALUES)?;
                let mut equals = BTreeSet::new();
                let mut maximum_bytes = 0;
                for case in cases {
                    if !equals.insert(case.equals.clone()) {
                        return Err(ManifestError::invalid(
                            "argv.choice.cases.equals",
                            "중복 match 값을 허용하지 않습니다",
                        ));
                    }
                    validate_token_text("argv.choice.cases.value", &case.value)?;
                    maximum_bytes = maximum_bytes.max(case.value.len());
                }
                validate_choice_coverage(schema, &equals)?;
                scalar_references.insert(input);
                maximum_bytes
            }
        };
        total_bytes = total_bytes.checked_add(maximum_bytes).ok_or_else(|| {
            ManifestError::invalid("argv", "최대 argv bytes 계산이 overflow했습니다")
        })?;
    }
    if total_bytes > MAX_ARGV_BYTES {
        return Err(ManifestError::invalid(
            "argv",
            format!("최대 실행 plan이 {MAX_ARGV_BYTES} bytes를 넘습니다"),
        ));
    }

    if profile
        .input_schema
        .scalars
        .keys()
        .any(|name| !scalar_references.contains(name.as_str()))
    {
        return Err(ManifestError::invalid(
            "argv",
            "선언한 scalar input이 choice에서 닫혀 있지 않습니다",
        ));
    }
    if profile
        .input_schema
        .artifacts
        .keys()
        .any(|name| !artifact_references.contains_key(name.as_str()))
    {
        return Err(ManifestError::invalid(
            "argv",
            "선언한 input Artifact가 argv에서 닫혀 있지 않습니다",
        ));
    }
    for output in profile.output_schema.artifacts.keys() {
        if artifact_references.get(output.as_str()).copied() != Some(1) {
            return Err(ManifestError::invalid(
                "argv",
                "required output Artifact는 argv에서 정확히 한 번 참조해야 합니다",
            ));
        }
    }
    Ok(())
}

fn validate_choice_coverage(
    schema: &ScalarSchema,
    actual: &BTreeSet<ScalarValue>,
) -> Result<(), ManifestError> {
    let expected: BTreeSet<ScalarValue> = match schema {
        ScalarSchema::String {
            allowed_values: Some(values),
            ..
        } => values.iter().cloned().map(ScalarValue::String).collect(),
        ScalarSchema::Integer {
            allowed_values: Some(values),
            ..
        } => values.iter().copied().map(ScalarValue::Integer).collect(),
        ScalarSchema::Boolean { allowed_values, .. } => allowed_values
            .clone()
            .unwrap_or_else(|| vec![false, true])
            .into_iter()
            .map(ScalarValue::Boolean)
            .collect(),
        ScalarSchema::String {
            allowed_values: None,
            ..
        }
        | ScalarSchema::Integer {
            allowed_values: None,
            ..
        } => {
            return Err(ManifestError::invalid(
                "argv.choice.input",
                "string/integer choice는 유한한 enum을 선언해야 합니다",
            ));
        }
    };
    if &expected != actual {
        return Err(ManifestError::invalid(
            "argv.choice.cases",
            "case가 scalar 입력 domain과 type까지 정확히 일치하지 않습니다",
        ));
    }
    Ok(())
}

fn generated_artifact_path_length(slot: &str, output: bool) -> usize {
    if output {
        "artifacts/out/".len() + slot.len() + ".part".len()
    } else {
        "artifacts/in/".len() + slot.len()
    }
}

fn validate_environment(
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<(), ManifestError> {
    if environment.len() > MAX_ENVIRONMENT_VARIABLES {
        return Err(ManifestError::invalid(
            "environment",
            format!("변수는 최대 {MAX_ENVIRONMENT_VARIABLES}개입니다"),
        ));
    }
    let mut total_bytes = 0_usize;
    for (key, value) in environment {
        if !is_environment_name(key) {
            return Err(ManifestError::invalid(
                "environment",
                "환경 변수 이름 형식이 잘못되었습니다",
            ));
        }
        if key == "PATH" || key.starts_with("LD_") {
            return Err(ManifestError::invalid(
                "environment",
                "PATH와 LD_*는 Runtime Package 실행 계층이 소유하는 reserved 변수입니다",
            ));
        }
        validate_token_text("environment", value)?;
        total_bytes = total_bytes
            .checked_add(key.len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or_else(|| ManifestError::invalid("environment", "크기 계산이 overflow했습니다"))?;
    }
    if total_bytes > MAX_ENVIRONMENT_BYTES {
        return Err(ManifestError::invalid(
            "environment",
            format!("전체 크기는 최대 {MAX_ENVIRONMENT_BYTES} bytes입니다"),
        ));
    }
    Ok(())
}

fn is_environment_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    let mut bytes = value.bytes();
    let first_is_valid = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    first_is_valid && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_resource_policy(
    defaults: &ResourcePolicyValues,
    maximum: &ResourcePolicyValues,
) -> Result<(), ManifestError> {
    validate_resource_values("resourcePolicy.defaults", defaults)?;
    validate_resource_values("resourcePolicy.maxOverrides", maximum)?;

    if defaults.limits.memory_max_bytes > maximum.limits.memory_max_bytes
        || defaults.limits.pids_max > maximum.limits.pids_max
        || defaults.limits.wall_time_limit_ms > maximum.limits.wall_time_limit_ms
        || defaults.output.stdout_tail_max_bytes > maximum.output.stdout_tail_max_bytes
        || defaults.output.stderr_tail_max_bytes > maximum.output.stderr_tail_max_bytes
    {
        return Err(ManifestError::invalid(
            "resourcePolicy",
            "defaults는 maxOverrides의 같은 상한을 넘을 수 없습니다",
        ));
    }

    let default_ratio = u128::from(defaults.limits.cpu_max.quota_micros)
        * u128::from(maximum.limits.cpu_max.period_micros);
    let maximum_ratio = u128::from(maximum.limits.cpu_max.quota_micros)
        * u128::from(defaults.limits.cpu_max.period_micros);
    if default_ratio > maximum_ratio {
        return Err(ManifestError::invalid(
            "resourcePolicy.defaults.limits.cpuMax",
            "기본 CPU 비율은 maxOverrides 비율 이하여야 합니다",
        ));
    }
    Ok(())
}

fn validate_resource_values(
    field: &'static str,
    values: &ResourcePolicyValues,
) -> Result<(), ManifestError> {
    validate_resource_limits(field, &values.limits)?;
    validate_output_limits(field, &values.output)
}

fn validate_resource_limits(
    field: &'static str,
    limits: &ManifestResourceLimits,
) -> Result<(), ManifestError> {
    for value in [
        limits.cpu_max.quota_micros,
        limits.cpu_max.period_micros,
        limits.memory_max_bytes,
        limits.pids_max,
        limits.wall_time_limit_ms,
    ] {
        require_positive(field, value)?;
    }
    Ok(())
}

fn validate_output_limits(
    field: &'static str,
    output: &ManifestOutputLimits,
) -> Result<(), ManifestError> {
    require_positive(field, output.stdout_tail_max_bytes)?;
    require_positive(field, output.stderr_tail_max_bytes)?;
    if output.stdout_tail_max_bytes > MAX_OUTPUT_TAIL_BYTES
        || output.stderr_tail_max_bytes > MAX_OUTPUT_TAIL_BYTES
        || output
            .stdout_tail_max_bytes
            .checked_add(output.stderr_tail_max_bytes)
            .is_none_or(|total| total > MAX_TOTAL_OUTPUT_BYTES)
    {
        return Err(ManifestError::invalid(
            field,
            "output tail은 각각 65,536 bytes, 합계 131,072 bytes 이하여야 합니다",
        ));
    }
    Ok(())
}

pub(super) fn validate_runtime_package(
    package: &RuntimePackageManifest,
) -> Result<(), ManifestError> {
    require_exact("schemaVersion", &package.schema_version, PACKAGE_SCHEMA)?;
    validate_identity("id", &package.id)?;
    validate_semver("version", &package.version)?;
    validate_runtime_platform(package)?;
    validate_relative_path("entrypoint", &package.entrypoint)?;

    if package.library_paths.len() > MAX_LIBRARY_PATHS {
        return Err(ManifestError::invalid(
            "libraryPaths",
            format!("최대 {MAX_LIBRARY_PATHS}개입니다"),
        ));
    }
    validate_unique_paths("libraryPaths", &package.library_paths)?;
    require_count("files", package.files.len(), 1, MAX_PACKAGE_FILES)?;

    package
        .files
        .iter()
        .try_fold(0_u64, |total, file| total.checked_add(file.size_bytes))
        .ok_or_else(|| ManifestError::invalid("files.sizeBytes", "전체 크기가 overflow했습니다"))?;

    let mut previous_path: Option<&str> = None;
    for file in &package.files {
        validate_relative_path("files.path", &file.path)?;
        if previous_path.is_some_and(|previous| previous.as_bytes() >= file.path.as_bytes()) {
            return Err(ManifestError::invalid(
                "files",
                "path byte 순으로 엄격히 정렬되고 중복이 없어야 합니다",
            ));
        }
        previous_path = Some(&file.path);
        if file.mode != "0444" && file.mode != "0555" {
            return Err(ManifestError::invalid(
                "files.mode",
                "0444 또는 0555만 허용합니다",
            ));
        }
    }

    let entrypoint = package
        .files
        .iter()
        .find(|file| file.path == package.entrypoint)
        .ok_or_else(|| {
            ManifestError::invalid("entrypoint", "files 목록의 regular file을 참조해야 합니다")
        })?;
    if entrypoint.mode != "0555" {
        return Err(ManifestError::invalid(
            "entrypoint",
            "entrypoint file mode는 0555여야 합니다",
        ));
    }

    for library_path in &package.library_paths {
        let prefix = format!("{library_path}/");
        if !package
            .files
            .iter()
            .any(|file| file.path.starts_with(&prefix))
        {
            return Err(ManifestError::invalid(
                "libraryPaths",
                "각 library path 아래에 manifest file이 하나 이상 있어야 합니다",
            ));
        }
    }

    if package.licenses.len() > MAX_LICENSES {
        return Err(ManifestError::invalid(
            "licenses",
            format!("최대 {MAX_LICENSES}개입니다"),
        ));
    }
    let file_paths: HashSet<&str> = package
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let mut license_keys = BTreeSet::new();
    for license in &package.licenses {
        validate_spdx_id(&license.spdx_id)?;
        validate_relative_path("licenses.path", &license.path)?;
        if !file_paths.contains(license.path.as_str()) {
            return Err(ManifestError::invalid(
                "licenses.path",
                "files 목록의 regular file을 참조해야 합니다",
            ));
        }
        if !license_keys.insert((&license.spdx_id, &license.path)) {
            return Err(ManifestError::invalid(
                "licenses",
                "중복 license metadata를 허용하지 않습니다",
            ));
        }
    }

    require_exact("sbom.format", &package.sbom.format, "SPDX-JSON-2.3")?;
    validate_relative_path("sbom.path", &package.sbom.path)?;
    if !file_paths.contains(package.sbom.path.as_str()) {
        return Err(ManifestError::invalid(
            "sbom.path",
            "files 목록의 regular file을 참조해야 합니다",
        ));
    }
    Ok(())
}

fn validate_runtime_platform(package: &RuntimePackageManifest) -> Result<(), ManifestError> {
    require_exact("platform.os", &package.platform.os, "linux")?;
    require_exact(
        "platform.architecture",
        &package.platform.architecture,
        "x86_64",
    )?;
    require_exact("platform.abi", &package.platform.abi, "gnu")?;
    require_exact(
        "platform.libc.family",
        &package.platform.libc.family,
        "glibc",
    )?;
    validate_component_version(
        "platform.libc.minimumVersion",
        &package.platform.libc.minimum_version,
    )
}

pub(super) fn validate_bundle(
    bundle: &BundleManifest,
    embedded_profile_digest: Sha256Digest,
    package: &ValidatedRuntimePackage,
) -> Result<(), ManifestError> {
    require_exact("schemaVersion", &bundle.schema_version, BUNDLE_SCHEMA)?;
    validate_identity("id", &bundle.id)?;
    validate_semver("version", &bundle.version)?;
    if bundle.id != bundle.profile.id || bundle.version != bundle.profile.version {
        return Err(ManifestError::invalid(
            "id/version",
            "Bundle identity는 embedded Profile과 정확히 같아야 합니다",
        ));
    }

    validate_identity("runtimePackage.id", &bundle.runtime_package.id)?;
    validate_semver("runtimePackage.version", &bundle.runtime_package.version)?;
    let package_manifest = package.manifest();
    if bundle.runtime_package.id != package_manifest.id
        || bundle.runtime_package.version != package_manifest.version
        || bundle.runtime_package.digest != package.digest()
    {
        return Err(ManifestError::invalid(
            "runtimePackage",
            "검증된 Runtime Package identity와 digest를 정확히 참조해야 합니다",
        ));
    }
    if bundle.profile.entrypoint != package_manifest.entrypoint {
        return Err(ManifestError::invalid(
            "profile.entrypoint",
            "참조한 Runtime Package entrypoint와 같아야 합니다",
        ));
    }

    require_exact("platform.os", &bundle.platform.os, "linux")?;
    require_exact(
        "platform.architecture",
        &bundle.platform.architecture,
        "x86_64",
    )?;
    require_exact("platform.abi", &bundle.platform.abi, "gnu")?;
    if bundle.platform.os != package_manifest.platform.os
        || bundle.platform.architecture != package_manifest.platform.architecture
        || bundle.platform.abi != package_manifest.platform.abi
    {
        return Err(ManifestError::invalid(
            "platform",
            "Bundle과 Runtime Package platform이 같아야 합니다",
        ));
    }

    require_exact(
        "policy.resourcePolicySource",
        &bundle.policy.resource_policy_source,
        "PROFILE",
    )?;
    if bundle.policy.artifact_inputs != ["LOCAL_INPUT"]
        || bundle.policy.artifact_outputs != ["LOCAL_FILE"]
    {
        return Err(ManifestError::invalid(
            "policy.artifactInputs/artifactOutputs",
            "v0.2 Local Artifact kind만 정확히 선언해야 합니다",
        ));
    }
    require_exact(
        "policy.outputPublication",
        &bundle.policy.output_publication,
        "PROFILE_DECLARED",
    )?;
    if bundle.policy.overwrite_published_artifacts {
        return Err(ManifestError::invalid(
            "policy.overwritePublishedArtifacts",
            "false여야 합니다",
        ));
    }

    require_exact(
        "integrity.algorithm",
        &bundle.integrity.algorithm,
        "SHA-256",
    )?;
    if bundle.integrity.profile_digest != embedded_profile_digest {
        return Err(ManifestError::invalid(
            "integrity.profileDigest",
            "embedded Profile canonical digest와 같아야 합니다",
        ));
    }
    if bundle.integrity.runtime_package_digest != bundle.runtime_package.digest
        || bundle.integrity.runtime_package_digest != package.digest()
    {
        return Err(ManifestError::invalid(
            "integrity.runtimePackageDigest",
            "Runtime Package 참조와 검증된 Package digest가 모두 같아야 합니다",
        ));
    }
    Ok(())
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES {
        return Err(ManifestError::invalid(
            field,
            format!("identity는 1~{MAX_ID_BYTES} ASCII bytes여야 합니다"),
        ));
    }
    let segments: Vec<_> = value.split('.').collect();
    if segments.len() < 2
        || segments
            .iter()
            .any(|segment| !is_canonical_name(segment, false))
    {
        return Err(ManifestError::invalid(
            field,
            "점으로 구분한 canonical lowercase identity여야 합니다",
        ));
    }
    Ok(())
}

fn validate_slot_name(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if !is_canonical_name(value, true) {
        return Err(ManifestError::invalid(
            field,
            "소문자로 시작하는 1~64자 ASCII slot name이어야 합니다",
        ));
    }
    Ok(())
}

fn is_canonical_name(value: &str, allow_uppercase_after_first: bool) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..].iter().all(|byte| {
        byte.is_ascii_lowercase()
            || (allow_uppercase_after_first && byte.is_ascii_uppercase())
            || byte.is_ascii_digit()
            || matches!(byte, b'_' | b'-')
    })
}

fn validate_semver(field: &'static str, value: &str) -> Result<(), ManifestError> {
    let version = Version::parse(value)
        .map_err(|_| ManifestError::invalid(field, "canonical SemVer 문자열이어야 합니다"))?;
    if version.to_string() != value {
        return Err(ManifestError::invalid(
            field,
            "canonical SemVer 문자열이어야 합니다",
        ));
    }
    Ok(())
}

fn validate_component_version(field: &'static str, value: &str) -> Result<(), ManifestError> {
    let components: Vec<_> = value.split('.').collect();
    if !(2..=3).contains(&components.len())
        || components.iter().any(|component| {
            component.is_empty()
                || !component.bytes().all(|byte| byte.is_ascii_digit())
                || (component.len() > 1 && component.starts_with('0'))
                || component.parse::<u32>().is_err()
        })
    {
        return Err(ManifestError::invalid(
            field,
            "두세 개의 canonical 숫자 component여야 합니다",
        ));
    }
    Ok(())
}

fn validate_relative_path(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > MAX_ARGV_TOKEN_BYTES
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || value
            .chars()
            .any(|character| character == '\0' || character.is_ascii_control())
    {
        return Err(ManifestError::invalid(
            field,
            "canonical relative path 형식이 아닙니다",
        ));
    }
    let mut segments = value.split('/');
    let first = segments
        .next()
        .expect("a non-empty string has one path segment");
    if first == ".taskcage"
        || first.is_empty()
        || first == "."
        || first == ".."
        || segments.any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(ManifestError::invalid(
            field,
            "reserved, empty, dot 또는 dot-dot segment를 허용하지 않습니다",
        ));
    }
    Ok(())
}

fn validate_path_segment(field: &'static str, value: &str) -> Result<(), ManifestError> {
    validate_relative_path(field, value)?;
    if value.contains('/') {
        return Err(ManifestError::invalid(
            field,
            "slash 없는 path segment 하나여야 합니다",
        ));
    }
    Ok(())
}

fn validate_unique_paths(field: &'static str, paths: &[String]) -> Result<(), ManifestError> {
    let mut unique = HashSet::new();
    for path in paths {
        validate_relative_path(field, path)?;
        if !unique.insert(path) {
            return Err(ManifestError::invalid(
                field,
                "중복 path를 허용하지 않습니다",
            ));
        }
    }
    Ok(())
}

fn validate_media_type(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > 255
        || !value.is_ascii()
        || value.matches('/').count() != 1
        || value.starts_with('/')
        || value.ends_with('/')
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'/' | b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
    {
        return Err(ManifestError::invalid(
            field,
            "parameter 없는 canonical ASCII media type이어야 합니다",
        ));
    }
    Ok(())
}

fn validate_spdx_id(value: &str) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(ManifestError::invalid(
            "licenses.spdxId",
            "canonical SPDX identifier 형식이 아닙니다",
        ));
    }
    Ok(())
}

fn validate_token_text(field: &'static str, value: &str) -> Result<(), ManifestError> {
    if value.len() > MAX_ARGV_TOKEN_BYTES || value.contains('\0') {
        return Err(ManifestError::invalid(
            field,
            "NUL 없이 최대 4,096 UTF-8 bytes여야 합니다",
        ));
    }
    Ok(())
}

fn require_exact(
    field: &'static str,
    actual: &str,
    expected: &'static str,
) -> Result<(), ManifestError> {
    if actual != expected {
        return Err(ManifestError::invalid(
            field,
            format!("{expected} 값만 지원합니다"),
        ));
    }
    Ok(())
}

fn require_positive(field: &'static str, value: u64) -> Result<(), ManifestError> {
    if value == 0 {
        return Err(ManifestError::invalid(field, "0보다 커야 합니다"));
    }
    Ok(())
}

fn require_count(
    field: &'static str,
    actual: usize,
    minimum: usize,
    maximum: usize,
) -> Result<(), ManifestError> {
    if actual < minimum || actual > maximum {
        return Err(ManifestError::invalid(
            field,
            format!("항목 수는 {minimum}~{maximum}개여야 합니다"),
        ));
    }
    Ok(())
}

fn require_unique_strings(field: &'static str, values: &[String]) -> Result<(), ManifestError> {
    let unique: HashSet<_> = values.iter().collect();
    if unique.len() != values.len() {
        return Err(ManifestError::invalid(
            field,
            "중복 문자열을 허용하지 않습니다",
        ));
    }
    Ok(())
}
