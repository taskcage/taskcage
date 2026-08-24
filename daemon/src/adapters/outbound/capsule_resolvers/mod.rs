//! Capsule catalog, reference resolver, Runtime Package cache 조립 adapter다.

use std::path::Path;

use thiserror::Error;

use crate::application::capsule::resolver::CapsuleResolver;
use crate::artifact::ArtifactStoreError;
use crate::bundle::BundleError;
use crate::digest::Sha256Digest;
use crate::resource_budget::ResourceBudget;
use crate::runtime_package::RuntimePackageError;

pub(crate) mod installed;
pub(crate) mod legacy;

use installed::InstalledCapsuleResolver;
use legacy::{FfmpegResolver, FileCopyResolver};

#[derive(Debug, Error)]
pub(crate) enum ProfileStartupError {
    #[error(transparent)]
    Artifact(#[from] ArtifactStoreError),
    #[error(transparent)]
    RuntimePackage(#[from] RuntimePackageError),
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("FFmpeg Runtime Package 계약이 잘못되었습니다: {0}")]
    FfmpegPackageContract(String),
}

pub(crate) fn open_resolvers(
    maximum_artifact_bytes: u64,
    default_budget: ResourceBudget,
    ffmpeg_registration: Option<(&Path, Sha256Digest)>,
    bundle_cache_root: Option<&Path>,
) -> Result<Vec<Box<dyn CapsuleResolver>>, ProfileStartupError> {
    let file_copy = FileCopyResolver::open(maximum_artifact_bytes, default_budget.clone())?;
    let ffmpeg = ffmpeg_registration
        .map(|(cache_root, digest)| {
            FfmpegResolver::open(
                cache_root,
                digest,
                maximum_artifact_bytes,
                default_budget.clone(),
            )
        })
        .transpose()?;
    let bundles = bundle_cache_root
        .map(|cache_root| InstalledCapsuleResolver::open(cache_root, maximum_artifact_bytes))
        .transpose()?;
    let mut resolvers: Vec<Box<dyn CapsuleResolver>> = Vec::with_capacity(3);
    if let Some(bundles) = bundles {
        resolvers.push(Box::new(bundles));
    }
    resolvers.push(Box::new(file_copy));
    if let Some(ffmpeg) = ffmpeg {
        resolvers.push(Box::new(ffmpeg));
    }
    Ok(resolvers)
}
