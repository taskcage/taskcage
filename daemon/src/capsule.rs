//! Runtime-facing Capsule compatibility layer over signed Bundle v0alpha1 storage.
//!
//! The Bundle archive, verifier, digest domain, and catalog remain the physical trust boundary.
//! This module gives runtime code an immutable, typed Capsule contract without introducing a new
//! archive format or allowing unverified manifests to become executable contracts.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::bundle::{
    BundleCatalog, BundleError, BundleInput, BundleInspection, BundleManifest, BundleProfile,
    InstalledBundle,
};
pub use crate::bundle::{
    BundleOutput as CapsuleOutput, BundleProfileArgument as CapsuleArgument,
    BundleProfileInputValueKind as CapsuleInputValueKind,
    BundleResourcePolicy as CapsuleResourcePolicy,
};
use crate::digest::Sha256Digest;
use crate::protocol::ProfileIdentity;
pub use taskcage_core::{CapsuleIdentity, IdentityError as CapsuleIdentityError};

#[derive(Debug, Error)]
pub enum CapsuleError {
    #[error(transparent)]
    LegacyBundle(#[from] BundleError),
    #[error("검증된 legacy Bundle 계약을 Capsule로 compile할 수 없습니다: {0}")]
    InvalidContract(String),
}

pub type CapsuleResult<T> = std::result::Result<T, CapsuleError>;

/// Existing immutable Bundle catalog를 exact Capsule identity로 조회하는 compatibility facade다.
#[derive(Debug, Clone, Copy)]
pub struct CapsuleCatalog<'a> {
    bundles: &'a BundleCatalog,
}

impl<'a> CapsuleCatalog<'a> {
    pub const fn new(bundles: &'a BundleCatalog) -> Self {
        Self { bundles }
    }

    /// Exact name/version mapping을 조회하고 재검증된 legacy representation을 compile한다.
    pub fn compile(&self, name: &str, version: &str) -> CapsuleResult<CompiledCapsule> {
        let inspection = self.bundles.inspect(name, version)?;
        CompiledCapsule::from_verified_inspection(inspection)
    }
}

/// Runtime and Profile Mapper가 소비하는 immutable Capsule contract다.
///
/// 모든 field는 private이며 mutable accessor나 deserialization 경로를 제공하지 않는다. 유일한 생성
/// 경로는 catalog에서 fresh inspection을 얻어 즉시 소비하는 `CapsuleCatalog::compile`이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCapsule {
    identity: CapsuleIdentity,
    catalog_digest: Sha256Digest,
    runtime: CapsuleRuntimeReference,
    profile: CompiledCapsuleProfile,
    provenance: CapsuleProvenance,
}

impl CompiledCapsule {
    pub fn identity(&self) -> &CapsuleIdentity {
        &self.identity
    }

    pub fn catalog_digest(&self) -> Sha256Digest {
        self.catalog_digest
    }

    pub fn runtime(&self) -> &CapsuleRuntimeReference {
        &self.runtime
    }

    pub fn profile(&self) -> &CompiledCapsuleProfile {
        &self.profile
    }

    pub fn provenance(&self) -> &CapsuleProvenance {
        &self.provenance
    }
}

impl CompiledCapsule {
    fn from_verified_inspection(inspection: BundleInspection) -> CapsuleResult<Self> {
        let (installed, manifest, profile) = inspection.into_parts();
        ensure_inspection_consistency(&installed, &manifest, &profile)?;

        let BundleManifest {
            name,
            version,
            signing_key_id,
            runtime,
            profile_digest,
            ..
        } = manifest;
        let BundleProfile {
            name: profile_name,
            version: profile_version,
            inputs,
            output,
            argv,
            policy,
            allowed_overrides,
            ..
        } = profile;
        let inputs = inputs
            .into_iter()
            .map(CapsuleInput::try_from)
            .collect::<CapsuleResult<Vec<_>>>()?;
        validate_allowed_overrides(&allowed_overrides)?;

        Ok(Self {
            identity: CapsuleIdentity::new(name, version)
                .map_err(|error| CapsuleError::InvalidContract(error.to_string()))?,
            catalog_digest: installed.digest,
            runtime: CapsuleRuntimeReference {
                package_id: runtime.package_id,
                package_digest: runtime.digest,
            },
            profile: CompiledCapsuleProfile {
                identity: ProfileIdentity {
                    name: profile_name,
                    version: profile_version,
                },
                digest: profile_digest,
                inputs,
                arguments: argv,
                output,
                resource_policy: policy,
                allowed_overrides,
            },
            provenance: CapsuleProvenance { signing_key_id },
        })
    }
}

