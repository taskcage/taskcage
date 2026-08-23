//! Backend-independent Artifact identity and descriptor values.

mod digest;

use std::fmt;

use thiserror::Error;

pub use digest::{DigestParseError, Sha256Digest};

pub const MAX_ARTIFACT_PATH_BYTES: usize = 4_096;

/// Artifact root 밖을 가리키지 않는 local adapter path다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactPath(String);

impl ArtifactPath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ArtifactPathError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_ARTIFACT_PATH_BYTES {
            return Err(ArtifactPathError::Length);
        }
        for segment in value.split('/') {
            if segment.is_empty() || matches!(segment, "." | "..") {
                return Err(ArtifactPathError::Segment);
            }
            if segment
                .bytes()
                .any(|byte| byte == b'\0' || byte == b'\\' || byte.is_ascii_control())
            {
                return Err(ArtifactPathError::UnsafeCharacter);
            }
        }
        if value.split('/').next() == Some(".taskcage") {
            return Err(ArtifactPathError::ReservedPath);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactPathError {
    #[error("artifact path는 1~4096 UTF-8 bytes여야 합니다")]
    Length,
    #[error("artifact path에 빈, . 또는 .. segment를 넣을 수 없습니다")]
    Segment,
    #[error("artifact path에 NUL, backslash 또는 ASCII control 문자를 넣을 수 없습니다")]
    UnsafeCharacter,
    #[error(".taskcage staging subtree는 caller Artifact path로 사용할 수 없습니다")]
    ReservedPath,
}

/// Transport와 저장 위치에 독립적인 immutable Artifact metadata다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    digest: Sha256Digest,
    size_bytes: u64,
    media_type: Option<String>,
}

impl ArtifactDescriptor {
    pub fn new(digest: Sha256Digest, size_bytes: u64, media_type: Option<String>) -> Self {
        Self {
            digest,
            size_bytes,
            media_type,
        }
    }

    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }
}

/// Local adapter가 해석할 source reference와 공통 descriptor를 묶은 input이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInputArtifact {
    path: ArtifactPath,
    descriptor: ArtifactDescriptor,
}

impl LocalInputArtifact {
    pub fn new(path: ArtifactPath, digest: Sha256Digest, size_bytes: u64) -> Self {
        Self {
            path,
            descriptor: ArtifactDescriptor::new(digest, size_bytes, None),
        }
    }

    pub fn path(&self) -> &ArtifactPath {
        &self.path
    }

    pub fn descriptor(&self) -> &ArtifactDescriptor {
        &self.descriptor
    }

    pub fn digest(&self) -> Sha256Digest {
        self.descriptor.digest()
    }

    pub fn size_bytes(&self) -> u64 {
        self.descriptor.size_bytes()
    }
}

/// Profile이 선언한 immutable output file 계약이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOutputArtifact {
    file_name: String,
    media_type: String,
    maximum_bytes: u64,
}

impl DeclaredOutputArtifact {
    pub fn new(
        file_name: impl Into<String>,
        media_type: impl Into<String>,
        maximum_bytes: u64,
    ) -> Result<Self, DeclaredOutputError> {
        let file_name = file_name.into();
        if file_name.is_empty()
            || file_name.contains('/')
            || file_name.contains('\\')
            || file_name == "."
            || file_name == ".."
            || file_name
                .bytes()
                .any(|byte| byte == b'\0' || byte.is_ascii_control())
        {
            return Err(DeclaredOutputError::FileName);
        }
        let media_type = media_type.into();
        if media_type.is_empty()
            || media_type.len() > 255
            || media_type
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b'\0')
        {
            return Err(DeclaredOutputError::MediaType);
        }
        if maximum_bytes == 0 {
            return Err(DeclaredOutputError::MaximumBytes);
        }
        Ok(Self {
            file_name,
            media_type,
            maximum_bytes,
        })
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn maximum_bytes(&self) -> u64 {
        self.maximum_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeclaredOutputError {
    #[error("output file name은 단일 안전 file name이어야 합니다")]
    FileName,
    #[error("output media type은 비어 있지 않은 control-character 없는 문자열이어야 합니다")]
    MediaType,
    #[error("output maximum bytes는 0보다 커야 합니다")]
    MaximumBytes,
}

/// 성공한 Task가 backend storage에 공개한 immutable Artifact descriptor다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedArtifact {
    path: ArtifactPath,
    descriptor: ArtifactDescriptor,
}

impl PublishedArtifact {
    pub fn new(
        path: ArtifactPath,
        digest: Sha256Digest,
        size_bytes: u64,
        media_type: impl Into<String>,
    ) -> Self {
        Self {
            path,
            descriptor: ArtifactDescriptor::new(digest, size_bytes, Some(media_type.into())),
        }
    }

    pub fn path(&self) -> &ArtifactPath {
        &self.path
    }

    pub fn descriptor(&self) -> &ArtifactDescriptor {
        &self.descriptor
    }

    pub fn digest(&self) -> Sha256Digest {
        self.descriptor.digest()
    }

    pub fn size_bytes(&self) -> u64 {
        self.descriptor.size_bytes()
    }

    pub fn media_type(&self) -> &str {
        self.descriptor
            .media_type()
            .expect("published Artifact always has a media type")
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn descriptor_keeps_content_identity_separate_from_local_reference() {
        let digest = Sha256Digest::from_str(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let input = LocalInputArtifact::new(
            ArtifactPath::parse("jobs/42/source.mov").unwrap(),
            digest,
            1_024,
        );

        assert_eq!(input.path().as_str(), "jobs/42/source.mov");
        assert_eq!(input.descriptor().digest(), digest);
        assert_eq!(input.descriptor().size_bytes(), 1_024);
        assert_eq!(input.descriptor().media_type(), None);
    }

    #[test]
    fn path_validation_rejects_escape_and_reserved_staging_paths() {
        assert!(matches!(
            ArtifactPath::parse("../source.mov"),
            Err(ArtifactPathError::Segment)
        ));
        assert!(matches!(
            ArtifactPath::parse(".taskcage/staging/source.mov"),
            Err(ArtifactPathError::ReservedPath)
        ));
        assert!(matches!(
            ArtifactPath::parse("jobs\\source.mov"),
            Err(ArtifactPathError::UnsafeCharacter)
        ));
    }

    #[test]
    fn declared_output_requires_one_safe_bounded_file() {
        let output = DeclaredOutputArtifact::new("result.wav", "audio/wav", 1_048_576).unwrap();
        assert_eq!(output.file_name(), "result.wav");
        assert_eq!(output.media_type(), "audio/wav");
        assert_eq!(output.maximum_bytes(), 1_048_576);
        assert_eq!(
            DeclaredOutputArtifact::new("../result.wav", "audio/wav", 1),
            Err(DeclaredOutputError::FileName)
        );
        assert_eq!(
            DeclaredOutputArtifact::new("result.wav", "audio/wav", 0),
            Err(DeclaredOutputError::MaximumBytes)
        );
    }
}
