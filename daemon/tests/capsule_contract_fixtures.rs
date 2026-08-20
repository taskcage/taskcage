use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use taskcage_core::CapsuleIdentity as CoreCapsuleIdentity;

const REQUEST_FIXTURES: [&str; 2] = ["error-capsule-profile-mismatch.json", "request-valid.json"];
const RESULT_FIXTURES: [&str; 5] = [
    "result-cancelled.json",
    "result-failed.json",
    "result-output-contract-failed.json",
    "result-success.json",
    "result-timeout.json",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Identity {
    name: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CapsuleRequestFixture {
    capsule: Identity,
    profile: Identity,
    inputs: BTreeMap<String, InputValue>,
    resource_overrides: ResourceOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
enum InputValue {
    Artifact {
        digest: String,
        #[serde(rename = "sizeBytes")]
        size_bytes: u64,
        #[serde(rename = "mediaType")]
        media_type: String,
    },
    Int64 {
        value: i64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ResourceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limits: Option<ResourceLimits>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ResourceLimits {
    wall_time_limit_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MismatchFixture {
    request: CapsuleRequestFixture,
    error: SemanticError,
    side_effects: SideEffects,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SemanticError {
    code: String,
    retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SideEffects {
    task_created: bool,
    artifact_staged: bool,
    cgroup_created: bool,
    process_started: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TerminalFixture {
    task_id: String,
    capsule: Identity,
    profile: Identity,
    state: TaskState,
    outcome: ProfileOutcome,
    termination_reason: TerminationReason,
    cleanup_confirmed: bool,
    process: ProcessResult,
    timing: TaskTiming,
    usage: TaskUsage,
    output: TaskOutput,
    artifacts: BTreeMap<String, PublishedArtifact>,
    failure: Option<ProfileFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum TaskState {
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ProfileOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum TerminationReason {
    Exited,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProcessResult {
    exit_code: Option<i32>,
    signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TaskTiming {
    submitted_at: String,
    started_at: String,
    finished_at: String,
    wall_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TaskUsage {
    cpu_time_micros: u64,
    memory_peak_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TaskOutput {
    stdout_tail: String,
    stderr_tail: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublishedArtifact {
    kind: ArtifactKind,
    path: String,
    digest: String,
    size_bytes: u64,
    media_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ArtifactKind {
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProfileFailure {
    code: String,
    message: String,
}

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("daemon crate는 repository root 아래에 있습니다")
        .join("protocol-fixtures")
        .join("capsule-v1")
}

fn fixture_value(name: &str) -> Value {
    serde_json::from_slice(
        &fs::read(fixture_directory().join(name)).expect("Capsule fixture를 읽어야 합니다"),
    )
    .expect("Capsule fixture는 유효한 JSON이어야 합니다")
}

fn decode_fixture<T>(name: &str) -> T
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let original = fixture_value(name);
    let decoded: T = serde_json::from_value(original.clone()).expect("typed fixture decode");
    assert_eq!(
        serde_json::to_value(&decoded).expect("typed fixture encode"),
        original,
        "fixture round-trip mismatch: {name}"
    );
    decoded
}

fn assert_valid_identity(identity: &Identity) -> CoreCapsuleIdentity {
    CoreCapsuleIdentity::new(&identity.name, &identity.version)
        .expect("fixture identity는 public Capsule identity여야 합니다")
}

#[test]
fn fixture_corpus_matches_the_documented_capsule_v1_set() {
    let actual: BTreeSet<_> = fs::read_dir(fixture_directory())
        .expect("fixture directory를 읽어야 합니다")
        .map(|entry| entry.expect("fixture entry를 읽어야 합니다"))
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "json")
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let expected: BTreeSet<_> = REQUEST_FIXTURES
        .into_iter()
        .chain(RESULT_FIXTURES)
        .map(str::to_owned)
        .collect();

    assert_eq!(actual, expected);
}

#[test]
fn valid_request_preserves_identity_typed_inputs_and_override() {
    let request: CapsuleRequestFixture = decode_fixture("request-valid.json");
    let capsule = assert_valid_identity(&request.capsule);
    let profile = assert_valid_identity(&request.profile);

    assert_eq!(capsule.name(), profile.name());
    assert_eq!(capsule.version(), profile.version());
    assert!(matches!(
        request.inputs.get("source"),
        Some(InputValue::Artifact {
            size_bytes: 1_024,
            media_type,
            ..
        }) if media_type == "audio/mpeg"
    ));
    assert!(matches!(
        request.inputs.get("sample_rate_hz"),
        Some(InputValue::Int64 { value: 16_000 })
    ));
    assert!(matches!(
        request.inputs.get("channels"),
        Some(InputValue::Int64 { value: 1 })
    ));
    assert_eq!(
        request
            .resource_overrides
            .limits
            .expect("limits override")
            .wall_time_limit_ms,
        120_000
    );
}

#[test]
fn identity_mismatch_is_non_retryable_and_pre_execution() {
    let fixture: MismatchFixture = decode_fixture("error-capsule-profile-mismatch.json");
    let capsule = assert_valid_identity(&fixture.request.capsule);
    let profile = assert_valid_identity(&fixture.request.profile);

    assert_ne!(capsule.name(), profile.name());
    assert_eq!(fixture.error.code, "CAPSULE_PROFILE_MISMATCH");
    assert!(!fixture.error.retryable);
    assert_eq!(
        fixture.side_effects,
        SideEffects {
            task_created: false,
            artifact_staged: false,
            cgroup_created: false,
            process_started: false,
        }
    );
}

#[test]
fn terminal_fixtures_preserve_cleanup_artifact_and_failure_meaning() {
    let cases = [
        (
            "result-cancelled.json",
            ProfileOutcome::Failed,
            TerminationReason::Cancelled,
            Some("CANCELLED"),
        ),
        (
            "result-failed.json",
            ProfileOutcome::Failed,
            TerminationReason::Exited,
            Some("PROCESS_FAILED"),
        ),
        (
            "result-output-contract-failed.json",
            ProfileOutcome::Failed,
            TerminationReason::Exited,
            Some("OUTPUT_CONTRACT_VIOLATION"),
        ),
        (
            "result-success.json",
            ProfileOutcome::Succeeded,
            TerminationReason::Exited,
            None,
        ),
        (
            "result-timeout.json",
            ProfileOutcome::Failed,
            TerminationReason::TimedOut,
            Some("TIMEOUT"),
        ),
    ];

    for (name, expected_outcome, expected_reason, expected_failure) in cases {
        let fixture: TerminalFixture = decode_fixture(name);
        let capsule = assert_valid_identity(&fixture.capsule);
        let profile = assert_valid_identity(&fixture.profile);

        assert_eq!(capsule.name(), profile.name(), "identity name: {name}");
        assert_eq!(
            capsule.version(),
            profile.version(),
            "identity version: {name}"
        );
        assert_eq!(fixture.state, TaskState::Finished, "state: {name}");
        assert_eq!(fixture.outcome, expected_outcome, "outcome: {name}");
        assert_eq!(
            fixture.termination_reason, expected_reason,
            "termination: {name}"
        );
        assert!(fixture.cleanup_confirmed, "cleanup: {name}");

        if expected_outcome == ProfileOutcome::Succeeded {
            assert_eq!(fixture.process.exit_code, Some(0), "exit code: {name}");
            assert_eq!(fixture.artifacts.len(), 1, "artifacts: {name}");
            assert!(fixture.failure.is_none(), "failure: {name}");
        } else {
            assert!(fixture.artifacts.is_empty(), "artifacts: {name}");
            assert_eq!(
                fixture
                    .failure
                    .as_ref()
                    .map(|failure| failure.code.as_str()),
                expected_failure,
                "failure code: {name}"
            );
        }
    }
}
