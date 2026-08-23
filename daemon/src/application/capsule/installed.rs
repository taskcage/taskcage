//! 서명된 Bundle catalog에 설치된 immutable Capsule을 해석한다.

use std::path::Path;

use crate::artifact::DeclaredOutputArtifact;
use crate::bundle::{BundleCatalog, BundleError};
use crate::capsule::{CapsuleCatalog, CapsuleError};
use crate::profile_invocation::{InputValueRejection, InvocationError, verify_profile_call};
use crate::protocol::{ErrorCode, ProfileRequestPayload};
use crate::protocol_mapper;
use crate::runtime_package::RuntimePackageCache;

use super::registry::{
    ProfileError, ProfileStartupError, ResolvedProfile, VerifiedProfileExecution,
};
use super::resolver::{CapsuleResolution, CapsuleResolver};

#[derive(Debug)]
pub(super) struct InstalledCapsuleResolver {
    maximum_artifact_bytes: u64,
    catalog: BundleCatalog,
    packages: RuntimePackageCache,
}

impl InstalledCapsuleResolver {
    pub(super) fn open(
        cache_root: &Path,
        maximum_artifact_bytes: u64,
    ) -> Result<Self, ProfileStartupError> {
        Ok(Self {
            maximum_artifact_bytes,
            catalog: BundleCatalog::open(cache_root).map_err(ProfileStartupError::Bundle)?,
            packages: RuntimePackageCache::open(cache_root)?,
        })
    }
}

impl CapsuleResolver for InstalledCapsuleResolver {
    fn resolve(&self, request: &ProfileRequestPayload) -> Result<CapsuleResolution, ProfileError> {
        let capsule = match CapsuleCatalog::new(&self.catalog)
            .compile(&request.profile.name, &request.profile.version)
        {
            Ok(capsule) => capsule,
            Err(CapsuleError::LegacyBundle(BundleError::NotFound { .. })) => {
                return Ok(CapsuleResolution::NotFound);
            }
            Err(error) => {
                return Err(ProfileError::new(
                    ErrorCode::EnvironmentUnavailable,
                    format!("installed Capsule catalog is unavailable: {error}"),
                ));
            }
        };
        let invocation = verify_profile_call(&capsule, protocol_mapper::profile_call(request))
            .map_err(profile_invocation_error)?;
        let source = invocation
            .input_artifacts()
            .values()
            .next()
            .expect("VerifiedInvocation must contain exactly one LOCAL_INPUT")
            .clone();
        let budget = invocation.effective_resources().clone();
        let arguments = invocation.arguments().to_vec();
        let package = self
            .packages
            .resolve(invocation.runtime().package_digest())
            .map_err(|error| {
                ProfileError::new(
                    ErrorCode::EnvironmentUnavailable,
                    format!("Capsule Runtime Package is unavailable: {error}"),
                )
            })?;
        let entrypoint = package.entrypoint().try_clone().map_err(|error| {
            ProfileError::new(
                ErrorCode::EnvironmentUnavailable,
                format!("verified Capsule entrypoint descriptor could not be pinned: {error}"),
            )
        })?;
        let output_contract = invocation.declared_output();
        let output = DeclaredOutputArtifact::new(
            &output_contract.file_name,
            &output_contract.media_type,
            output_contract
                .maximum_bytes
                .min(self.maximum_artifact_bytes),
        )
        .map_err(|error| ProfileError::new(ErrorCode::InvalidProfileInput, error.to_string()))?;

        Ok(CapsuleResolution::Resolved(Box::new(ResolvedProfile {
            request: request.clone(),
            source,
            output,
            budget,
            execution: VerifiedProfileExecution::Bundle {
                entrypoint,
                arguments,
            },
            output_slot: output_contract.name.clone(),
        })))
    }
}

pub(super) fn profile_invocation_error(error: InvocationError) -> ProfileError {
    let code = match &error {
        InvocationError::CapsuleProfileNotFound { .. } => ErrorCode::ProfileNotFound,
        InvocationError::PolicyRejected { .. } => ErrorCode::LimitExceedsPolicy,
        InvocationError::InvalidCompiledContract { .. } => ErrorCode::EnvironmentUnavailable,
        InvocationError::InputValueRejected {
            rejection: InputValueRejection::ArtifactPath,
            ..
        } => ErrorCode::InvalidArtifactPath,
        InvocationError::InvalidProfileIdentity { .. }
        | InvocationError::DuplicateInput { .. }
        | InvocationError::InputSetMismatch { .. }
        | InvocationError::InputTypeMismatch { .. }
        | InvocationError::InputValueRejected { .. }
        | InvocationError::InvalidResourceOverride { .. } => ErrorCode::InvalidProfileInput,
    };
    ProfileError::new(code, error.to_string())
}
