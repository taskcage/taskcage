use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use taskcaged::codec::{decode_json, encode_json_frame};
use taskcaged::protocol::{
    ErrorCode, Request, Response, TaskPayload, TaskState, TerminationReason,
};
use taskcaged::resource_budget::ResourceBudget;

const REQUEST_FIXTURES: [&str; 1] = ["submit-task-valid.json"];
const RESPONSE_FIXTURES: [&str; 6] = [
    "error-capacity-exhausted.json",
    "task-accepted.json",
    "task-result-execution-failed.json",
    "task-result-output-truncated.json",
    "task-result-timeout.json",
    "task-running.json",
];

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("daemon crate는 repository root 아래에 있습니다")
        .join("protocol-fixtures")
        .join("v1")
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    fs::read(fixture_directory().join(name)).expect("protocol fixture를 읽어야 합니다")
}

fn assert_semantic_round_trip<T>(name: &str, message: &T)
where
    T: Serialize,
{
    let original: Value =
        serde_json::from_slice(&fixture_bytes(name)).expect("fixture는 유효한 JSON이어야 합니다");
    let serialized = serde_json::to_value(message).expect("typed message를 JSON으로 바꿔야 합니다");
    assert_eq!(serialized, original, "fixture round-trip mismatch: {name}");

    let frame = encode_json_frame(message).expect("fixture message를 frame으로 만들어야 합니다");
    let encoded_length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    assert_eq!(encoded_length, frame.len() - 4, "frame length: {name}");
}

#[test]
fn fixture_corpus_matches_the_documented_v1_set() {
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
        .chain(RESPONSE_FIXTURES)
        .map(str::to_owned)
        .collect();

    assert_eq!(actual, expected);
}

#[test]
fn request_fixtures_decode_and_round_trip() {
    for name in REQUEST_FIXTURES {
        let request: Request = decode_json(&fixture_bytes(name)).expect("request fixture decode");
        assert_eq!(request.protocol_version(), 1, "protocol version: {name}");
        assert!(!request.request_id().is_empty(), "request id: {name}");
        assert_semantic_round_trip(name, &request);
    }
}

#[test]
fn response_fixtures_decode_and_round_trip() {
    for name in RESPONSE_FIXTURES {
        let response: Response =
            decode_json(&fixture_bytes(name)).expect("response fixture decode");
        assert_eq!(response.protocol_version(), 1, "protocol version: {name}");
        assert!(!response.request_id().is_empty(), "request id: {name}");
        assert_semantic_round_trip(name, &response);
    }
}

#[test]
fn submit_fixture_preserves_command_and_required_budgets() {
    let request: Request = decode_json(&fixture_bytes("submit-task-valid.json")).unwrap();
    let Request::SubmitTask { payload, .. } = request else {
        panic!("submit fixture must contain submitTask");
    };

    assert_eq!(payload.command.program, "/usr/bin/pdftotext");
    assert_eq!(payload.command.working_directory, "/srv/taskcage/jobs/42");
    assert_eq!(
        payload.command.environment.get("LANG").map(String::as_str),
        Some("C.UTF-8")
    );
    assert_eq!(payload.limits.cpu_max.quota_micros, 100_000);
    assert_eq!(payload.limits.cpu_max.period_micros, 100_000);
    assert_eq!(payload.limits.memory_max_bytes, 536_870_912);
    assert_eq!(payload.limits.pids_max, 32);
    assert_eq!(payload.limits.wall_time_limit_ms, 120_000);
    assert_eq!(payload.output.stdout_tail_max_bytes, 65_536);
    assert_eq!(payload.output.stderr_tail_max_bytes, 65_536);
}

#[test]
fn submit_fixture_converts_to_execution_budget_without_loss() {
    let request: Request = decode_json(&fixture_bytes("submit-task-valid.json")).unwrap();
    let Request::SubmitTask { payload, .. } = request else {
        panic!("submit fixture must contain submitTask");
    };
    let expected_limits = payload.limits.clone();

    let budget = ResourceBudget::try_from_protocol(payload.limits, payload.output).unwrap();
    let cgroup = budget.cgroup_limits();

    assert_eq!(cgroup.cpu.quota_micros.get(), 100_000);
    assert_eq!(cgroup.cpu.period_micros.get(), 100_000);
    assert_eq!(cgroup.memory_max_bytes.get(), 536_870_912);
    assert_eq!(cgroup.max_processes.get(), 32);
    assert_eq!(budget.wall_timeout(), std::time::Duration::from_secs(120));
    assert_eq!(budget.stdout_tail_max_bytes(), 65_536);
    assert_eq!(budget.stderr_tail_max_bytes(), 65_536);
    assert_eq!(budget.effective_limits(), &expected_limits);
}

#[test]
fn task_fixtures_preserve_state_and_terminal_meaning() {
    let accepted: Response = decode_json(&fixture_bytes("task-accepted.json")).unwrap();
    assert!(matches!(
        accepted,
        Response::TaskAccepted { payload, .. } if payload.state == TaskState::Running
    ));

    let running: Response = decode_json(&fixture_bytes("task-running.json")).unwrap();
    assert!(matches!(
        running,
        Response::Task {
            payload: TaskPayload::Running { .. },
            ..
        }
    ));

    let timeout: Response = decode_json(&fixture_bytes("task-result-timeout.json")).unwrap();
    assert!(matches!(
        timeout,
        Response::Task {
            payload: TaskPayload::Finished {
                termination_reason: TerminationReason::TimedOut,
                ..
            },
            ..
        }
    ));

    let execution_failed: Response =
        decode_json(&fixture_bytes("task-result-execution-failed.json")).unwrap();
    assert!(matches!(
        execution_failed,
        Response::Task {
            payload: TaskPayload::Finished {
                termination_reason: TerminationReason::ExecutionFailed,
                process,
                ..
            },
            ..
        } if process.exit_code.is_none() && process.signal.is_none()
    ));

    let truncated: Response =
        decode_json(&fixture_bytes("task-result-output-truncated.json")).unwrap();
    assert!(matches!(
        truncated,
        Response::Task {
            payload: TaskPayload::Finished {
                termination_reason: TerminationReason::Exited,
                output,
                ..
            },
            ..
        } if output.stdout_truncated && !output.stderr_truncated
    ));
}

#[test]
fn capacity_fixture_preserves_retryable_error() {
    let response: Response = decode_json(&fixture_bytes("error-capacity-exhausted.json")).unwrap();
    assert!(matches!(
        response,
        Response::Error { payload, .. }
            if payload.code == ErrorCode::CapacityExhausted && payload.retryable
    ));
}
