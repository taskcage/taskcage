//! Embedded 전용 private execution helper.
//!
//! 이 바이너리는 공개 daemon이나 socket service가 아니다. Java SDK가 자식 프로세스로
//! 시작하고 stdin/stdout의 newline-delimited JSON으로 요청을 주고받는다. 실행의 실제
//! 제한·관찰·정리는 taskcage-core에 위임한다.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use taskcage_core::cgroup::{CgroupLimits, CgroupManager, CpuLimit};
use taskcage_core::deadline::MonotonicDeadline;
use taskcage_core::executor::{PreparedCommand, SpawnOutcome, WaitOutcome, spawn_in_cgroup};
use taskcage_core::output::CaptureLimits;
use taskcage_core::preflight::{CapabilityProbe, SystemProbe, VerifiedEnvironment};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const MAX_FRAME_BYTES: usize = 1_048_576;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    #[serde(rename = "getCapabilities")]
    GetCapabilities {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "execute")]
    Execute {
        #[serde(rename = "requestId")]
        request_id: String,
        payload: ExecutePayload,
    },
    #[serde(rename = "shutdown")]
    Shutdown {
        #[serde(rename = "requestId")]
        request_id: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutePayload {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    working_directory: String,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    limits: ResourceLimits,
    output: OutputLimits,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceLimits {
    cpu_max: CpuMax,
    memory_max_bytes: u64,
    pids_max: u64,
    wall_time_limit_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CpuMax {
    quota_micros: u64,
    period_micros: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputLimits {
    stdout_tail_max_bytes: u32,
    stderr_tail_max_bytes: u32,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum Response {
    #[serde(rename = "capabilities")]
    Capabilities {
        #[serde(rename = "requestId")]
        request_id: String,
        payload: CapabilitiesPayload,
    },
    #[serde(rename = "finished")]
    Finished {
        #[serde(rename = "requestId")]
        request_id: String,
        payload: FinishedPayload,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "requestId")]
        request_id: Option<String>,
        payload: ErrorPayload,
    },
    #[serde(rename = "shutdownAck")]
    ShutdownAck {
        #[serde(rename = "requestId")]
        request_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilitiesPayload {
    helper_version: &'static str,
    protocol_version: u32,
    max_frame_bytes: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FinishedPayload {
    outcome: &'static str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    stdout_tail: String,
    stderr_tail: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    cpu_usage_micros: u64,
    memory_peak_bytes: u64,
}

#[derive(Debug, Serialize)]
struct ErrorPayload {
    code: &'static str,
    message: String,
}

#[derive(Debug)]
struct Helper {
    manager: CgroupManager,
    sequence: u64,
}

impl Helper {
    fn initialize(environment: VerifiedEnvironment) -> Result<Self, String> {
        CgroupManager::initialize(environment)
            .map(|manager| Self {
                manager,
                sequence: 0,
            })
            .map_err(|error| error.to_string())
    }

    async fn execute(&mut self, request_id: &str, payload: ExecutePayload) -> Response {
        self.sequence = self.sequence.saturating_add(1);
        let job_id = format!("exec-{}-{}", sanitize_id(request_id), self.sequence);
        let wall_time_limit_ms = payload.limits.wall_time_limit_ms;
        let limits = match payload.limits.into_core() {
            Ok(value) => value,
            Err(message) => return error_response(request_id, "INVALID_LIMITS", message),
        };
        let capture = match payload.output.into_core() {
            Ok(value) => value,
            Err(message) => return error_response(request_id, "INVALID_OUTPUT_LIMITS", message),
        };
        let timeout = Duration::from_millis(wall_time_limit_ms);
        let Some(deadline) = MonotonicDeadline::from_now(timeout) else {
            return error_response(
                request_id,
                "INVALID_LIMITS",
                "wall time limit is invalid".into(),
            );
        };
        let mut cgroup = match self.manager.create_job(&job_id, limits) {
            Ok(value) => value,
            Err(error) => {
                return error_response(request_id, "CGROUP_CREATE_FAILED", error.to_string());
            }
        };
        let mut command = Vec::with_capacity(payload.args.len() + 1);
        command.push(OsString::from(&payload.program));
        command.extend(payload.args.into_iter().map(OsString::from));
        let prepared = match PreparedCommand::new(
            command,
            &PathBuf::from(payload.working_directory),
            payload
                .environment
                .into_iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                .collect(),
        ) {
            Ok(value) => value,
            Err(error) => {
                let _ = cgroup
                    .finish_until(MonotonicDeadline::from_now(CLEANUP_TIMEOUT).unwrap())
                    .await;
                return error_response(request_id, "INVALID_COMMAND", error.to_string());
            }
        };
        let pending = match spawn_in_cgroup(&prepared, cgroup.raw_fd(), capture) {
            Ok(value) => value,
            Err(error) => {
                let _ = cgroup
                    .finish_until(MonotonicDeadline::from_now(CLEANUP_TIMEOUT).unwrap())
                    .await;
                return error_response(request_id, "SPAWN_FAILED", error.to_string());
            }
        };
        let mut pending = pending;
        let token = match pending.commit_start_signal() {
            Ok(value) => value,
            Err(error) => {
                let _ = cgroup
                    .finish_until(MonotonicDeadline::from_now(CLEANUP_TIMEOUT).unwrap())
                    .await;
                return error_response(request_id, "START_FAILED", error.to_string());
            }
        };
        let started = match pending.into_start_committed(token).wait_for_exec() {
            Ok(SpawnOutcome::Started(value)) => value,
            Ok(SpawnOutcome::ExecFailed(value)) => {
                let _ = cgroup
                    .finish_until(MonotonicDeadline::from_now(CLEANUP_TIMEOUT).unwrap())
                    .await;
                return error_response(request_id, "EXEC_FAILED", format!("errno {}", value.errno));
            }
            Err(error) => {
                let _ = cgroup
                    .finish_until(MonotonicDeadline::from_now(CLEANUP_TIMEOUT).unwrap())
                    .await;
                return error_response(request_id, "START_FAILED", error.to_string());
            }
        };
        let wait = match started.wait_until(deadline).await {
            Ok(value) => value,
            Err(error) => return error_response(request_id, "WAIT_FAILED", error.to_string()),
        };
        let cleanup_deadline = MonotonicDeadline::from_now(CLEANUP_TIMEOUT).unwrap();
        let (outcome, exit) = match wait {
            WaitOutcome::Exited(exit) => ("SUCCEEDED", exit),
            WaitOutcome::TimedOut => {
                if let Err(error) = cgroup.kill_all() {
                    return error_response(request_id, "CLEANUP_FAILED", error.to_string());
                }
                match started.reap_after_kill_until(cleanup_deadline).await {
                    Ok(exit) => ("TIMED_OUT", exit),
                    Err(error) => {
                        return error_response(request_id, "CLEANUP_FAILED", error.to_string());
                    }
                }
            }
        };
        let output = match started.finish_output_until(cleanup_deadline).await {
            Ok(value) => value,
            Err(error) => return error_response(request_id, "OUTPUT_FAILED", error.to_string()),
        };
        let stats = match cgroup.finish_after_kill_until(cleanup_deadline).await {
            Ok(value) => value,
            Err(error) => return error_response(request_id, "CLEANUP_FAILED", error.to_string()),
        };
        Response::Finished {
            request_id: request_id.to_owned(),
            payload: FinishedPayload {
                outcome,
                exit_code: exit.exit_code,
                signal: exit.signal,
                stdout_tail: String::from_utf8_lossy(output.stdout.raw_tail()).into_owned(),
                stderr_tail: String::from_utf8_lossy(output.stderr.raw_tail()).into_owned(),
                stdout_truncated: output.stdout.truncated(),
                stderr_truncated: output.stderr.truncated(),
                cpu_usage_micros: stats.cpu_usage_micros,
                memory_peak_bytes: stats.memory_peak_bytes,
            },
        }
    }
}

impl ResourceLimits {
    fn into_core(self) -> Result<CgroupLimits, String> {
        Ok(CgroupLimits {
            memory_max_bytes: nonzero_u64("memoryMaxBytes", self.memory_max_bytes)?,
            max_processes: nonzero_u64("pidsMax", self.pids_max)?,
            cpu: CpuLimit {
                quota_micros: nonzero_u64("cpuMax.quotaMicros", self.cpu_max.quota_micros)?,
                period_micros: nonzero_u64("cpuMax.periodMicros", self.cpu_max.period_micros)?,
            },
        })
    }
}

impl OutputLimits {
    fn into_core(self) -> Result<CaptureLimits, String> {
        Ok(CaptureLimits::new(
            nonzero_usize("stdoutTailMaxBytes", self.stdout_tail_max_bytes)?,
            nonzero_usize("stderrTailMaxBytes", self.stderr_tail_max_bytes)?,
        ))
    }
}

fn nonzero_u64(field: &str, value: u64) -> Result<NonZeroU64, String> {
    NonZeroU64::new(value).ok_or_else(|| format!("{field} must be greater than zero"))
}

fn nonzero_usize(field: &str, value: u32) -> Result<NonZeroUsize, String> {
    NonZeroUsize::new(value as usize).ok_or_else(|| format!("{field} must be greater than zero"))
}

fn sanitize_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    if sanitized.is_empty() {
        "request".into()
    } else {
        sanitized
    }
}

fn error_response(request_id: &str, code: &'static str, message: String) -> Response {
    Response::Error {
        request_id: Some(request_id.to_owned()),
        payload: ErrorPayload { code, message },
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Response,
) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(response).map_err(io::Error::other)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let environment = SystemProbe::from_environment()
        .check()
        .map_err(io::Error::other)?;
    let mut helper = Helper::initialize(environment).map_err(io::Error::other)?;
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::BufWriter::new(tokio::io::stdout());
    while let Some(line) = lines.next_line().await? {
        if line.len() > MAX_FRAME_BYTES {
            write_response(
                &mut stdout,
                &Response::Error {
                    request_id: None,
                    payload: ErrorPayload {
                        code: "FRAME_TOO_LARGE",
                        message: "request exceeds maximum frame size".into(),
                    },
                },
            )
            .await?;
            continue;
        }
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(value) => value,
            Err(error) => {
                write_response(
                    &mut stdout,
                    &Response::Error {
                        request_id: None,
                        payload: ErrorPayload {
                            code: "MALFORMED_REQUEST",
                            message: error.to_string(),
                        },
                    },
                )
                .await?;
                continue;
            }
        };
        let shutdown = matches!(request, Request::Shutdown { .. });
        let response = match request {
            Request::GetCapabilities { request_id } => Response::Capabilities {
                request_id,
                payload: CapabilitiesPayload {
                    helper_version: env!("CARGO_PKG_VERSION"),
                    protocol_version: 1,
                    max_frame_bytes: MAX_FRAME_BYTES,
                },
            },
            Request::Execute {
                request_id,
                payload,
            } => helper.execute(&request_id, payload).await,
            Request::Shutdown { request_id } => Response::ShutdownAck { request_id },
        };
        write_response(&mut stdout, &response).await?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_camel_case_execute_request_without_shell_fields() {
        let request: Request = serde_json::from_str(
            r#"{"type":"execute","requestId":"r-1","payload":{"program":"/bin/echo","args":["hello world"],"workingDirectory":"/tmp","limits":{"cpuMax":{"quotaMicros":1000,"periodMicros":1000},"memoryMaxBytes":1,"pidsMax":1,"wallTimeLimitMs":1},"output":{"stdoutTailMaxBytes":1,"stderrTailMaxBytes":1}}}"#,
        )
        .expect("execute request should decode");
        match request {
            Request::Execute {
                request_id,
                payload,
            } => {
                assert_eq!(request_id, "r-1");
                assert_eq!(payload.args, vec!["hello world"]);
            }
            _ => panic!("expected execute request"),
        }
    }

    #[test]
    fn sanitizes_request_ids_for_cgroup_names() {
        assert_eq!(sanitize_id("request/with spaces"), "request-with-spaces");
        assert_eq!(sanitize_id(""), "request");
    }
}
