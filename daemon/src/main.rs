//! 명령행 인자를 읽어 작업 설정을 만들고 TaskCage 데몬 기능을 호출하는 시작점이다.

use std::env;
use std::ffi::{OsStr, OsString};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use taskcaged::cgroup::{CgroupLimits, CpuLimit};
use taskcaged::{Error, RunOnceConfig};

const DEFAULT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESSES: u32 = 64;
const DEFAULT_CPU_QUOTA_MICROS: u64 = 100_000;
const DEFAULT_CPU_PERIOD_MICROS: u64 = 100_000;
const DEFAULT_TIMEOUT_MILLIS: u64 = 30_000;
const DEFAULT_CLEANUP_MILLIS: u64 = 5_000;

#[tokio::main]
async fn main() -> taskcaged::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "taskcaged=info".into()),
        )
        .init();

    let mut args = env::args_os().skip(1);
    match args.next().as_deref() {
        None => taskcaged::run().await,
        Some(command) if command == OsStr::new("run-once") => {
            let config = parse_run_once(args.collect())?;
            let report = taskcaged::run_once(config).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some(other) => Err(Error::InvalidArgument(format!(
            "unknown command {other:?}; expected run-once"
        ))),
    }
}

fn parse_run_once(args: Vec<OsString>) -> taskcaged::Result<RunOnceConfig> {
    // 옵션을 생략해도 무제한으로 실행되지 않도록 보수적인 기본값을 먼저 넣는다.
    let mut cgroup_root = None;
    let mut memory_bytes = DEFAULT_MEMORY_BYTES;
    let mut max_processes = DEFAULT_MAX_PROCESSES;
    let mut cpu_quota_micros = DEFAULT_CPU_QUOTA_MICROS;
    let mut cpu_period_micros = DEFAULT_CPU_PERIOD_MICROS;
    let mut timeout_millis = DEFAULT_TIMEOUT_MILLIS;
    let mut cleanup_millis = DEFAULT_CLEANUP_MILLIS;
    let mut working_directory = None;
    let mut job_id = None;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if argument == OsStr::new("--") {
            // `--` 뒤의 값은 TaskCage 옵션으로 해석하지 않고 실행 파일과 인자로 그대로 넘긴다.
            let command = args[index + 1..].to_vec();
            if command.is_empty() {
                return Err(Error::InvalidArgument(
                    "run-once requires a command after --".to_owned(),
                ));
            }
            return Ok(RunOnceConfig {
                cgroup_root,
                job_id: job_id.unwrap_or_else(generate_job_id),
                limits: CgroupLimits {
                    memory_max_bytes: nonzero_u64("memory-bytes", memory_bytes)?,
                    max_processes: nonzero_u32("pids", max_processes)?,
                    cpu: CpuLimit {
                        quota_micros: nonzero_u64("cpu-quota-us", cpu_quota_micros)?,
                        period_micros: nonzero_u64("cpu-period-us", cpu_period_micros)?,
                    },
                },
                wall_timeout: Duration::from_millis(timeout_millis),
                cleanup_timeout: Duration::from_millis(cleanup_millis),
                working_directory,
                command,
            });
        }

        // TaskCage 옵션은 항상 이름과 값 한 쌍으로 읽는다.
        let name = argument.to_str().ok_or_else(|| {
            Error::InvalidArgument(format!("option name is not UTF-8: {argument:?}"))
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("missing value for option {name}")))?;
        match name {
            "--cgroup-root" if value != OsStr::new("auto") => {
                cgroup_root = Some(PathBuf::from(value));
            }
            "--cgroup-root" => cgroup_root = None,
            "--memory-bytes" => memory_bytes = parse_number(name, value)?,
            "--pids" => max_processes = parse_number(name, value)?,
            "--cpu-quota-us" => cpu_quota_micros = parse_number(name, value)?,
            "--cpu-period-us" => cpu_period_micros = parse_number(name, value)?,
            "--timeout-ms" => timeout_millis = parse_number(name, value)?,
            "--cleanup-timeout-ms" => cleanup_millis = parse_number(name, value)?,
            "--working-directory" => working_directory = Some(PathBuf::from(value)),
            "--job-id" => {
                job_id = Some(
                    value
                        .to_str()
                        .ok_or_else(|| Error::InvalidArgument("job-id must be UTF-8".to_owned()))?
                        .to_owned(),
                );
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "unknown run-once option {name}"
                )));
            }
        }
        index += 2;
    }

    Err(Error::InvalidArgument(
        "usage: taskcaged run-once [options] -- <executable> [args...]".to_owned(),
    ))
}

fn parse_number<T>(name: &str, value: &OsStr) -> taskcaged::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = value
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("{name} must be UTF-8")))?;
    value
        .parse()
        .map_err(|error| Error::InvalidArgument(format!("invalid {name}: {error}")))
}

fn nonzero_u64(name: &str, value: u64) -> taskcaged::Result<NonZeroU64> {
    NonZeroU64::new(value)
        .ok_or_else(|| Error::InvalidArgument(format!("{name} must be greater than zero")))
}

fn nonzero_u32(name: &str, value: u32) -> taskcaged::Result<NonZeroU32> {
    NonZeroU32::new(value)
        .ok_or_else(|| Error::InvalidArgument(format!("{name} must be greater than zero")))
}

fn generate_job_id() -> String {
    // 같은 데몬에서 빠르게 작업이 이어져도 겹치기 어렵도록 PID와 현재 시각을 함께 사용한다.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}
