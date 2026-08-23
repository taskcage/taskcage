//! File-copy reference Capsule resolver다.

use std::fs;
use std::path::Path;

use crate::artifact::{ArtifactStoreError, DeclaredOutputArtifact};
use crate::protocol::{ErrorCode, ProfileInputValue, ProfileRequestPayload};
use crate::resource_budget::ResourceBudget;

use super::{parse_source, resolve_budget};
use crate::application::capsule::registry::{
    ProfileError, ProfileStartupError, ResolvedProfile, VerifiedProfileExecution,
};
use crate::application::capsule::resolver::{CapsuleResolution, CapsuleResolver};

pub(crate) const FILE_COPY_PROFILE_NAME: &str = "file-copy";
pub(crate) const FILE_COPY_PROFILE_VERSION: &str = "1.0.0";
pub(crate) const FILE_COPY_PROGRAM: &str = "/usr/bin/cp";
const FILE_COPY_OUTPUT_SLOT: &str = "result";
const FILE_COPY_OUTPUT_FILE: &str = "result.txt";
const FILE_COPY_OUTPUT_MEDIA_TYPE: &str = "text/plain";

#[derive(Debug)]
pub(crate) struct FileCopyResolver {
    maximum_artifact_bytes: u64,
    default_budget: ResourceBudget,
}

impl FileCopyResolver {
    pub(crate) fn open(
        maximum_artifact_bytes: u64,
        default_budget: ResourceBudget,
    ) -> Result<Self, ProfileStartupError> {
        let program = Path::new(FILE_COPY_PROGRAM);
        let metadata = fs::metadata(program).map_err(|source| ArtifactStoreError::Io {
            operation: "file-copy profile program 확인",
            path: program.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ArtifactStoreError::NotRegularFile(program.to_path_buf()).into());
        }
        Ok(Self {
            maximum_artifact_bytes,
            default_budget,
        })
    }
}

impl CapsuleResolver for FileCopyResolver {
    fn resolve(&self, request: &ProfileRequestPayload) -> Result<CapsuleResolution, ProfileError> {
        if request.profile.name != FILE_COPY_PROFILE_NAME
            || request.profile.version != FILE_COPY_PROFILE_VERSION
        {
            return Ok(CapsuleResolution::NotFound);
        }
        if request.inputs.len() != 4 {
            return Err(ProfileError::new(
                ErrorCode::InvalidProfileInput,
                "file-copy requires exactly source, label, retain_metadata, and priority inputs",
            ));
        }
        let source = parse_source(request)?;
        match request.inputs.get("label") {
            Some(ProfileInputValue::String { value })
                if !value.is_empty() && value.len() <= 128 => {}
            Some(ProfileInputValue::String { .. }) => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.label must contain 1 to 128 bytes",
                ));
            }
            _ => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.label must be STRING",
                ));
            }
        }
        if !matches!(
            request.inputs.get("retain_metadata"),
            Some(ProfileInputValue::Boolean { .. })
        ) {
            return Err(ProfileError::new(
                ErrorCode::InvalidProfileInput,
                "inputs.retain_metadata must be BOOLEAN",
            ));
        }
        match request.inputs.get("priority") {
            Some(ProfileInputValue::Int64 { value }) if (0..=100).contains(value) => {}
            Some(ProfileInputValue::Int64 { .. }) => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.priority must be INT64 between 0 and 100",
                ));
            }
            _ => {
                return Err(ProfileError::new(
                    ErrorCode::InvalidProfileInput,
                    "inputs.priority must be INT64",
                ));
            }
        }

        let budget = resolve_budget(&self.default_budget, request.resource_overrides.as_ref())?;
        let output = DeclaredOutputArtifact::new(
            FILE_COPY_OUTPUT_FILE,
            FILE_COPY_OUTPUT_MEDIA_TYPE,
            self.maximum_artifact_bytes,
        )
        .expect("static file-copy output contract must be valid");
        Ok(CapsuleResolution::Resolved(Box::new(ResolvedProfile {
            request: request.clone(),
            source,
            output,
            budget,
            execution: VerifiedProfileExecution::BuiltInFileCopy,
            output_slot: FILE_COPY_OUTPUT_SLOT.to_owned(),
        })))
    }
}
