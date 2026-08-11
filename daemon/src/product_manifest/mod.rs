//! Product Alpha manifest를 side effect 없이 검증하고 RFC 8785 digest를 계산한다.
//!
//! RFC 8785는 object key 정렬뿐 아니라 ECMAScript 문자열과 숫자 직렬화 규칙도 고정한다.
//! 이 모듈은 그 부분만 `serde_json_canonicalizer`에 맡기고, TaskCage schema와 중복 key,
//! 정수 범위 검증은 canonicalization 전에 직접 수행한다.

mod model;
mod validate;

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::digest::Sha256Digest;

pub use model::*;

use validate::{validate_bundle, validate_execution_profile, validate_runtime_package};

/// Product Alpha manifest 하나의 최대 JSON 크기다.
pub const MAX_MANIFEST_BYTES: usize = 1_048_576;

// RFC 8785 input은 I-JSON이고, 정수 의미가 반올림 없이 언어 사이에서 유지되는 범위다.
const MAX_EXACT_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const MIN_EXACT_JSON_INTEGER: i64 = -9_007_199_254_740_991;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest JSON은 비어 있을 수 없습니다")]
    Empty,
    #[error("manifest가 {actual} bytes여서 상한 {maximum} bytes를 넘었습니다")]
    TooLarge { actual: usize, maximum: usize },
    #[error("manifest JSON이 잘못되었습니다: {0}")]
    Json(String),
    #[error("manifest field {field}가 잘못되었습니다: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error("manifest를 RFC 8785 JSON으로 canonicalize하지 못했습니다: {0}")]
    Canonicalization(String),
}

impl ManifestError {
    pub(crate) fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidatedManifest<T> {
    manifest: T,
    digest: Sha256Digest,
    canonical_json: Vec<u8>,
}

impl<T> ValidatedManifest<T> {
    pub fn manifest(&self) -> &T {
        &self.manifest
    }

    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical_json
    }

    pub fn into_manifest(self) -> T {
        self.manifest
    }
}

pub type ValidatedExecutionProfile = ValidatedManifest<ExecutionProfileManifest>;
pub type ValidatedRuntimePackage = ValidatedManifest<RuntimePackageManifest>;
pub type ValidatedBundle = ValidatedManifest<BundleManifest>;

pub fn parse_execution_profile(source: &[u8]) -> Result<ValidatedExecutionProfile, ManifestError> {
    let value = parse_strict_json(source)?;
    let manifest: ExecutionProfileManifest = deserialize_manifest(value.clone())?;
    validate_execution_profile(&manifest)?;
    finish_manifest(value, manifest)
}

pub fn parse_runtime_package(source: &[u8]) -> Result<ValidatedRuntimePackage, ManifestError> {
    let value = parse_strict_json(source)?;
    let manifest: RuntimePackageManifest = deserialize_manifest(value.clone())?;
    validate_runtime_package(&manifest)?;
    finish_manifest(value, manifest)
}

/// Bundle의 embedded Profile과 이미 검증된 Runtime Package 참조를 모두 닫아 검증한다.
pub fn parse_bundle(
    source: &[u8],
    runtime_package: &ValidatedRuntimePackage,
) -> Result<ValidatedBundle, ManifestError> {
    let value = parse_strict_json(source)?;
    let profile_value = value
        .as_object()
        .and_then(|object| object.get("profile"))
        .cloned()
        .ok_or_else(|| ManifestError::invalid("profile", "embedded Profile이 필요합니다"))?;
    let profile_digest = digest_value(&profile_value)?.1;
    let manifest: BundleManifest = deserialize_manifest(value.clone())?;
    validate_execution_profile(&manifest.profile)?;
    validate_bundle(&manifest, profile_digest, runtime_package)?;
    finish_manifest(value, manifest)
}

fn deserialize_manifest<T>(value: Value) -> Result<T, ManifestError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(value).map_err(|error| ManifestError::Json(error.to_string()))
}

fn finish_manifest<T>(value: Value, manifest: T) -> Result<ValidatedManifest<T>, ManifestError> {
    let (canonical_json, digest) = digest_value(&value)?;
    Ok(ValidatedManifest {
        manifest,
        digest,
        canonical_json,
    })
}

fn digest_value(value: &Value) -> Result<(Vec<u8>, Sha256Digest), ManifestError> {
    // 전용 crate를 사용해 Rust와 Java 구현이 따라야 할 RFC 8785 bytes를 정확히 고정한다.
    let canonical_json = serde_json_canonicalizer::to_vec(value)
        .map_err(|error| ManifestError::Canonicalization(error.to_string()))?;
    let hash: [u8; 32] = Sha256::digest(&canonical_json).into();
    Ok((canonical_json, Sha256Digest::from_bytes(hash)))
}

fn parse_strict_json(source: &[u8]) -> Result<Value, ManifestError> {
    if source.is_empty() {
        return Err(ManifestError::Empty);
    }
    if source.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge {
            actual: source.len(),
            maximum: MAX_MANIFEST_BYTES,
        });
    }

    let mut deserializer = serde_json::Deserializer::from_slice(source);
    let value = UniqueValue
        .deserialize(&mut deserializer)
        .map_err(|error| ManifestError::Json(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| ManifestError::Json(error.to_string()))?;
    if !value.is_object() {
        return Err(ManifestError::Json(
            "manifest top-level JSON은 object여야 합니다".to_owned(),
        ));
    }
    Ok(value)
}

struct UniqueValue;

impl<'de> DeserializeSeed<'de> for UniqueValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Product Alpha JSON value without duplicate keys or floats")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value < MIN_EXACT_JSON_INTEGER {
            return Err(E::custom("JSON integer is outside the exact I-JSON range"));
        }
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value > MAX_EXACT_JSON_INTEGER {
            return Err(E::custom("JSON integer is outside the exact I-JSON range"));
        }
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom(
            "floating-point and exponent JSON numbers are unsupported",
        ))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        UniqueValue.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(UniqueValue)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format_args!(
                    "duplicate JSON object key: {key}"
                )));
            }
            let value = object.next_value_seed(UniqueValue)?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
