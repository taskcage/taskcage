//! 명령행 인자를 제한값과 셸 없는 실행 인자 배열로 바꾸는 시작점이다.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use taskcaged::cgroup::{CgroupLimits, CpuLimit};
use taskcaged::output::CaptureLimits;
use taskcaged::{DaemonConfig, Error, RunOnceConfig};
use tracing_subscriber::EnvFilter;

const DEFAULT_CLEANUP_MILLIS: u64 = 5_000;

#[tokio::main]
async fn main() -> taskcaged::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("taskcaged=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut args = env::args_os().skip(1);
    match args.next().as_deref() {
        None => Err(Error::InvalidArgument(
            "서비스 실행에는 serve와 명시적 socket 설정이 필요합니다".to_owned(),
        )),
        Some(command) if command == OsStr::new("serve") => {
            let config = parse_serve(args.collect())?;
            taskcaged::run(config).await
        }
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
            "알 수 없는 명령입니다: {other:?}; serve, check-environment 또는 run-once를 사용하세요"
        ))),
    }
}

fn parse_serve(args: Vec<OsString>) -> taskcaged::Result<DaemonConfig> {
    let mut socket_path = None;
    let mut max_concurrent_tasks = None;
    let mut cleanup_timeout_ms = None;
    let mut fail_stop_timeout_ms = None;
    let mut index = 0;
    while index < args.len() {
        let name = args[index].to_str().ok_or_else(|| {
            Error::InvalidArgument("serve 옵션 이름은 UTF-8이어야 합니다".to_owned())
        })?;
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::InvalidArgument(format!("{name} 옵션 값이 없습니다")))?;
        match name {
            "--socket" if socket_path.is_none() => socket_path = Some(PathBuf::from(value)),
            "--max-concurrent-tasks" if max_concurrent_tasks.is_none() => {
                max_concurrent_tasks = Some(parse_number(name, value)?);
            }
            "--cleanup-timeout-ms" if cleanup_timeout_ms.is_none() => {
                cleanup_timeout_ms = Some(parse_number(name, value)?);
            }
            "--fail-stop-timeout-ms" if fail_stop_timeout_ms.is_none() => {
                fail_stop_timeout_ms = Some(parse_number(name, value)?);
            }
            "--socket"
            | "--max-concurrent-tasks"
            | "--cleanup-timeout-ms"
            | "--fail-stop-timeout-ms" => {
                return Err(Error::InvalidArgument(format!(
                    "serve 옵션이 중복되었습니다: {name}"
                )));
            }
            _ => {
                return Err(Error::InvalidArgument(format!(
                    "알 수 없는 serve 옵션입니다: {name}"
                )));
            }
        }
        index += 2;
    }

    DaemonConfig::new(
        required_option("socket", socket_path)?,
        required_option("max-concurrent-tasks", max_concurrent_tasks)?,
        Duration::from_millis(required_option("cleanup-timeout-ms", cleanup_timeout_ms)?),
        Duration::from_millis(required_option(
            "fail-stop-timeout-ms",
            fail_stop_timeout_ms,
        )?),
    )
}

fn parse_run_once(args: Vec<OsString>) -> taskcaged::Result<RunOnceConfig> {
    let mut cgroup_root = None;
    let mut memory_bytes = None;
    let mut max_processes = None;
    let mut cpu_quota_micros = None;
    let mut cpu_period_micros = None;
    let mut timeout_millis = None;
    let mut stdout_tail_bytes = None;
    let mut stderr_tail_bytes = None;
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
                    max_processes: NonZeroU64::from(nonzero_u32(
                        "pids",
                        required_option("pids", max_processes)?,
                    )?),
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
                capture_limits: CaptureLimits::new(
                    nonzero_usize(
                        "stdout-tail-bytes",
                        required_option("stdout-tail-bytes", stdout_tail_bytes)?,
                    )?,
                    nonzero_usize(
                        "stderr-tail-bytes",
                        required_option("stderr-tail-bytes", stderr_tail_bytes)?,
                    )?,
                ),
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
            "--stdout-tail-bytes" => stdout_tail_bytes = Some(parse_number(name, value)?),
            "--stderr-tail-bytes" => stderr_tail_bytes = Some(parse_number(name, value)?),
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

fn nonzero_usize(name: &str, value: usize) -> taskcaged::Result<NonZeroUsize> {
    NonZeroUsize::new(value)
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
    fn serve_requires_an_explicit_absolute_socket_and_internal_limits() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let config = parse_serve(vec![
            OsString::from("--socket"),
            socket.into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("2"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap();
        assert!(format!("{config:?}").contains("max_concurrent_tasks: 2"));

        let error = parse_serve(vec![
            OsString::from("--socket"),
            OsString::from("relative.sock"),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("2"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("절대 경로"));
    }

    #[test]
    fn serve_rejects_zero_or_missing_settings() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let error = parse_serve(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("0"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("0보다 커야"));

        let error = parse_serve(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("cleanup-timeout-ms 옵션은 필수"));

        let error = parse_serve(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("0"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("fail-stop timeout은 0보다 커야"));

        let error = parse_serve(vec![
            OsString::from("--socket"),
            socket.into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
        ])
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("fail-stop-timeout-ms 옵션은 필수")
        );
    }

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
            OsString::from("--stdout-tail-bytes"),
            OsString::from("64"),
            OsString::from("--stderr-tail-bytes"),
            OsString::from("64"),
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
            OsString::from("--stdout-tail-bytes"),
            OsString::from("64"),
            OsString::from("--stderr-tail-bytes"),
            OsString::from("64"),
            OsString::from("--working-directory"),
            working_directory.into_os_string(),
            OsString::from("--"),
            program.into_os_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("0보다 커야"));
    }

    #[test]
    fn zero_output_limit_is_rejected() {
        let working_directory = std::env::temp_dir();
        let program = working_directory.join("echo");
        let error = parse_run_once(vec![
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
            OsString::from("--stdout-tail-bytes"),
            OsString::from("0"),
            OsString::from("--stderr-tail-bytes"),
            OsString::from("64"),
            OsString::from("--working-directory"),
            working_directory.into_os_string(),
            OsString::from("--"),
            program.into_os_string(),
        ])
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("stdout-tail-bytes 값은 0보다 커야")
        );
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
            OsString::from("--stdout-tail-bytes"),
            OsString::from("64"),
            OsString::from("--stderr-tail-bytes"),
            OsString::from("64"),
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
