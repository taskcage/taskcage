//! Reference와 legacy resolver를 일반 installed Capsule 경로와 분리한다.

pub(super) mod ffmpeg;
pub(super) mod file_copy;

use std::str::FromStr;

use crate::artifact::{ArtifactPath, LocalInputArtifact};
use crate::digest::Sha256Digest;
use crate::protocol::{
    ErrorCode, OutputLimits, ProfileInputValue, ProfileRequestPayload, ProfileResourceOverrides,
    ResourceLimits,
};
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};

pub(crate) use ffmpeg::{FfmpegResolver, ffmpeg_arguments};
pub(crate) use file_copy::FileCopyResolver;

use super::registry::ProfileError;

fn parse_source(request: &ProfileRequestPayload) -> Result<LocalInputArtifact, ProfileError> {
    match request.inputs.get("source") {
        Some(ProfileInputValue::LocalInput {
            path,
            digest,
            size_bytes,
        }) => {
            let path = ArtifactPath::parse(path.clone()).map_err(|error| {
                ProfileError::new(ErrorCode::InvalidArtifactPath, error.to_string())
            })?;
            let digest = Sha256Digest::from_str(digest).map_err(|error| {
                ProfileError::new(ErrorCode::InvalidProfileInput, error.to_string())
            })?;
            Ok(LocalInputArtifact::new(path, digest, *size_bytes))
        }
        Some(_) => Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "inputs.source must be LOCAL_INPUT",
        )),
        None => Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "inputs.source is required",
        )),
    }
}

fn resolve_budget(
    default: &ResourceBudget,
    overrides: Option<&ProfileResourceOverrides>,
) -> Result<ResourceBudget, ProfileError> {
    let Some(overrides) = overrides else {
        return Ok(default.clone());
    };
    let mut limits: ResourceLimits = default.protocol_limits();
    let mut output: OutputLimits = default.protocol_output();
    let mut has_value = false;
    if let Some(partial) = &overrides.limits {
        if let Some(cpu_max) = &partial.cpu_max {
            limits.cpu_max = cpu_max.clone();
            has_value = true;
        }
        if let Some(value) = partial.memory_max_bytes {
            limits.memory_max_bytes = value;
            has_value = true;
        }
        if let Some(value) = partial.pids_max {
            limits.pids_max = value;
            has_value = true;
        }
        if let Some(value) = partial.wall_time_limit_ms {
            limits.wall_time_limit_ms = value;
            has_value = true;
        }
    }
    if let Some(partial) = &overrides.output {
        if let Some(value) = partial.stdout_tail_max_bytes {
            output.stdout_tail_max_bytes = value;
            has_value = true;
        }
        if let Some(value) = partial.stderr_tail_max_bytes {
            output.stderr_tail_max_bytes = value;
            has_value = true;
        }
    }
    if !has_value {
        return Err(ProfileError::new(
            ErrorCode::InvalidProfileInput,
            "resourceOverrides must contain at least one nested field",
        ));
    }
    ResourceBudget::try_from_protocol(limits, output).map_err(profile_input_budget_error)
}

fn profile_input_budget_error(error: ResourceBudgetError) -> ProfileError {
    ProfileError::new(ErrorCode::InvalidProfileInput, error.to_string())
}
