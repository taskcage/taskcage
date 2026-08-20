//! Backend-independent TaskCage execution contract.
//!
//! `taskcage-core` owns values that describe an immutable Capsule execution. The daemon and the
//! private embedded helper are adapters around the execution implementation that will be moved into
//! this crate incrementally. Transport, host admission policy and process supervision do not belong
//! in this crate.

use semver::Version;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleIdentity {
    name: String,
    version: String,
}

impl CapsuleIdentity {
    /// Creates an immutable Capsule identity using the public naming and version contract.
    pub fn new(name: impl Into<String>, version: impl AsRef<str>) -> Result<Self, IdentityError> {
        let name = name.into();
        let valid_name = (1..=63).contains(&name.len())
            && name.split('.').all(|segment| {
                let bytes = segment.as_bytes();
                !bytes.is_empty()
                    && bytes[0].is_ascii_lowercase()
                    && bytes[1..].iter().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-'
                    })
            });
        if !valid_name {
            return Err(IdentityError::InvalidName(name));
        }

        let version = Version::parse(version.as_ref())
            .map_err(|_| IdentityError::InvalidVersion(version.as_ref().to_owned()))?;
        if !version.pre.is_empty() || !version.build.is_empty() {
            return Err(IdentityError::PrereleaseOrBuildMetadata(
                version.to_string(),
            ));
        }

        Ok(Self {
            name,
            version: version.to_string(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("Capsule name is invalid: {0:?}")]
    InvalidName(String),
    #[error("Capsule version is invalid: {0:?}")]
    InvalidVersion(String),
    #[error("Capsule version cannot contain prerelease or build metadata: {0:?}")]
    PrereleaseOrBuildMetadata(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_capsule_identity() {
        let identity = CapsuleIdentity::new("ffmpeg-audio-to-wav", "1.0.0").unwrap();
        assert_eq!(identity.name(), "ffmpeg-audio-to-wav");
        assert_eq!(identity.version(), "1.0.0");
    }

    #[test]
    fn rejects_prerelease_and_invalid_names() {
        assert_eq!(
            CapsuleIdentity::new("ffmpeg", "1.0.0-alpha").unwrap_err(),
            IdentityError::PrereleaseOrBuildMetadata("1.0.0-alpha".to_owned())
        );
        assert!(matches!(
            CapsuleIdentity::new("FFmpeg", "1.0.0"),
            Err(IdentityError::InvalidName(_))
        ));
        assert!(CapsuleIdentity::new("media.extract-audio", "1.0.0").is_ok());
    }
}
