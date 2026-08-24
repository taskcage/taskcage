//! 서명된 Bundle catalog에 설치된 immutable Capsule을 해석한다.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::application::{ProfileSubmission, UseCaseErrorCode};
use crate::artifact::DeclaredOutputArtifact;
use crate::bundle::{BundleCatalog, BundleError};
use crate::capsule::{CapsuleCatalog, CapsuleError};
use crate::execution_plan::ResolvedExecutionPlan;
use crate::profile_invocation::{
    InputValueRejection, InvocationError, VerifiedArgument, verify_profile_call,
};
use crate::resource_budget::ResourceBudget;
use crate::runtime_package::RuntimePackageCache;

use super::ProfileStartupError;
use crate::application::capsule::registry::{ProfileError, ResolvedProfile};
#[cfg(test)]
use crate::application::capsule::resolver::ProfileExecutionKind;
use crate::application::capsule::resolver::{CapsuleResolution, CapsuleResolver, ProfileExecution};

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
    fn resolve(&self, request: &ProfileSubmission) -> Result<CapsuleResolution, ProfileError> {
        let capsule = match CapsuleCatalog::new(&self.catalog).compile(
            request.call().identity().name(),
            request.call().identity().version(),
        ) {
            Ok(capsule) => capsule,
            Err(CapsuleError::LegacyBundle(BundleError::NotFound { .. })) => {
                return Ok(CapsuleResolution::NotFound);
            }
            Err(error) => {
                return Err(ProfileError::new(
                    UseCaseErrorCode::EnvironmentUnavailable,
                    format!("installed Capsule catalog is unavailable: {error}"),
                ));
            }
        };
        let invocation = verify_profile_call(&capsule, request.call().clone())
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
                    UseCaseErrorCode::EnvironmentUnavailable,
                    format!("Capsule Runtime Package is unavailable: {error}"),
                )
            })?;
        let entrypoint = package.entrypoint().try_clone().map_err(|error| {
            ProfileError::new(
                UseCaseErrorCode::EnvironmentUnavailable,
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
        .map_err(|error| {
            ProfileError::new(UseCaseErrorCode::InvalidProfileInput, error.to_string())
        })?;

        Ok(CapsuleResolution::Resolved(Box::new(ResolvedProfile::new(
            request.clone(),
            source,
            output,
            budget,
            Box::new(InstalledExecution {
                entrypoint,
                arguments,
            }),
            output_contract.name.clone(),
        ))))
    }
}

#[derive(Debug)]
struct InstalledExecution {
    entrypoint: File,
    arguments: Vec<VerifiedArgument>,
}

impl ProfileExecution for InstalledExecution {
    fn into_plan(
        self: Box<Self>,
        profile_name: &str,
        input: PathBuf,
        output: PathBuf,
        working_directory: PathBuf,
        budget: ResourceBudget,
    ) -> ResolvedExecutionPlan {
        ResolvedExecutionPlan::from_pinned_entrypoint(
            self.entrypoint,
            OsString::from(profile_name),
            self.arguments
                .into_iter()
                .map(|argument| match argument {
                    VerifiedArgument::Literal(value) => OsString::from(value),
                    VerifiedArgument::InputArtifactPath { .. } => input.as_os_str().to_owned(),
                    VerifiedArgument::OutputArtifactPath { .. } => output.as_os_str().to_owned(),
                })
                .collect(),
            working_directory,
            BTreeMap::new(),
            budget,
        )
    }

    #[cfg(test)]
    fn kind(&self) -> ProfileExecutionKind {
        ProfileExecutionKind::Bundle
    }
}

pub(crate) fn profile_invocation_error(error: InvocationError) -> ProfileError {
    let code = match &error {
        InvocationError::CapsuleProfileNotFound { .. } => UseCaseErrorCode::ProfileNotFound,
        InvocationError::PolicyRejected { .. } => UseCaseErrorCode::LimitExceedsPolicy,
        InvocationError::InvalidCompiledContract { .. } => UseCaseErrorCode::EnvironmentUnavailable,
        InvocationError::InputValueRejected {
            rejection: InputValueRejection::ArtifactPath,
            ..
        } => UseCaseErrorCode::InvalidArtifactPath,
        InvocationError::InvalidProfileIdentity { .. }
        | InvocationError::DuplicateInput { .. }
        | InvocationError::InputSetMismatch { .. }
        | InvocationError::InputTypeMismatch { .. }
        | InvocationError::InputValueRejected { .. }
        | InvocationError::InvalidResourceOverride { .. } => UseCaseErrorCode::InvalidProfileInput,
    };
    ProfileError::new(code, error.to_string())
}
