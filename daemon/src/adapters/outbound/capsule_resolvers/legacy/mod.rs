//! Reference와 legacy resolver를 일반 installed Capsule 경로와 분리한다.

pub(crate) mod ffmpeg;
pub(crate) mod file_copy;

use std::str::FromStr;

use crate::application::UseCaseErrorCode;
use crate::artifact::{ArtifactPath, LocalInputArtifact};
use crate::digest::Sha256Digest;
use crate::resource_budget::{ResourceBudget, ResourceBudgetError};
use taskcage_core::capsule::{ProfileCall, ProfileResourceOverrides, ProfileValue};

pub(crate) use ffmpeg::FfmpegResolver;
#[cfg(test)]
pub(crate) use ffmpeg::ffmpeg_arguments;
pub(crate) use file_copy::FileCopyResolver;

use crate::application::capsule::registry::ProfileError;

fn input<'a>(call: &'a ProfileCall, name: &str) -> Option<&'a ProfileValue> {
    call.inputs()
        .find_map(|(slot, value)| (slot == name).then_some(value))
}

fn parse_source(call: &ProfileCall) -> Result<LocalInputArtifact, ProfileError> {
    match input(call, "source") {
        Some(ProfileValue::LocalInput {
            path,
            digest,
            size_bytes,
        }) => {
            let path = ArtifactPath::parse(path.clone()).map_err(|error| {
                ProfileError::new(UseCaseErrorCode::InvalidArtifactPath, error.to_string())
            })?;
            let digest = Sha256Digest::from_str(digest).map_err(|error| {
                ProfileError::new(UseCaseErrorCode::InvalidProfileInput, error.to_string())
            })?;
            Ok(LocalInputArtifact::new(path, digest, *size_bytes))
        }
        Some(_) => Err(ProfileError::new(
            UseCaseErrorCode::InvalidProfileInput,
            "inputs.source must be LOCAL_INPUT",
        )),
        None => Err(ProfileError::new(
            UseCaseErrorCode::InvalidProfileInput,
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
    if overrides.is_empty() {
        return Err(ProfileError::new(
            UseCaseErrorCode::InvalidProfileInput,
            "resourceOverrides must contain at least one nested field",
        ));
    }
    let current = default.as_core();
    let resources = current.resources();
    let output = current.output();
    let cpu = overrides.cpu_max();
    ResourceBudget::try_new(
        cpu.map_or(resources.cpu().quota_micros().get(), |value| {
            value.quota_micros()
        }),
        cpu.map_or(resources.cpu().period_micros().get(), |value| {
            value.period_micros()
        }),
        overrides
            .memory_max_bytes()
            .unwrap_or(resources.memory_max_bytes().get()),
        overrides.pids_max().unwrap_or(resources.pids_max().get()),
        overrides
            .wall_time_limit_ms()
            .unwrap_or(resources.wall_time_limit_ms().get()),
        overrides
            .stdout_tail_max_bytes()
            .unwrap_or(output.stdout_tail_max_bytes().get() as u32),
        overrides
            .stderr_tail_max_bytes()
            .unwrap_or(output.stderr_tail_max_bytes().get() as u32),
    )
    .map_err(profile_input_budget_error)
}

fn profile_input_budget_error(error: ResourceBudgetError) -> ProfileError {
    ProfileError::new(UseCaseErrorCode::InvalidProfileInput, error.to_string())
}
