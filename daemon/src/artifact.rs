//! Local Product Alpha Artifact descriptor의 공통 검증값이다.
//!
//! 실제 root-relative open, snapshot과 publish는 Profile 실행 경로가 추가될 때 이 값만 소비한다.
//! Raw Command Protocol v1은 이 모듈을 사용하지 않는다.

use std::fmt;

use thiserror::Error;

use crate::digest::Sha256Digest;

/// Artifact root 기준 상대 path의 최대 UTF-8 byte 길이다.
pub const MAX_ARTIFACT_PATH_BYTES: usize = 4_096;

/// Artifact root 밖을 가리키지 않는 wire path다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactPath(String);

impl ArtifactPath {
    /// Local Artifact path 문법을 side effect 없이 검증한다.
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

/// Input Artifact가 선언하는 immutable snapshot identity다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInputArtifact {
    path: ArtifactPath,
    digest: Sha256Digest,
    size_bytes: u64,
}

impl LocalInputArtifact {
    pub fn new(path: ArtifactPath, digest: Sha256Digest, size_bytes: u64) -> Self {
        Self {
            path,
            digest,
            size_bytes,
        }
    }

    pub fn path(&self) -> &ArtifactPath {
        &self.path
    }

    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
}

/// Artifact path가 Local Product Alpha 경계를 벗어났다.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn parse_descriptor(value: &Value) -> LocalInputArtifact {
        let object = value
            .as_object()
            .expect("Artifact descriptor는 JSON object여야 합니다");
        assert_eq!(
            object.get("kind").and_then(Value::as_str),
            Some("LOCAL_INPUT")
        );
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .expect("path가 필요합니다");
        let digest = object
            .get("digest")
            .and_then(Value::as_str)
            .expect("digest가 필요합니다");
        let size_bytes = object
            .get("sizeBytes")
            .and_then(Value::as_u64)
            .expect("sizeBytes는 unsigned JSON integer여야 합니다");

        LocalInputArtifact::new(
            ArtifactPath::parse(path).expect("Artifact path가 유효해야 합니다"),
            digest
                .parse()
                .expect("digest가 canonical SHA-256이어야 합니다"),
            size_bytes,
        )
    }

    #[test]
    fn accepts_a_canonical_root_relative_input_path() {
        let path = ArtifactPath::parse("jobs/42/source.mov").expect("valid Artifact path");

        assert_eq!(path.as_str(), "jobs/42/source.mov");
    }

    #[test]
    fn keeps_unicode_and_percent_sequences_as_literal_path_bytes() {
        let path = ArtifactPath::parse("입력/%2e%2e/source.mov").expect("valid literal path");

        assert_eq!(path.to_string(), "입력/%2e%2e/source.mov");
    }

    #[test]
    fn rejects_traversal_reserved_and_ambiguous_paths() {
        for path in [
            "",
            "/absolute/file",
            "jobs/../source.mov",
            "jobs//source.mov",
            "jobs/./source.mov",
            ".taskcage/staging/input",
            "jobs\\source.mov",
            "jobs/line\\nbreak",
        ] {
            assert!(ArtifactPath::parse(path).is_err(), "path={path:?}");
        }
    }

    #[test]
    fn binds_path_digest_and_size_without_a_mutable_source_reference() {
        let artifact = LocalInputArtifact::new(
            ArtifactPath::parse("jobs/42/source.mov").unwrap(),
            DIGEST.parse().unwrap(),
            1_048_576,
        );

        assert_eq!(artifact.path().as_str(), "jobs/42/source.mov");
        assert_eq!(artifact.digest().to_string(), DIGEST);
        assert_eq!(artifact.size_bytes(), 1_048_576);
    }

    #[test]
    fn valid_fixture_is_a_canonical_local_input_descriptor() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-input-valid.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");

        let artifact = parse_descriptor(&fixture);

        assert_eq!(artifact.path().as_str(), "jobs/42/source.mov");
        assert_eq!(artifact.digest().to_string(), DIGEST);
        assert_eq!(artifact.size_bytes(), 1_048_576);
    }

    #[test]
    fn rejection_fixtures_require_target_not_to_start() {
        let invalid_path: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-input-invalid-path.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");
        let mismatch: Value = serde_json::from_str(include_str!(
            "../../protocol-fixtures/v2/artifact-input-digest-mismatch.json"
        ))
        .expect("fixture JSON이 유효해야 합니다");

        let descriptor = invalid_path
            .get("descriptor")
            .expect("invalid path fixture에는 descriptor가 필요합니다");
        let path = descriptor
            .get("path")
            .and_then(Value::as_str)
            .expect("path가 필요합니다");
        assert!(ArtifactPath::parse(path).is_err());
        assert_eq!(
            invalid_path.get("expectedError").and_then(Value::as_str),
            Some("INVALID_ARTIFACT_PATH")
        );
        assert_eq!(
            invalid_path.get("targetMustStart").and_then(Value::as_bool),
            Some(false)
        );

        let descriptor = mismatch
            .get("descriptor")
            .expect("digest mismatch fixture에는 descriptor가 필요합니다");
        let artifact = parse_descriptor(descriptor);
        assert_eq!(artifact.path().as_str(), "jobs/42/source.mov");
        assert_eq!(
            mismatch.get("expectedError").and_then(Value::as_str),
            Some("ARTIFACT_DIGEST_MISMATCH")
        );
        assert_eq!(
            mismatch.get("targetMustStart").and_then(Value::as_bool),
            Some(false)
        );
    }
}
