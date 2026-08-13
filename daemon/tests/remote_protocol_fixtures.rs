use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use taskcaged::codec::{decode_json, encode_json_frame};
use taskcaged::remote_protocol::{
    AuthenticatePayload, REMOTE_MAX_FRAME_BYTES, RemoteRequest, RemoteResponse,
    RequestValidationError,
};

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../protocol-fixtures/remote-v1")
}

#[test]
fn authentication_debug_output_redacts_the_secret() {
    let payload = AuthenticatePayload {
        client_id: "document-worker".to_owned(),
        secret: "must-never-appear-in-logs".to_owned(),
    };
    let debug = format!("{payload:?}");
    assert!(debug.contains("document-worker"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("must-never-appear-in-logs"));
}

fn fixture_names() -> Vec<String> {
    let mut names = fs::read_dir(fixture_directory())
        .expect("Remote fixture 디렉터리를 읽어야 합니다")
        .filter_map(|entry| {
            let entry = entry.expect("Remote fixture 항목을 읽어야 합니다");
            let path = entry.path();
            (path.extension().and_then(|value| value.to_str()) == Some("json"))
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    fs::read(fixture_directory().join(name)).expect("Remote fixture를 읽어야 합니다")
}

fn assert_round_trip<T>(name: &str, value: &T)
where
    T: Serialize,
{
    let original: Value = serde_json::from_slice(&fixture_bytes(name)).expect("fixture JSON");
    let encoded = serde_json::to_value(value).expect("wire 값을 직렬화해야 합니다");
    assert_eq!(encoded, original, "{name} semantic round trip");

    let frame = encode_json_frame(value).expect("wire frame을 만들 수 있어야 합니다");
    let declared = u32::from_be_bytes(frame[..4].try_into().expect("frame prefix")) as usize;
    assert_eq!(declared, frame.len() - 4, "{name} frame length");
    assert!(declared <= REMOTE_MAX_FRAME_BYTES, "{name} frame bound");
}

#[test]
fn approved_remote_fixture_corpus_round_trips() {
    let request_types = [
        "abortArtifactUpload",
        "authenticate",
        "beginArtifactUpload",
        "cancelTask",
        "completeArtifactUpload",
        "getCapabilities",
        "getProfileResult",
        "readArtifactChunk",
        "submitProfile",
        "uploadArtifactChunk",
    ];

    let names = fixture_names();
    assert_eq!(names.len(), 43, "승인된 Remote fixture 수가 바뀌었습니다");
    for name in names {
        let bytes = fixture_bytes(&name);
        let raw: Value = decode_json(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        let message_type = raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{name}: type이 필요합니다"));
        if request_types.contains(&message_type) {
            let request: RemoteRequest = serde_json::from_value(raw)
                .unwrap_or_else(|error| panic!("{name}: request decode: {error}"));
            assert_eq!(request.validate_envelope(), Ok(()), "{name} envelope");
            assert_round_trip(&name, &request);
        } else {
            let response: RemoteResponse = serde_json::from_value(raw)
                .unwrap_or_else(|error| panic!("{name}: response decode: {error}"));
            assert_round_trip(&name, &response);
        }
    }
}

#[test]
fn remote_request_rejects_local_input_and_unknown_fields() {
    let local_input = br#"{
        "remoteProtocolVersion":1,
        "requestId":"33333333-3333-4333-8333-333333333333",
        "type":"submitProfile",
        "payload":{
            "clientRequestId":"44444444-4444-4444-8444-444444444444",
            "profile":{"name":"file-copy","version":"1.0.0"},
            "inputs":{"source":{"kind":"LOCAL_INPUT","path":"/tmp/a"}},
            "unexpected":true
        }
    }"#;
    assert!(serde_json::from_slice::<RemoteRequest>(local_input).is_err());
}

#[test]
fn remote_envelope_validation_is_separate_from_local_protocol() {
    let fixture = fixture_bytes("get-capabilities.json");
    let mut request: Value = serde_json::from_slice(&fixture).expect("fixture JSON");
    request["remoteProtocolVersion"] = Value::from(2);
    let request: RemoteRequest = serde_json::from_value(request).expect("typed request");
    assert_eq!(
        request.validate_envelope(),
        Err(RequestValidationError::UnsupportedVersion(2))
    );

    let mut request: Value = serde_json::from_slice(&fixture).expect("fixture JSON");
    request["requestId"] = Value::from("not-a-uuid");
    let request: RemoteRequest = serde_json::from_value(request).expect("typed request");
    assert_eq!(
        request.validate_envelope(),
        Err(RequestValidationError::InvalidRequestId)
    );

    let mut request: Value =
        serde_json::from_slice(&fixture_bytes("get-profile-result.json")).expect("fixture JSON");
    request["payload"]["taskId"] = Value::from("not-a-uuid");
    let request: RemoteRequest = serde_json::from_value(request).expect("typed request");
    assert_eq!(
        request.validate_envelope(),
        Err(RequestValidationError::InvalidPayload)
    );
}
