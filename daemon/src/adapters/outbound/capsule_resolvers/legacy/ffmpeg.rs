//! 기존 FFmpeg reference Capsule resolver다.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::application::{ProfileSubmission, UseCaseErrorCode};
use crate::artifact::DeclaredOutputArtifact;
use crate::digest::Sha256Digest;
use crate::execution_plan::ResolvedExecutionPlan;
use crate::resource_budget::ResourceBudget;
use crate::runtime_package::{ResolvedRuntimePackage, RuntimePackageCache};
use taskcage_core::capsule::{ProfileCall, ProfileValue};

use super::{input, parse_source, resolve_budget};
use crate::adapters::outbound::capsule_resolvers::ProfileStartupError;
use crate::application::capsule::registry::{ProfileError, ResolvedProfile};
#[cfg(test)]
use crate::application::capsule::resolver::ProfileExecutionKind;
use crate::application::capsule::resolver::{CapsuleResolution, CapsuleResolver, ProfileExecution};

pub(crate) const FFMPEG_PROFILE_NAME: &str = "ffmpeg-audio-to-wav";
pub(crate) const FFMPEG_PROFILE_VERSION: &str = "1.0.0";
pub(crate) const FFMPEG_PACKAGE_ID: &str = "org.taskcage.ffmpeg";
pub(crate) const FFMPEG_PACKAGE_ENTRYPOINT: &str = "bin/ffmpeg";
pub(crate) const FFMPEG_OUTPUT_SLOT: &str = "audio";
pub(crate) const FFMPEG_OUTPUT_FILE: &str = "result.wav";
pub(crate) const FFMPEG_OUTPUT_MEDIA_TYPE: &str = "audio/wav";
pub(crate) const FFMPEG_SAMPLE_RATES: &[i64] = &[8_000, 16_000, 22_050, 44_100, 48_000];
pub(crate) const FFMPEG_CHANNELS: &[i64] = &[1, 2];

#[derive(Debug)]
pub(crate) struct FfmpegResolver {
    maximum_artifact_bytes: u64,
    default_budget: ResourceBudget,
    cache: RuntimePackageCache,
    digest: Sha256Digest,
}

impl FfmpegResolver {
    pub(crate) fn open(
        cache_root: &Path,
        digest: Sha256Digest,
        maximum_artifact_bytes: u64,
        default_budget: ResourceBudget,
    ) -> Result<Self, ProfileStartupError> {
        let cache = RuntimePackageCache::open(cache_root)?;
        let package = cache.resolve(digest)?;
        validate_ffmpeg_package_contract(&package)?;
        Ok(Self {
            maximum_artifact_bytes,
            default_budget,
            cache,
            digest,
        })
    }
}

impl CapsuleResolver for FfmpegResolver {
    fn resolve(&self, request: &ProfileSubmission) -> Result<CapsuleResolution, ProfileError> {
        let call = request.call();
        if call.identity().name() != FFMPEG_PROFILE_NAME
            || call.identity().version() != FFMPEG_PROFILE_VERSION
        {
            return Ok(CapsuleResolution::NotFound);
        }
        let (source, sample_rate_hz, channels) = validate_ffmpeg_call(call)?;
        let budget = resolve_budget(&self.default_budget, call.resource_overrides())?;
        let package = self.cache.resolve(self.digest).map_err(|error| {
            ProfileError::new(
                UseCaseErrorCode::EnvironmentUnavailable,
                format!("registered FFmpeg Runtime Package is unavailable: {error}"),
            )
        })?;
        validate_ffmpeg_package_contract(&package).map_err(|error| {
            ProfileError::new(UseCaseErrorCode::EnvironmentUnavailable, error.to_string())
        })?;
        let entrypoint = package.entrypoint().try_clone().map_err(|error| {
            ProfileError::new(
                UseCaseErrorCode::EnvironmentUnavailable,
                format!("verified FFmpeg entrypoint descriptor could not be pinned: {error}"),
            )
        })?;
        let output = DeclaredOutputArtifact::new(
            FFMPEG_OUTPUT_FILE,
            FFMPEG_OUTPUT_MEDIA_TYPE,
            self.maximum_artifact_bytes,
        )
        .expect("static FFmpeg output contract must be valid");
        Ok(CapsuleResolution::Resolved(Box::new(ResolvedProfile::new(
            request.clone(),
            source,
            output,
            budget,
            Box::new(FfmpegExecution {
                entrypoint,
                sample_rate_hz,
                channels,
            }),
            FFMPEG_OUTPUT_SLOT.to_owned(),
        ))))
    }
}

