use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use taskcaged::protocol::{CpuMax, OutputLimits, ResourceLimits};
use taskcaged::{DaemonConfig, DeploymentResourceMaximum, Error};

use super::{parse_number, required_option};

pub(crate) fn parse(args: Vec<OsString>) -> taskcaged::Result<DaemonConfig> {
    let mut socket_path = None;
    let mut max_concurrent_tasks = None;
    let mut max_registry_tasks = None;
    let mut max_concurrent_connections = None;
    let mut cleanup_timeout_ms = None;
    let mut fail_stop_timeout_ms = None;
    let mut max_task_cpu_quota_micros = None;
    let mut max_task_cpu_period_micros = None;
    let mut max_task_memory_bytes = None;
    let mut max_task_pids = None;
    let mut max_task_timeout_ms = None;
    let mut max_task_stdout_tail_bytes = None;
    let mut max_task_stderr_tail_bytes = None;
    let mut profile_artifact_root = None;
    let mut profile_artifact_max_bytes = None;
    let mut runtime_package_cache_root = None;
    let mut ffmpeg_audio_to_wav_package_digest = None;
    let mut bundle_cache_root = None;
    let mut remote_config = None;
    let mut metrics_listen = None;
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
            "--max-registry-tasks" if max_registry_tasks.is_none() => {
                max_registry_tasks = Some(parse_number(name, value)?);
            }
            "--max-concurrent-connections" if max_concurrent_connections.is_none() => {
                max_concurrent_connections = Some(parse_number(name, value)?);
            }
            "--cleanup-timeout-ms" if cleanup_timeout_ms.is_none() => {
                cleanup_timeout_ms = Some(parse_number(name, value)?);
            }
            "--fail-stop-timeout-ms" if fail_stop_timeout_ms.is_none() => {
                fail_stop_timeout_ms = Some(parse_number(name, value)?);
            }
            "--max-task-cpu-quota-us" if max_task_cpu_quota_micros.is_none() => {
                max_task_cpu_quota_micros = Some(parse_number(name, value)?);
            }
            "--max-task-cpu-period-us" if max_task_cpu_period_micros.is_none() => {
                max_task_cpu_period_micros = Some(parse_number(name, value)?);
            }
            "--max-task-memory-bytes" if max_task_memory_bytes.is_none() => {
                max_task_memory_bytes = Some(parse_number(name, value)?);
            }
            "--max-task-pids" if max_task_pids.is_none() => {
                max_task_pids = Some(parse_number(name, value)?);
            }
            "--max-task-timeout-ms" if max_task_timeout_ms.is_none() => {
                max_task_timeout_ms = Some(parse_number(name, value)?);
            }
            "--max-task-stdout-tail-bytes" if max_task_stdout_tail_bytes.is_none() => {
                max_task_stdout_tail_bytes = Some(parse_number(name, value)?);
            }
            "--max-task-stderr-tail-bytes" if max_task_stderr_tail_bytes.is_none() => {
                max_task_stderr_tail_bytes = Some(parse_number(name, value)?);
            }
            "--profile-artifact-root" if profile_artifact_root.is_none() => {
                profile_artifact_root = Some(PathBuf::from(value));
            }
            "--profile-artifact-max-bytes" if profile_artifact_max_bytes.is_none() => {
                profile_artifact_max_bytes = Some(parse_number(name, value)?);
            }
            "--runtime-package-cache-root" if runtime_package_cache_root.is_none() => {
                runtime_package_cache_root = Some(PathBuf::from(value));
            }
            "--ffmpeg-audio-to-wav-package-digest"
                if ffmpeg_audio_to_wav_package_digest.is_none() =>
            {
                let value = value.to_str().ok_or_else(|| {
                    Error::InvalidArgument(
                        "FFmpeg Runtime Package digest는 UTF-8이어야 합니다".to_owned(),
                    )
                })?;
                ffmpeg_audio_to_wav_package_digest = Some(
                    taskcaged::digest::Sha256Digest::from_str(value).map_err(|error| {
                        Error::InvalidArgument(format!(
                            "잘못된 --ffmpeg-audio-to-wav-package-digest 값입니다: {error}"
                        ))
                    })?,
                );
            }
            "--bundle-cache-root" if bundle_cache_root.is_none() => {
                bundle_cache_root = Some(PathBuf::from(value));
            }
            "--remote-config" if remote_config.is_none() => {
                remote_config = Some(PathBuf::from(value));
            }
            "--metrics-listen" if metrics_listen.is_none() => {
                let value = value.to_str().ok_or_else(|| {
                    Error::InvalidArgument("metrics listen address는 UTF-8이어야 합니다".to_owned())
                })?;
                metrics_listen = Some(value.parse::<SocketAddr>().map_err(|error| {
                    Error::InvalidArgument(format!("잘못된 --metrics-listen 값입니다: {error}"))
                })?);
            }
            "--socket"
            | "--max-concurrent-tasks"
            | "--max-registry-tasks"
            | "--max-concurrent-connections"
            | "--cleanup-timeout-ms"
            | "--fail-stop-timeout-ms"
            | "--max-task-cpu-quota-us"
            | "--max-task-cpu-period-us"
            | "--max-task-memory-bytes"
            | "--max-task-pids"
            | "--max-task-timeout-ms"
            | "--max-task-stdout-tail-bytes"
            | "--max-task-stderr-tail-bytes"
            | "--profile-artifact-root"
            | "--profile-artifact-max-bytes"
            | "--runtime-package-cache-root"
            | "--ffmpeg-audio-to-wav-package-digest"
            | "--bundle-cache-root"
            | "--remote-config"
            | "--metrics-listen" => {
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

    let config = DaemonConfig::new(
        required_option("socket", socket_path)?,
        required_option("max-concurrent-tasks", max_concurrent_tasks)?,
        required_option("max-registry-tasks", max_registry_tasks)?,
        required_option("max-concurrent-connections", max_concurrent_connections)?,
        Duration::from_millis(required_option("cleanup-timeout-ms", cleanup_timeout_ms)?),
        Duration::from_millis(required_option(
            "fail-stop-timeout-ms",
            fail_stop_timeout_ms,
        )?),
        DeploymentResourceMaximum::new(
            ResourceLimits {
                cpu_max: CpuMax {
                    quota_micros: required_option(
                        "max-task-cpu-quota-us",
                        max_task_cpu_quota_micros,
                    )?,
                    period_micros: required_option(
                        "max-task-cpu-period-us",
                        max_task_cpu_period_micros,
                    )?,
                },
                memory_max_bytes: required_option("max-task-memory-bytes", max_task_memory_bytes)?,
                pids_max: required_option("max-task-pids", max_task_pids)?,
                wall_time_limit_ms: required_option("max-task-timeout-ms", max_task_timeout_ms)?,
            },
            OutputLimits {
                stdout_tail_max_bytes: required_option(
                    "max-task-stdout-tail-bytes",
                    max_task_stdout_tail_bytes,
                )?,
                stderr_tail_max_bytes: required_option(
                    "max-task-stderr-tail-bytes",
                    max_task_stderr_tail_bytes,
                )?,
            },
        ),
    )?;
    let config = match (profile_artifact_root, profile_artifact_max_bytes) {
        (None, None) => config,
        (Some(root), Some(maximum_bytes)) => {
            config.with_file_copy_profile(root, maximum_bytes)?
        }
        _ => Err(Error::InvalidArgument(
            "file-copy Profile에는 --profile-artifact-root와 --profile-artifact-max-bytes를 함께 지정해야 합니다"
                .to_owned(),
        ))?,
    };
    let config = match (
        runtime_package_cache_root,
        ffmpeg_audio_to_wav_package_digest,
    ) {
        (None, None) => config,
        (Some(cache_root), Some(digest)) => {
            config.with_ffmpeg_audio_to_wav_profile(cache_root, digest)?
        }
        _ => {
            return Err(Error::InvalidArgument(
                "FFmpeg Profile 등록에는 --runtime-package-cache-root와 --ffmpeg-audio-to-wav-package-digest를 함께 지정해야 합니다"
                    .to_owned(),
            ));
        }
    };
    let config = match bundle_cache_root {
        Some(cache_root) => config.with_bundle_profile_catalog(cache_root)?,
        None => config,
    };
    let config = match remote_config {
        Some(path) => config.with_remote_config(path),
        None => Ok(config),
    }?;
    match metrics_listen {
        Some(address) => config.with_metrics_listen(address),
        None => Ok(config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_deployment_policy(mut args: Vec<OsString>) -> Vec<OsString> {
        args.extend([
            OsString::from("--max-task-cpu-quota-us"),
            OsString::from("200000"),
            OsString::from("--max-task-cpu-period-us"),
            OsString::from("100000"),
            OsString::from("--max-task-memory-bytes"),
            OsString::from("2147483648"),
            OsString::from("--max-task-pids"),
            OsString::from("128"),
            OsString::from("--max-task-timeout-ms"),
            OsString::from("900000"),
            OsString::from("--max-task-stdout-tail-bytes"),
            OsString::from("65536"),
            OsString::from("--max-task-stderr-tail-bytes"),
            OsString::from("65536"),
        ]);
        args
    }

    fn parse_with_deployment_policy(args: Vec<OsString>) -> taskcaged::Result<DaemonConfig> {
        parse(with_deployment_policy(args))
    }

    #[test]
    fn serve_requires_an_explicit_absolute_socket_and_internal_limits() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let config = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            socket.into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("2"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("8"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap();
        assert!(format!("{config:?}").contains("max_concurrent_tasks: 2"));
        assert!(format!("{config:?}").contains("max_registry_tasks: 8"));
        assert!(format!("{config:?}").contains("max_concurrent_connections: 8"));

        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            OsString::from("relative.sock"),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("2"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("8"),
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
        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("0"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("8"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("0보다 커야"));

        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("8"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("cleanup-timeout-ms 옵션은 필수"));

        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("8"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("0"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("fail-stop timeout은 0보다 커야"));

        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            socket.into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("8"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
        ])
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("fail-stop-timeout-ms 옵션은 필수")
        );

        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            std::env::temp_dir().join("taskcaged.sock").into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("0"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("max-concurrent-connections"));

        let error = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            std::env::temp_dir().join("taskcaged.sock").into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ])
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("max-concurrent-connections 옵션은 필수")
        );
    }

    #[test]
    fn serve_rejects_invalid_or_duplicate_connection_limit() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let common_tail = [
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("5000"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("10000"),
        ];
        let mut invalid = vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("many"),
        ];
        invalid.extend(common_tail.iter().cloned());
        let error = parse_with_deployment_policy(invalid).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("잘못된 --max-concurrent-connections 값")
        );

        let mut duplicate = vec![
            OsString::from("--socket"),
            socket.into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("8"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("2"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("3"),
        ];
        duplicate.extend(common_tail);
        let error = parse_with_deployment_policy(duplicate).unwrap_err();
        assert!(error.to_string().contains("serve 옵션이 중복되었습니다"));
        assert!(error.to_string().contains("--max-concurrent-connections"));
    }

    #[test]
    fn serve_accepts_one_explicit_metrics_listener() {
        let socket = std::env::temp_dir().join("taskcaged-metrics.sock");
        let mut arguments = vec![
            OsString::from("--socket"),
            socket.into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("1"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("1"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("1"),
            OsString::from("--metrics-listen"),
            OsString::from("127.0.0.1:9098"),
        ];
        let config = parse_with_deployment_policy(arguments.clone()).unwrap();
        assert!(format!("{config:?}").contains("metrics_listen: Some(127.0.0.1:9098)"));

        arguments.extend([
            OsString::from("--metrics-listen"),
            OsString::from("127.0.0.1:9099"),
        ]);
        let error = parse_with_deployment_policy(arguments).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("serve 옵션이 중복되었습니다: --metrics-listen")
        );
    }

    #[test]
    fn serve_requires_a_positive_registry_limit_at_least_the_execution_limit() {
        let socket = std::env::temp_dir().join("taskcaged.sock");
        let make_args = |registry_limit: Option<&str>| {
            let mut args = vec![
                OsString::from("--socket"),
                socket.clone().into_os_string(),
                OsString::from("--max-concurrent-tasks"),
                OsString::from("2"),
            ];
            if let Some(limit) = registry_limit {
                args.extend([
                    OsString::from("--max-registry-tasks"),
                    OsString::from(limit),
                ]);
            }
            args.extend([
                OsString::from("--max-concurrent-connections"),
                OsString::from("8"),
                OsString::from("--cleanup-timeout-ms"),
                OsString::from("5000"),
                OsString::from("--fail-stop-timeout-ms"),
                OsString::from("10000"),
            ]);
            args
        };

        assert!(
            parse_with_deployment_policy(make_args(None))
                .unwrap_err()
                .to_string()
                .contains("max-registry-tasks 옵션은 필수")
        );
        assert!(
            parse_with_deployment_policy(make_args(Some("0")))
                .unwrap_err()
                .to_string()
                .contains("max-registry-tasks 값은 0보다 커야")
        );
        assert!(
            parse_with_deployment_policy(make_args(Some("1")))
                .unwrap_err()
                .to_string()
                .contains("max-concurrent-tasks 이상")
        );
        assert!(parse_with_deployment_policy(make_args(Some("2"))).is_ok());

        assert!(
            parse_with_deployment_policy(make_args(Some("many")))
                .unwrap_err()
                .to_string()
                .contains("잘못된 --max-registry-tasks 값")
        );

        let large = usize::MAX.to_string();
        let large_config = parse_with_deployment_policy(make_args(Some(&large))).unwrap();
        assert!(format!("{large_config:?}").contains(&format!("max_registry_tasks: {large}")));

        let minimum = parse_with_deployment_policy(vec![
            OsString::from("--socket"),
            socket.clone().into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("1"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("1"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("1"),
        ])
        .unwrap();
        assert!(format!("{minimum:?}").contains("max_registry_tasks: 1"));

        let mut duplicate = make_args(Some("2"));
        duplicate.extend([OsString::from("--max-registry-tasks"), OsString::from("3")]);
        let duplicate_error = parse_with_deployment_policy(duplicate).unwrap_err();
        assert!(
            duplicate_error
                .to_string()
                .contains("serve 옵션이 중복되었습니다: --max-registry-tasks")
        );
    }

    #[test]
    fn serve_requires_a_valid_explicit_deployment_policy() {
        let base = vec![
            OsString::from("--socket"),
            std::env::temp_dir().join("taskcaged.sock").into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("1"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("1"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("1"),
        ];
        let missing = parse(base.clone()).unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("max-task-cpu-quota-us 옵션은 필수")
        );

        let mut invalid = with_deployment_policy(base);
        let value_index = invalid
            .iter()
            .position(|value| value == "--max-task-memory-bytes")
            .unwrap()
            + 1;
        invalid[value_index] = OsString::from("0");
        let error = parse(invalid).unwrap_err();
        assert!(error.to_string().contains("deployment resource policy"));
        assert!(error.to_string().contains("0보다 커야"));
    }

    #[test]
    fn serve_enables_file_copy_only_with_a_complete_artifact_configuration() {
        let base = vec![
            OsString::from("--socket"),
            std::env::temp_dir().join("taskcaged.sock").into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("1"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("1"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("1"),
        ];
        let mut root_only = base.clone();
        root_only.extend([
            OsString::from("--profile-artifact-root"),
            std::env::temp_dir().into_os_string(),
        ]);
        let error = parse(with_deployment_policy(root_only)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("artifact-root와 --profile-artifact-max-bytes"),
            "unexpected error: {error}"
        );

        let mut complete = base;
        complete.extend([
            OsString::from("--profile-artifact-root"),
            std::env::temp_dir().into_os_string(),
            OsString::from("--profile-artifact-max-bytes"),
            OsString::from("1024"),
        ]);
        let config =
            parse(with_deployment_policy(complete)).expect("complete Profile configuration");
        assert!(format!("{config:?}").contains("local_profile: Some"));
    }

    #[test]
    fn serve_requires_complete_ffmpeg_runtime_package_registration() {
        let base = || {
            with_deployment_policy(vec![
                OsString::from("--socket"),
                std::env::temp_dir()
                    .join("taskcaged-ffmpeg.sock")
                    .into_os_string(),
                OsString::from("--max-concurrent-tasks"),
                OsString::from("1"),
                OsString::from("--max-registry-tasks"),
                OsString::from("1"),
                OsString::from("--max-concurrent-connections"),
                OsString::from("1"),
                OsString::from("--cleanup-timeout-ms"),
                OsString::from("1"),
                OsString::from("--fail-stop-timeout-ms"),
                OsString::from("1"),
                OsString::from("--profile-artifact-root"),
                std::env::temp_dir().into_os_string(),
                OsString::from("--profile-artifact-max-bytes"),
                OsString::from("1024"),
            ])
        };
        let digest = format!("sha256:{}", "a".repeat(64));
        let cache_root = std::env::temp_dir().join("taskcage-runtime-package-cache");

        let mut no_artifacts = with_deployment_policy(vec![
            OsString::from("--socket"),
            std::env::temp_dir()
                .join("taskcaged-ffmpeg-no-artifacts.sock")
                .into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("1"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("1"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("1"),
        ]);
        no_artifacts.extend([
            OsString::from("--runtime-package-cache-root"),
            cache_root.clone().into_os_string(),
            OsString::from("--ffmpeg-audio-to-wav-package-digest"),
            OsString::from(&digest),
        ]);
        assert!(
            parse(no_artifacts)
                .unwrap_err()
                .to_string()
                .contains("Artifact 설정")
        );

        let mut root_only = base();
        root_only.extend([
            OsString::from("--runtime-package-cache-root"),
            cache_root.clone().into_os_string(),
        ]);
        assert!(
            parse(root_only)
                .unwrap_err()
                .to_string()
                .contains("함께 지정")
        );

        let mut digest_only = base();
        digest_only.extend([
            OsString::from("--ffmpeg-audio-to-wav-package-digest"),
            OsString::from(&digest),
        ]);
        assert!(
            parse(digest_only)
                .unwrap_err()
                .to_string()
                .contains("함께 지정")
        );

        let mut invalid_digest = base();
        invalid_digest.extend([
            OsString::from("--runtime-package-cache-root"),
            cache_root.clone().into_os_string(),
            OsString::from("--ffmpeg-audio-to-wav-package-digest"),
            OsString::from("sha256:not-canonical"),
        ]);
        assert!(
            parse(invalid_digest)
                .unwrap_err()
                .to_string()
                .contains("잘못된 --ffmpeg-audio-to-wav-package-digest")
        );

        let mut complete = base();
        complete.extend([
            OsString::from("--runtime-package-cache-root"),
            cache_root.into_os_string(),
            OsString::from("--ffmpeg-audio-to-wav-package-digest"),
            OsString::from(digest),
        ]);
        let config = parse(complete).expect("complete FFmpeg registration");
        assert!(format!("{config:?}").contains("ffmpeg_audio_to_wav: Some"));
    }

    #[test]
    fn serve_requires_artifact_configuration_before_enabling_bundle_catalog() {
        let args = with_deployment_policy(vec![
            OsString::from("--socket"),
            std::env::temp_dir()
                .join("taskcaged-bundle.sock")
                .into_os_string(),
            OsString::from("--max-concurrent-tasks"),
            OsString::from("1"),
            OsString::from("--max-registry-tasks"),
            OsString::from("1"),
            OsString::from("--max-concurrent-connections"),
            OsString::from("1"),
            OsString::from("--cleanup-timeout-ms"),
            OsString::from("1"),
            OsString::from("--fail-stop-timeout-ms"),
            OsString::from("1"),
            OsString::from("--bundle-cache-root"),
            std::env::temp_dir().into_os_string(),
        ]);
        let error = parse(args).unwrap_err();
        assert!(error.to_string().contains("Bundle Profile catalog"));
    }
}