fn ensure_inspection_consistency(
    installed: &InstalledBundle,
    manifest: &BundleManifest,
    profile: &BundleProfile,
) -> CapsuleResult<()> {
    if installed.name != manifest.name
        || installed.version != manifest.version
        || installed.runtime_package_id != manifest.runtime.package_id
        || installed.runtime_package_digest != manifest.runtime.digest
        || profile.name != manifest.name
        || profile.version != manifest.version
    {
        return Err(CapsuleError::InvalidContract(
            "catalog, manifest, Profile 또는 Runtime Package identity가 일치하지 않습니다"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_allowed_overrides(overrides: &[String]) -> CapsuleResult<()> {
    let mut unique = BTreeSet::new();
    for field in overrides {
        if !matches!(
            field.as_str(),
            "limits.cpuMax"
                | "limits.memoryMaxBytes"
                | "limits.pidsMax"
                | "limits.wallTimeLimitMs"
                | "output.stdoutTailMaxBytes"
                | "output.stderrTailMaxBytes"
        ) || !unique.insert(field)
        {
            return Err(CapsuleError::InvalidContract(
                "resource override allowlist가 검증된 v0alpha1 contract가 아닙니다".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Digest-pinned Runtime Package reference다.
///
/// Bundle v0alpha1에는 별도 entrypoint 문자열이 없다. `package_digest`가 Runtime Package
/// manifest와 그 manifest의 entrypoint를 전이적으로 고정하며, 실행 계층은 해당 digest를 resolve해
/// 검증된 entrypoint descriptor를 pin해야 한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleRuntimeReference {
    package_id: String,
    package_digest: Sha256Digest,
}

impl CapsuleRuntimeReference {
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    pub fn package_digest(&self) -> Sha256Digest {
        self.package_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCapsuleProfile {
    identity: ProfileIdentity,
    digest: Sha256Digest,
    inputs: Vec<CapsuleInput>,
    arguments: Vec<CapsuleArgument>,
    output: CapsuleOutput,
    resource_policy: CapsuleResourcePolicy,
    allowed_overrides: Vec<String>,
}

impl CompiledCapsuleProfile {
    pub fn identity(&self) -> &ProfileIdentity {
        &self.identity
    }

    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn inputs(&self) -> &[CapsuleInput] {
        &self.inputs
    }

    pub fn arguments(&self) -> &[CapsuleArgument] {
        &self.arguments
    }

    pub fn output(&self) -> &CapsuleOutput {
        &self.output
    }

    pub fn resource_policy(&self) -> &CapsuleResourcePolicy {
        &self.resource_policy
    }

    pub fn allowed_overrides(&self) -> &[String] {
        &self.allowed_overrides
    }
}

/// v0alpha1 input의 검증된 typed schema다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleInput {
    name: String,
    required: bool,
    kind: CapsuleInputKind,
}

impl CapsuleInput {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn required(&self) -> bool {
        self.required
    }

    pub fn kind(&self) -> &CapsuleInputKind {
        &self.kind
    }
}

impl TryFrom<BundleInput> for CapsuleInput {
    type Error = CapsuleError;

    fn try_from(input: BundleInput) -> Result<Self, Self::Error> {
        let BundleInput {
            name,
            kind,
            required,
            allowed_values,
            minimum,
            maximum,
        } = input;
        if !required {
            return Err(CapsuleError::InvalidContract(format!(
                "v0alpha1 input {name}은 required여야 합니다"
            )));
        }
        let kind = match (kind.as_str(), allowed_values, minimum, maximum) {
            ("LOCAL_INPUT", None, None, None) => CapsuleInputKind::LocalInput,
            ("STRING", None, None, None) => CapsuleInputKind::String,
            ("BOOLEAN", None, None, None) => CapsuleInputKind::Boolean,
            ("INT64", Some(values), None, None)
                if (1..=64).contains(&values.len())
                    && values.windows(2).all(|pair| pair[0] < pair[1]) =>
            {
                CapsuleInputKind::Int64(CapsuleInt64Constraint::AllowedValues(values))
            }
            ("INT64", None, Some(minimum), Some(maximum)) if minimum <= maximum => {
                CapsuleInputKind::Int64(CapsuleInt64Constraint::Range { minimum, maximum })
            }
            _ => {
                return Err(CapsuleError::InvalidContract(format!(
                    "input {name}의 kind 또는 validation contract가 잘못되었습니다"
                )));
            }
        };
        Ok(Self {
            name,
            required,
            kind,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsuleInputKind {
    LocalInput,
    String,
    Int64(CapsuleInt64Constraint),
    Boolean,
}

impl CapsuleInputKind {
    pub fn schema_name(&self) -> &'static str {
        match self {
            Self::LocalInput => "LOCAL_INPUT",
            Self::String => "STRING",
            Self::Int64(_) => "INT64",
            Self::Boolean => "BOOLEAN",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsuleInt64Constraint {
    AllowedValues(Vec<i64>),
    Range { minimum: i64, maximum: i64 },
}

impl CapsuleInt64Constraint {
    pub fn allows(&self, value: i64) -> bool {
        match self {
            Self::AllowedValues(values) => values.binary_search(&value).is_ok(),
            Self::Range { minimum, maximum } => (*minimum..=*maximum).contains(&value),
        }
    }
}

/// Legacy signed manifest가 commit한 signer key ID다.
///
/// Catalog는 signature/checksum receipt를 저장하지 않으므로 이 값은 독립적인 signature receipt가
/// 아니다. Signature 검증 자체는 기존 Bundle importer의 external trust-anchor 경계에 남는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleProvenance {
    signing_key_id: String,
}

impl CapsuleProvenance {
    pub fn signing_key_id(&self) -> &str {
        &self.signing_key_id
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use crate::bundle::test_support::{
        bundle_bytes, catalog_with_runtime_package, profile_bytes, signed_bundle_archive,
        signed_entries, write_archive,
    };
    use crate::bundle::{BundleCatalog, BundleError, BundleImportOutcome};

    use super::*;

    const CAPSULE_NAME: &str = "ffmpeg-audio-to-wav";
    const CAPSULE_VERSION: &str = "1.0.0";

    fn assert_capsule_absent(catalog: &BundleCatalog) {
        assert!(matches!(
            CapsuleCatalog::new(catalog).compile(CAPSULE_NAME, CAPSULE_VERSION),
            Err(CapsuleError::LegacyBundle(BundleError::NotFound { .. }))
        ));
    }

    #[test]
    fn compiles_a_signed_bundle_inspection_to_an_immutable_capsule_contract() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let profile_raw = profile_bytes();
        let (archive, keys) = signed_bundle_archive(&profile_raw, package_digest);
        let imported = catalog.import(archive.path(), &keys).unwrap();
        assert_eq!(imported.outcome, BundleImportOutcome::Imported);

        let capsule = CapsuleCatalog::new(&catalog)
            .compile(CAPSULE_NAME, CAPSULE_VERSION)
            .unwrap();
        let mut detached_inspection = catalog.inspect(CAPSULE_NAME, CAPSULE_VERSION).unwrap();
        detached_inspection.profile.argv = vec![CapsuleArgument::Literal("mutated".to_owned())];
        let recompiled = CapsuleCatalog::new(&catalog)
            .compile(CAPSULE_NAME, CAPSULE_VERSION)
            .unwrap();

        assert_ne!(
            detached_inspection.profile.argv.as_slice(),
            recompiled.profile().arguments()
        );
        assert_eq!(capsule, recompiled);
        assert_eq!(capsule.identity().name(), CAPSULE_NAME);
        assert_eq!(capsule.identity().version(), CAPSULE_VERSION);
        assert_eq!(capsule.catalog_digest(), imported.digest);
        assert_eq!(capsule.runtime().package_id(), "org.taskcage.ffmpeg");
        assert_eq!(capsule.runtime().package_digest(), package_digest);
        assert_eq!(capsule.profile().identity().name, CAPSULE_NAME);
        assert_eq!(capsule.profile().identity().version, CAPSULE_VERSION);
        assert_eq!(
            capsule.profile().digest(),
            Sha256Digest::from_bytes(Sha256::digest(&profile_raw).into())
        );
        assert_eq!(capsule.provenance().signing_key_id(), "test-release");

        let inputs = capsule.profile().inputs();
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].name(), "source");
        assert!(inputs[0].required());
        assert!(matches!(inputs[0].kind(), CapsuleInputKind::LocalInput));
        assert!(matches!(
            inputs[1].kind(),
            CapsuleInputKind::Int64(CapsuleInt64Constraint::AllowedValues(values))
                if values == &[8_000, 16_000, 22_050, 44_100, 48_000]
        ));
        assert!(matches!(
            inputs[2].kind(),
            CapsuleInputKind::Int64(CapsuleInt64Constraint::AllowedValues(values))
                if values == &[1, 2]
        ));

        let arguments = capsule.profile().arguments();
        assert_eq!(arguments.len(), 16);
        assert!(matches!(
            &arguments[5],
            CapsuleArgument::InputPath { slot } if slot == "source"
        ));
        assert!(matches!(
            &arguments[12],
            CapsuleArgument::InputValue {
                kind: CapsuleInputValueKind::Int64,
                slot,
            } if slot == "sample_rate_hz"
        ));
        assert!(matches!(
            &arguments[15],
            CapsuleArgument::OutputPath { slot } if slot == "audio"
        ));

        let output = capsule.profile().output();
        assert_eq!(output.name, "audio");
        assert_eq!(output.file_name, "result.wav");
        assert_eq!(output.media_type, "audio/wav");
        assert_eq!(output.maximum_bytes, 1024);
        assert_eq!(
            capsule.profile().resource_policy().limits.memory_max_bytes,
            536_870_912
        );
        assert!(capsule.profile().allowed_overrides().is_empty());
    }

    #[test]
    fn invalid_signature_never_reaches_capsule_compilation() {
        let (_root, catalog, package_digest) = catalog_with_runtime_package();
        let profile = profile_bytes();
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
        assert_capsule_absent(&catalog);
    }

    #[test]
    fn checksum_or_profile_digest_mismatch_never_reaches_capsule_compilation() {
        let (_root, checksum_catalog, package_digest) = catalog_with_runtime_package();
        let profile = profile_bytes();
        let bundle = bundle_bytes(&profile, &package_digest.to_string());
        let (mut entries, keys) = signed_entries(bundle, profile);
        entries
            .iter_mut()
            .find(|(name, _)| *name == "profile.json")
            .unwrap()
            .1
            .push(b' ');
        let checksum_mismatch = write_archive(entries);
        assert!(matches!(
            checksum_catalog.import(checksum_mismatch.path(), &keys),
            Err(BundleError::Archive(_))
        ));
        assert_capsule_absent(&checksum_catalog);

        let (_root, digest_catalog, package_digest) = catalog_with_runtime_package();
        let original_profile = profile_bytes();
        let bundle = bundle_bytes(&original_profile, &package_digest.to_string());
        let mut changed_profile: serde_json::Value =
            serde_json::from_slice(&original_profile).unwrap();
        changed_profile["output"]["maximumBytes"] = serde_json::json!(2048);
        let changed_profile = serde_json::to_vec(&changed_profile).unwrap();
        let (entries, keys) = signed_entries(bundle, changed_profile);
        let digest_mismatch = write_archive(entries);
        assert!(matches!(
            digest_catalog.import(digest_mismatch.path(), &keys),
            Err(BundleError::Manifest(_))
        ));
        assert_capsule_absent(&digest_catalog);
    }

    #[test]
    fn profile_identity_mismatch_or_a_second_profile_is_rejected_before_compilation() {
        let (_root, identity_catalog, package_digest) = catalog_with_runtime_package();
        let profile = profile_bytes();
        let bundle = bundle_bytes(&profile, &package_digest.to_string());
        let mut bundle_value: serde_json::Value = serde_json::from_slice(&bundle).unwrap();
        bundle_value["name"] = serde_json::json!("different-capsule");
        let (entries, keys) = signed_entries(serde_json::to_vec(&bundle_value).unwrap(), profile);
        let identity_mismatch = write_archive(entries);
        assert!(matches!(
            identity_catalog.import(identity_mismatch.path(), &keys),
            Err(BundleError::Manifest(_))
        ));
        assert_capsule_absent(&identity_catalog);

        let (_root, profile_count_catalog, package_digest) = catalog_with_runtime_package();
        let profile = profile_bytes();
        let bundle = bundle_bytes(&profile, &package_digest.to_string());
        let (mut entries, keys) = signed_entries(bundle, profile.clone());
        entries.push(("second-profile.json", profile));
        let two_profiles = write_archive(entries);
        assert!(matches!(
            profile_count_catalog.import(two_profiles.path(), &keys),
            Err(BundleError::Archive(_))
        ));
        assert_capsule_absent(&profile_count_catalog);
    }
}
