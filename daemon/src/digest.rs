//! SHA-256 identity를 manifest, Package payload와 Artifact가 같은 문법으로 공유한다.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// digest-addressed 경로에 바로 쓸 수 있는 prefix 없는 소문자 16진수다.
    pub fn hex(&self) -> String {
        let mut hexadecimal = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            write!(hexadecimal, "{byte:02x}").expect("writing to String cannot fail");
        }
        hexadecimal
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha256:")?;
        formatter.write_str(&self.hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("sha256: prefix 뒤에 소문자 16진수 64자가 필요합니다")]
pub struct DigestParseError;

impl FromStr for Sha256Digest {
    type Err = DigestParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hexadecimal = value.strip_prefix("sha256:").ok_or(DigestParseError)?;
        if hexadecimal.len() != 64
            || !hexadecimal
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DigestParseError);
        }

        let mut bytes = [0_u8; 32];
        for (index, chunk) in hexadecimal.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex_value(chunk[0]) << 4) | hex_value(chunk[1]);
        }
        Ok(Self(bytes))
    }
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("digest syntax is checked before decoding"),
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_only_canonical_sha256_identities() {
        let text = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let digest: Sha256Digest = text.parse().expect("canonical digest should parse");

        assert_eq!(digest.to_string(), text);
        assert_eq!(digest.hex(), &text["sha256:".len()..]);
        assert_eq!(digest.as_bytes().len(), 32);
        for invalid in [
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:ABCDEF",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg",
        ] {
            assert!(invalid.parse::<Sha256Digest>().is_err());
        }
    }
}
