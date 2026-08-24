//! File-copy reference Capsule resolver다.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::application::{ProfileSubmission, UseCaseErrorCode};
use crate::artifact::{ArtifactStoreError, DeclaredOutputArtifact};
use crate::execution_plan::{RawCommand, ResolvedExecutionPlan};
use crate::resource_budget::ResourceBudget;
use taskcage_core::capsule::ProfileValue;

use super::{input, parse_source, resolve_budget};
use crate::adapters::outbound::capsule_resolvers::ProfileStartupError;
use crate::application::capsule::registry::{ProfileError, ResolvedProfile};
#[cfg(test)]
use crate::application::capsule::resolver::ProfileExecutionKind;
use crate::application::capsule::resolver::{CapsuleResolution, CapsuleResolver, ProfileExecution};

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
    fn resolve(&self, request: &ProfileSubmission) -> Result<CapsuleResolution, ProfileError> {
        let call = request.call();
        if call.identity().name() != FILE_COPY_PROFILE_NAME
            || call.identity().version() != FILE_COPY_PROFILE_VERSION
        {
            return Ok(CapsuleResolution::NotFound);
        }
        if call.inputs().len() != 4 {
            return Err(ProfileError::new(
                UseCaseErrorCode::InvalidProfileInput,
                "file-copy requires exactly source, label, retain_metadata, and priority inputs",
            ));
        }
        let source = parse_source(call)?;
        match input(call, "label") {
            Some(ProfileValue::String(value)) if !value.is_empty() && value.len() <= 128 => {}
            Some(ProfileValue::String(_)) => {
                return Err(ProfileError::new(
                    UseCaseErrorCode::InvalidProfileInput,
                    "inputs.label must contain 1 to 128 bytes",
                ));
            }
            _ => {
                return Err(ProfileError::new(
                    UseCaseErrorCode::InvalidProfileInput,
                    "inputs.label must be STRING",
                ));
            }
        }
        if !matches!(
            input(call, "retain_metadata"),
            Some(ProfileValue::Boolean(_))
        ) {
            return Err(ProfileError::new(
                UseCaseErrorCode::InvalidProfileInput,
                "inputs.retain_metadata must be BOOLEAN",
            ));
        }
        match input(call, "priority") {
            Some(ProfileValue::Int64(value)) if (0..=100).contains(value) => {}
            Some(ProfileValue::Int64(_)) => {
                return Err(ProfileError::new(
                    UseCaseErrorCode::InvalidProfileInput,
                    "inputs.priority must be INT64 between 0 and 100",
                ));
            }
            _ => {
                return Err(ProfileError::new(
                    UseCaseErrorCode::InvalidProfileInput,
                    "inputs.priority must be INT64",
                ));
            }
        }

        let budget = resolve_budget(&self.default_budget, call.resource_overrides())?;
        let output = DeclaredOutputArtifact::new(
            FILE_COPY_OUTPUT_FILE,
            FILE_COPY_OUTPUT_MEDIA_TYPE,
            self.maximum_artifact_bytes,
        )
        .expect("static file-copy output contract must be valid");
        Ok(CapsuleResolution::Resolved(Box::new(ResolvedProfile::new(
            request.clone(),
            source,
            output,
            budget,
            Box::new(FileCopyExecution),
            FILE_COPY_OUTPUT_SLOT.to_owned(),
        ))))
    }
}

#[derive(Debug)]
struct FileCopyExecution;

impl ProfileExecution for FileCopyExecution {
    fn into_plan(
        self: Box<Self>,
        _profile_name: &str,
        input: PathBuf,
        output: PathBuf,
        working_directory: PathBuf,
        budget: ResourceBudget,
    ) -> ResolvedExecutionPlan {
        let command = RawCommand {
            program: FILE_COPY_PROGRAM.to_owned(),
            arguments: vec![
                input.to_string_lossy().into_owned(),
                output.to_string_lossy().into_owned(),
            ],
            working_directory: working_directory.to_string_lossy().into_owned(),
            environment: BTreeMap::new(),
        };
        ResolvedExecutionPlan::from_validated_raw(&command, budget)
    }

    #[cfg(test)]
    fn kind(&self) -> ProfileExecutionKind {
        ProfileExecutionKind::FileCopy
    }
}
