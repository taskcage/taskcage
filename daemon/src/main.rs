//! 명령행 인자를 제한값과 셸 없는 실행 인자 배열로 바꾸는 시작점이다.

use std::env;
use std::ffi::{OsStr, OsString};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use taskcaged::cgroup::{CgroupLimits, CpuLimit};
use taskcaged::{Error, RunOnceConfig};
use tracing_subscriber::EnvFilter;

const DEFAULT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESSES: u32 = 64;
const DEFAULT_CPU_QUOTA_MICROS: u64 = 100_000;
const DEFAULT_CPU_PERIOD_MICROS: u64 = 100_000;
const DEFAULT_TIMEOUT_MILLIS: u64 = 30_000;
const DEFAULT_CLEANUP_MILLIS: u64 = 5_000;

#[tokio::main]
async fn main() -> taskcaged::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("taskcaged=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut args = env::args_os().skip(1);
    match args.next().as_deref() {
        None => taskcaged::run().await,
        Some(command) if command == OsStr::new("check-environment") => {
            if args.next().is_some() {
                return Err(Error::InvalidArgument(
                    "check-environment 뒤에는 인자를 받을 수 없습니다".to_owned(),
                ));
            }
            let report = taskcaged::check_environment()?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Some(command) if command == OsStr::new("run-once") => {
            let config = parse_run_once(args.collect())?;
            let report = taskcaged::run_once(config).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some(other) => Err(Error::InvalidArgument(format!(
            "알 수 없는 명령입니다: {other:?}; check-environment 또는 run-once를 사용하세요"
        ))),
    }
}

fn parse_run_once(args: Vec<OsString>) -> taskcaged::Result<RunOnceConfig> {
    // 값을 생략해도 무제한 실행이 되지 않도록 보수적인 기본 상한을 사용한다.
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
            let command = args[index + 1..].to_vec();
            if command.is_empty() {
                return Err(Error::InvalidArgument(
                    "-- 뒤에 실행 파일을 입력해야 합니다".to_owned(),
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

        let name = argument.to_str().ok_or_else(|| {
            Error::InvalidArgument(format!("옵션 이름이 UTF-8이 아닙니다: {argument:?}"))
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{name} 옵션 값이 없습니다")))?;
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
                        .ok_or_else(|| {
                            Error::InvalidArgument("job-id는 UTF-8이어야 합니다".to_owned())
                        })?
                        .to_owned(),
                );
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 run-once 옵션입니다: {name}"
                )));
            }
        }
        index += 2;
    }

    Err(Error::InvalidArgument(
        "사용법: taskcaged run-once [옵션] -- <실행 파일> [인자...]".to_owned(),
    ))
}

fn parse_number<T>(name: &str, value: &OsStr) -> taskcaged::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = value
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("{name} 값은 UTF-8이어야 합니다")))?;
    value
        .parse()
        .map_err(|error| Error::InvalidArgument(format!("잘못된 {name} 값입니다: {error}")))
}

fn nonzero_u64(name: &str, value: u64) -> taskcaged::Result<NonZeroU64> {
    NonZeroU64::new(value)
        .ok_or_else(|| Error::InvalidArgument(format!("{name} 값은 0보다 커야 합니다")))
}

fn nonzero_u32(name: &str, value: u32) -> taskcaged::Result<NonZeroU32> {
    NonZeroU32::new(value)
        .ok_or_else(|| Error::InvalidArgument(format!("{name} 값은 0보다 커야 합니다")))
}

fn generate_job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_after_separator_is_not_reparsed() {
        let config = parse_run_once(vec![
            OsString::from("--"),
            OsString::from("echo"),
            OsString::from("hello world"),
        ])
        .unwrap();
        assert_eq!(config.command[1], OsString::from("hello world"));
    }

    #[test]
    fn zero_limit_is_rejected() {
        let error = parse_run_once(vec![
            OsString::from("--memory-bytes"),
            OsString::from("0"),
            OsString::from("--"),
            OsString::from("echo"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("0보다 커야"));
    }
}
