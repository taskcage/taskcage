use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256Digest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionProfileManifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub entrypoint: String,
    pub input_schema: InputSchema,
    pub output_schema: OutputSchema,
    pub argv: Vec<ArgvToken>,
    pub environment: BTreeMap<String, String>,
    pub resource_policy: ResourcePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputSchema {
    pub scalars: BTreeMap<String, ScalarSchema>,
    pub artifacts: BTreeMap<String, InputArtifactSchema>,
    pub additional_properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ScalarSchema {
    String {
        required: bool,
        #[serde(rename = "enum")]
        allowed_values: Option<Vec<String>>,
        min_length: Option<u64>,
        max_length: Option<u64>,
    },
    Integer {
        required: bool,
        #[serde(rename = "enum")]
        allowed_values: Option<Vec<i64>>,
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    Boolean {
        required: bool,
        #[serde(rename = "enum")]
        allowed_values: Option<Vec<bool>>,
    },
}

impl ScalarSchema {
    pub fn required(&self) -> bool {
        match self {
            Self::String { required, .. }
            | Self::Integer { required, .. }
            | Self::Boolean { required, .. } => *required,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputArtifactSchema {
    pub kind: InputArtifactKind,
    pub required: bool,
    pub media_types: Vec<String>,
    pub max_size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputArtifactKind {
    #[serde(rename = "LOCAL_INPUT")]
    LocalInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputSchema {
    pub artifacts: BTreeMap<String, OutputArtifactSchema>,
    pub additional_properties: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputArtifactSchema {
    pub kind: OutputArtifactKind,
    pub required: bool,
    pub media_type: String,
    pub max_size_bytes: u64,
    pub file_name: String,
    pub publication: OutputPublication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputArtifactKind {
    #[serde(rename = "LOCAL_FILE")]
    LocalFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputPublication {
    #[serde(rename = "TASK_SCOPED")]
    TaskScoped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ArgvToken {
    Literal {
        value: String,
    },
    Choice {
        input: String,
        cases: Vec<ChoiceCase>,
    },
    Artifact {
        slot: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceCase {
    pub equals: ScalarValue,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    Boolean(bool),
    Integer(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    pub defaults: ResourcePolicyValues,
    #[serde(rename = "maxOverrides")]
    pub max_overrides: ResourcePolicyValues,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicyValues {
    pub limits: ManifestResourceLimits,
    pub output: ManifestOutputLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestResourceLimits {
    pub cpu_max: ManifestCpuMax,
    pub memory_max_bytes: u64,
    pub pids_max: u64,
    pub wall_time_limit_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestCpuMax {
    pub quota_micros: u64,
    pub period_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestOutputLimits {
    pub stdout_tail_max_bytes: u64,
    pub stderr_tail_max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePackageManifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub platform: RuntimePlatform,
    pub entrypoint: String,
    pub library_paths: Vec<String>,
    pub files: Vec<PackageFile>,
    pub licenses: Vec<PackageLicense>,
    pub sbom: PackageSbom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePlatform {
    pub os: String,
    pub architecture: String,
    pub abi: String,
    pub libc: RuntimeLibc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLibc {
    pub family: String,
    pub minimum_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub digest: Sha256Digest,
    pub size_bytes: u64,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageLicense {
    pub spdx_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSbom {
    pub format: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleManifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub profile: ExecutionProfileManifest,
    pub runtime_package: RuntimePackageReference,
    pub platform: BundlePlatform,
    pub policy: BundlePolicy,
    pub integrity: BundleIntegrity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePackageReference {
    pub id: String,
    pub version: String,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePlatform {
    pub os: String,
    pub architecture: String,
    pub abi: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundlePolicy {
    pub resource_policy_source: String,
    pub artifact_inputs: Vec<String>,
    pub artifact_outputs: Vec<String>,
    pub output_publication: String,
    pub overwrite_published_artifacts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleIntegrity {
    pub algorithm: String,
    pub profile_digest: Sha256Digest,
    pub runtime_package_digest: Sha256Digest,
}
