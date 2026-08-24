//! Capsule resolve와 실행 계약 검증 use case다.

pub(crate) mod contract;
pub(crate) mod registry;
pub(crate) mod resolver;

#[cfg(test)]
pub(crate) use crate::adapters::outbound::capsule_resolvers::legacy::ffmpeg::{
    FFMPEG_PACKAGE_ENTRYPOINT, FFMPEG_PACKAGE_ID, FFMPEG_PROFILE_NAME, FFMPEG_PROFILE_VERSION,
};
#[cfg(test)]
pub(crate) use crate::adapters::outbound::capsule_resolvers::legacy::file_copy::{
    FILE_COPY_PROFILE_NAME, FILE_COPY_PROFILE_VERSION,
};
pub(crate) use registry::{ProfileError, ProfileRegistry, ResolvedProfile, StagedProfile};