#[derive(Debug)]
struct FfmpegExecution {
    entrypoint: File,
    sample_rate_hz: i64,
    channels: i64,
}

impl ProfileExecution for FfmpegExecution {
    fn into_plan(
        self: Box<Self>,
        _profile_name: &str,
        input: PathBuf,
        output: PathBuf,
        working_directory: PathBuf,
        budget: ResourceBudget,
    ) -> ResolvedExecutionPlan {
        ResolvedExecutionPlan::from_pinned_entrypoint(
            self.entrypoint,
            OsString::from("ffmpeg"),
            ffmpeg_arguments(&input, self.sample_rate_hz, self.channels, &output),
            working_directory,
            BTreeMap::new(),
            budget,
        )
    }

    #[cfg(test)]
    fn kind(&self) -> ProfileExecutionKind {
        ProfileExecutionKind::LegacyFfmpeg
    }
}

fn validate_ffmpeg_call(
    call: &ProfileCall,
) -> Result<(crate::artifact::LocalInputArtifact, i64, i64), ProfileError> {
    if call.inputs().len() != 3 {
        return Err(ProfileError::new(
            UseCaseErrorCode::InvalidProfileInput,
            "ffmpeg-audio-to-wav requires exactly source, sample_rate_hz, and channels inputs",
        ));
    }
    let source = parse_source(call)?;
    let sample_rate_hz = allowed_int64(
        call,
        "sample_rate_hz",
        FFMPEG_SAMPLE_RATES,
        "8000, 16000, 22050, 44100, or 48000",
    )?;
    let channels = allowed_int64(call, "channels", FFMPEG_CHANNELS, "1 or 2")?;
    Ok((source, sample_rate_hz, channels))
}

fn allowed_int64(
    call: &ProfileCall,
    slot: &'static str,
    allowed: &[i64],
    allowed_description: &'static str,
) -> Result<i64, ProfileError> {
    match input(call, slot) {
        Some(ProfileValue::Int64(value)) if allowed.contains(value) => Ok(*value),
        Some(ProfileValue::Int64(_)) => Err(ProfileError::new(
            UseCaseErrorCode::InvalidProfileInput,
            format!("inputs.{slot} must be {allowed_description}"),
        )),
        _ => Err(ProfileError::new(
            UseCaseErrorCode::InvalidProfileInput,
            format!("inputs.{slot} must be INT64"),
        )),
    }
}

#[cfg(test)]
pub(crate) fn validate_ffmpeg_inputs(
    request: &crate::protocol::ProfileRequestPayload,
) -> Result<(crate::artifact::LocalInputArtifact, i64, i64), ProfileError> {
    let call = crate::protocol_mapper::profile_call(request);
    validate_ffmpeg_call(&call)
}

fn validate_ffmpeg_package_contract(
    package: &ResolvedRuntimePackage,
) -> Result<(), ProfileStartupError> {
    let manifest = package.manifest();
    if manifest.id != FFMPEG_PACKAGE_ID {
        return Err(ProfileStartupError::FfmpegPackageContract(format!(
            "id must be {FFMPEG_PACKAGE_ID}, actual={}",
            manifest.id
        )));
    }
    if manifest.entrypoint != FFMPEG_PACKAGE_ENTRYPOINT {
        return Err(ProfileStartupError::FfmpegPackageContract(format!(
            "entrypoint must be {FFMPEG_PACKAGE_ENTRYPOINT}, actual={}",
            manifest.entrypoint
        )));
    }
    Ok(())
}

pub(crate) fn ffmpeg_arguments(
    input: &Path,
    sample_rate_hz: i64,
    channels: i64,
    output: &Path,
) -> Vec<OsString> {
    [
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-nostdin"),
        OsString::from("-i"),
        input.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:a:0"),
        OsString::from("-vn"),
        OsString::from("-c:a"),
        OsString::from("pcm_s16le"),
        OsString::from("-ar"),
        OsString::from(sample_rate_hz.to_string()),
        OsString::from("-ac"),
        OsString::from(channels.to_string()),
        output.as_os_str().to_owned(),
    ]
    .into_iter()
    .collect()
}
