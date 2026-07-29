//! TaskCage protocol의 bounded length-prefixed JSON frame codec이다.

use std::fmt;
use std::io;
use std::str::Utf8Error;

use serde::Serialize;
use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::MAX_FRAME_BYTES;

const LENGTH_PREFIX_BYTES: usize = 4;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame I/O가 실패했습니다")]
    Io(#[from] io::Error),
    #[error("frame payload 길이는 0일 수 없습니다")]
    ZeroLength,
    #[error("frame payload가 최대 크기를 넘었습니다: {length} > {max}")]
    TooLarge { length: usize, max: usize },
    #[error("frame payload가 UTF-8이 아닙니다")]
    InvalidUtf8(#[from] Utf8Error),
    #[error("frame payload가 유효한 JSON이 아닙니다")]
    InvalidJson(#[from] serde_json::Error),
    #[error("frame JSON 최상위 값은 object여야 합니다")]
    NonObject,
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    read_frame_or_eof(reader).await?.ok_or_else(|| {
        FrameError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "frame length prefix가 없습니다",
        ))
    })
}

/// 다음 frame을 읽되 prefix를 전혀 받기 전의 정상 연결 종료는 별도로 구분한다.
pub async fn read_frame_or_eof<R>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
    if reader.read(&mut prefix[..1]).await? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..]).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    validate_length(length)?;

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

pub async fn read_json_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let payload = read_frame(reader).await?;
    decode_json(&payload)
}

pub fn encode_json_frame<T>(message: &T) -> Result<Vec<u8>, FrameError>
where
    T: Serialize,
{
    let value = serde_json::to_value(message)?;
    if !value.is_object() {
        return Err(FrameError::NonObject);
    }
    let payload = serde_json::to_vec(&value)?;
    validate_length(payload.len())?;

    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub async fn write_json_frame<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let frame = encode_json_frame(message)?;
    writer.write_all(&frame).await?;
    Ok(())
}

pub fn decode_json<T>(payload: &[u8]) -> Result<T, FrameError>
where
    T: DeserializeOwned,
{
    validate_length(payload.len())?;
    let text = std::str::from_utf8(payload)?;
    let value = parse_without_duplicate_keys(text)?;
    if !value.is_object() {
        return Err(FrameError::NonObject);
    }
    Ok(serde_json::from_value(value)?)
}

fn validate_length(length: usize) -> Result<(), FrameError> {
    if length == 0 {
        return Err(FrameError::ZeroLength);
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            length,
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

fn parse_without_duplicate_keys(text: &str) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = UniqueValue.deserialize(&mut deserializer)?;
    deserializer.end()?;
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
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("JSON number must be finite"))
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
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncWriteExt, duplex};

    use super::*;

    #[tokio::test]
    async fn reads_fragmented_frame() {
        let message = json!({"protocolVersion": 1, "payload": {}});
        let frame = encode_json_frame(&message).unwrap();
        let (mut sender, mut receiver) = duplex(8);
        let writer = tokio::spawn(async move {
            for byte in frame {
                sender.write_all(&[byte]).await.unwrap();
            }
        });

        let decoded: Value = read_json_frame(&mut receiver).await.unwrap();
        writer.await.unwrap();
        assert_eq!(decoded, message);
    }

    #[tokio::test]
    async fn writes_complete_frame_across_partial_writes() {
        let message = json!({"protocolVersion": 1, "payload": {"value": "test"}});
        let expected = message.clone();
        let (mut sender, mut receiver) = duplex(1);
        let writer = tokio::spawn(async move {
            write_json_frame(&mut sender, &message).await.unwrap();
        });

        let decoded: Value = read_json_frame(&mut receiver).await.unwrap();
        writer.await.unwrap();
        assert_eq!(decoded, expected);
    }

    #[tokio::test]
    async fn reads_coalesced_frames_in_order() {
        let first = json!({"sequence": 1});
        let second = json!({"sequence": 2});
        let mut bytes = encode_json_frame(&first).unwrap();
        bytes.extend_from_slice(&encode_json_frame(&second).unwrap());
        let (mut sender, mut receiver) = duplex(bytes.len());
        sender.write_all(&bytes).await.unwrap();

        let decoded_first: Value = read_json_frame(&mut receiver).await.unwrap();
        let decoded_second: Value = read_json_frame(&mut receiver).await.unwrap();
        assert_eq!(decoded_first, first);
        assert_eq!(decoded_second, second);
    }

    #[tokio::test]
    async fn rejects_zero_and_oversized_lengths_before_payload_read() {
        let (mut sender, mut receiver) = duplex(8);
        sender.write_all(&0_u32.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_frame(&mut receiver).await,
            Err(FrameError::ZeroLength)
        ));

        let (mut sender, mut receiver) = duplex(8);
        sender
            .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            read_frame(&mut receiver).await,
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn distinguishes_clean_eof_from_an_incomplete_prefix() {
        let (sender, mut receiver) = duplex(8);
        drop(sender);
        assert_eq!(read_frame_or_eof(&mut receiver).await.unwrap(), None);

        let (mut sender, mut receiver) = duplex(8);
        sender.write_all(&[0, 0]).await.unwrap();
        drop(sender);
        assert!(matches!(
            read_frame_or_eof(&mut receiver).await,
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn rejects_invalid_utf8_json_and_non_object() {
        assert!(matches!(
            decode_json::<Value>(&[0xff]),
            Err(FrameError::InvalidUtf8(_))
        ));
        assert!(matches!(
            decode_json::<Value>(br#"{"#),
            Err(FrameError::InvalidJson(_))
        ));
        assert!(matches!(
            decode_json::<Value>(br#"[]"#),
            Err(FrameError::NonObject)
        ));
    }

    #[test]
    fn rejects_duplicate_keys_at_any_depth() {
        assert!(matches!(
            decode_json::<Value>(br#"{"payload":{"taskId":"a","taskId":"b"}}"#),
            Err(FrameError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_non_object_encoding() {
        assert!(matches!(
            encode_json_frame(&vec![1, 2, 3]),
            Err(FrameError::NonObject)
        ));
    }
}
