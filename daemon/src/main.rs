//! 명령행 인자를 제한값과 셸 없는 실행 인자 배열로 바꾸는 시작점이다.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use taskcaged::cgroup::{CgroupLimits, CpuLimit};
use taskcaged::{Error, RunOnceConfig};
use tracing_subscriber::EnvFilter;

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
    let mut cgroup_root = None;
    let mut memory_bytes = None;
    let mut max_processes = None;
    let mut cpu_quota_micros = None;
    let mut cpu_period_micros = None;
    let mut timeout_millis = None;
    let mut cleanup_millis = DEFAULT_CLEANUP_MILLIS;
    let mut working_directory: Option<PathBuf> = None;
    let mut environment = BTreeMap::new();
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
            if !PathBuf::from(&command[0]).is_absolute() {
                return Err(Error::InvalidArgument(
                    "실행 파일은 절대 경로여야 합니다".to_owned(),
                ));
            }
            let working_directory = required_option("working-directory", working_directory)?;
            if !working_directory.is_absolute() {
                return Err(Error::InvalidArgument(
                    "working-directory는 절대 경로여야 합니다".to_owned(),
                ));
            }
            return Ok(RunOnceConfig {
                cgroup_root,
                job_id: job_id.unwrap_or_else(generate_job_id),
                limits: CgroupLimits {
                    memory_max_bytes: nonzero_u64(
                        "memory-bytes",
                        required_option("memory-bytes", memory_bytes)?,
                    )?,
                    max_processes: nonzero_u32("pids", required_option("pids", max_processes)?)?,
                    cpu: CpuLimit {
                        quota_micros: nonzero_u64(
                            "cpu-quota-us",
                            required_option("cpu-quota-us", cpu_quota_micros)?,
                        )?,
                        period_micros: nonzero_u64(
                            "cpu-period-us",
                            required_option("cpu-period-us", cpu_period_micros)?,
                        )?,
                    },
                },
                wall_timeout: Duration::from_millis(required_option("timeout-ms", timeout_millis)?),
                cleanup_timeout: Duration::from_millis(cleanup_millis),
                working_directory,
                environment,
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
            "--memory-bytes" => memory_bytes = Some(parse_number(name, value)?),
            "--pids" => max_processes = Some(parse_number(name, value)?),
            "--cpu-quota-us" => cpu_quota_micros = Some(parse_number(name, value)?),
            "--cpu-period-us" => cpu_period_micros = Some(parse_number(name, value)?),
            "--timeout-ms" => timeout_millis = Some(parse_number(name, value)?),
            "--cleanup-timeout-ms" => cleanup_millis = parse_number(name, value)?,
            "--working-directory" => working_directory = Some(PathBuf::from(value)),
            "--env" => {
                let (key, value) = parse_environment(value)?;
                if environment.insert(key.clone(), value).is_some() {
                    return Err(Error::InvalidArgument(format!(
                        "환경 변수가 중복되었습니다: {key:?}"
                    )));
                }
            }
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

fn required_option<T>(name: &str, value: Option<T>) -> taskcaged::Result<T> {
    value.ok_or_else(|| Error::InvalidArgument(format!("{name} 옵션은 필수입니다")))
}

fn parse_environment(value: &OsStr) -> taskcaged::Result<(OsString, OsString)> {
    let value = value
        .to_str()
        .ok_or_else(|| Error::InvalidArgument("환경 변수는 UTF-8이어야 합니다".to_owned()))?;
    let (key, value) = value.split_once('=').ok_or_else(|| {
        Error::InvalidArgument("환경 변수는 KEY=VALUE 형식이어야 합니다".to_owned())
    })?;
    if key.is_empty() {
        return Err(Error::InvalidArgument(
            "환경 변수 이름은 비어 있을 수 없습니다".to_owned(),
        ));
    }
    Ok((OsString::from(key), OsString::from(value)))
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
        let working_directory = std::env::temp_dir();
        let program = working_directory.join("echo");
        let config = parse_run_once(vec![
            OsString::from("--memory-bytes"),
            OsString::from("1024"),
            OsString::from("--pids"),
            OsString::from("2"),
            OsString::from("--cpu-quota-us"),
            OsString::from("1000"),
            OsString::from("--cpu-period-us"),
            OsString::from("10000"),
            OsString::from("--timeout-ms"),
            OsString::from("1000"),
            OsString::from("--working-directory"),
            working_directory.into_os_string(),
            OsString::from("--"),
            program.into_os_string(),
            OsString::from("hello world"),
        ])
        .unwrap();
        assert_eq!(config.command[1], OsString::from("hello world"));
    }

    #[test]
    fn zero_limit_is_rejected() {
        let working_directory = std::env::temp_dir();
        let program = working_directory.join("echo");
        let error = parse_run_once(vec![
            OsString::from("--memory-bytes"),
            OsString::from("0"),
            OsString::from("--pids"),
            OsString::from("2"),
            OsString::from("--cpu-quota-us"),
            OsString::from("1000"),
            OsString::from("--cpu-period-us"),
            OsString::from("10000"),
            OsString::from("--timeout-ms"),
            OsString::from("1000"),
            OsString::from("--working-directory"),
            working_directory.into_os_string(),
            OsString::from("--"),
            program.into_os_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("0보다 커야"));
    }

    #[test]
    fn working_directory_is_not_defaulted() {
        let program = std::env::temp_dir().join("echo");
        let error =
            parse_run_once(vec![OsString::from("--"), program.into_os_string()]).unwrap_err();

        assert!(error.to_string().contains("working-directory 옵션은 필수"));
    }

    #[test]
    fn resource_limits_are_not_defaulted() {
        let working_directory = std::env::temp_dir();
        let program = working_directory.join("echo");
        let error = parse_run_once(vec![
            OsString::from("--working-directory"),
            working_directory.into_os_string(),
            OsString::from("--"),
            program.into_os_string(),
        ])
        .unwrap_err();

        assert!(error.to_string().contains("memory-bytes 옵션은 필수"));
    }

    #[test]
    fn explicit_environment_is_preserved() {
        let working_directory = std::env::temp_dir();
        let program = working_directory.join("echo");
        let config = parse_run_once(vec![
            OsString::from("--memory-bytes"),
            OsString::from("1024"),
            OsString::from("--pids"),
            OsString::from("2"),
            OsString::from("--cpu-quota-us"),
            OsString::from("1000"),
            OsString::from("--cpu-period-us"),
            OsString::from("10000"),
            OsString::from("--timeout-ms"),
            OsString::from("1000"),
            OsString::from("--working-directory"),
            working_directory.into_os_string(),
            OsString::from("--env"),
            OsString::from("LANG=C.UTF-8"),
            OsString::from("--"),
            program.into_os_string(),
        ])
        .unwrap();

        assert_eq!(
            config.environment.get(OsStr::new("LANG")),
            Some(&OsString::from("C.UTF-8"))
        );
    }

    #[test]
    fn rejects_relative_program_before_execution() {
        let error = parse_run_once(vec![OsString::from("--"), OsString::from("echo")]).unwrap_err();

        assert!(error.to_string().contains("절대 경로"));
    }
}
