#[cfg(target_os = "linux")]
use std::sync::Arc;

use super::config::DaemonConfig;
#[cfg(target_os = "linux")]
use super::listeners;
#[cfg(target_os = "linux")]
use crate::application::task::TaskRegistrySettings;
#[cfg(target_os = "linux")]
use crate::capacity::TaskCapacitySettings;
#[cfg(target_os = "linux")]
use crate::fail_stop::{FailStopCoordinator, FailStopSettings};
#[cfg(target_os = "linux")]
use crate::handlers::ProtocolHandlers;
#[cfg(target_os = "linux")]
use crate::preflight::SystemProbe;
#[cfg(target_os = "linux")]
use crate::startup::StartupOwnership;
#[cfg(target_os = "linux")]
use crate::startup_cgroup::recover_from_environment;
use crate::{Error, Result};

/// 명시적 UDS 설정으로 protocol v1 daemon을 실행한다.
#[cfg(target_os = "linux")]
pub async fn run(config: DaemonConfig) -> Result<()> {
    let DaemonConfig {
        socket_path,
        max_concurrent_tasks,
        max_registry_tasks,
        max_concurrent_connections,
        cleanup_timeout,
        fail_stop_timeout,
        deployment_policy,
        local_profile,
        remote,
    } = config;
    let startup = StartupOwnership::acquire(&socket_path)
        .map_err(|error| Error::Startup(error.to_string()))?;
    let capacity_settings = TaskCapacitySettings::new(max_concurrent_tasks)
        .map_err(|error| Error::InvalidArgument(error.to_string()))?;
    let registry_settings = TaskRegistrySettings::new(max_registry_tasks)
        .map_err(|error| Error::InvalidArgument(error.to_string()))?;
    let environment = run_startup_steps(
        || match recover_from_environment(cleanup_timeout) {
            Ok(report) => {
                tracing::info!(
                    event = "startup_recovery_completed",
                    removed_jobs = report.removed_jobs,
                    "잔여 TaskCage 작업 cgroup 복구를 완료했습니다"
                );
                Ok(report)
            }
            Err(error) => {
                tracing::error!(
                    event = "startup_recovery_failed",
                    stage = error.stage(),
                    "잔여 TaskCage 작업 cgroup 복구에 실패했습니다"
                );
                Err(Error::Startup(error.to_string()))
            }
        },
        |report| {
            SystemProbe::from_environment()
                .check_after_recovery(report.placement)
                .map_err(Error::from)
        },
    )?;
    let fail_stop = FailStopCoordinator::new(
        FailStopSettings::new(fail_stop_timeout)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?,
    );
    tracing::info!(
        event = "preflight_completed",
        cgroup_v2_ready = true,
        "cgroup 사전 검사를 통과했습니다"
    );
    let local_profile_runtime = local_profile
        .as_ref()
        .map(|settings| {
            let ffmpeg_registration = settings
                .ffmpeg_audio_to_wav
                .as_ref()
                .map(|registration| (registration.cache_root.as_path(), registration.digest));
            crate::profile::LocalProfileRuntime::open(
                &settings.artifact_root,
                settings.maximum_artifact_bytes,
                deployment_policy.maximum().clone(),
                ffmpeg_registration,
                settings.bundle_cache_root.as_deref(),
            )
            .map_err(|error| {
                Error::InvalidArgument(format!("local profile 설정이 안전하지 않습니다: {error}"))
            })
        })
        .transpose()?;
    let handlers = Arc::new(ProtocolHandlers::initialize(
        Ok(environment),
        capacity_settings,
        registry_settings,
        deployment_policy,
        fail_stop,
        local_profile_runtime,
    )?);
    tracing::info!(event = "daemon_started", "TaskCage daemon started");
    let result = listeners::serve(
        startup,
        cleanup_timeout,
        max_concurrent_connections,
        local_profile.is_some(),
        remote,
        handlers,
    )
    .await;
    if result.is_ok() {
        tracing::info!(
            event = "daemon_stopped",
            outcome = "CLEAN",
            "TaskCage daemon stopped"
        );
    } else {
        tracing::error!(
            event = "daemon_stopped",
            outcome = "ERROR",
            "TaskCage daemon stopped with an error"
        );
    }
    result
}

#[cfg(any(target_os = "linux", test))]
fn run_startup_steps<R, T>(
    recover: impl FnOnce() -> Result<R>,
    preflight: impl FnOnce(R) -> Result<T>,
) -> Result<T> {
    let recovered = recover()?;
    preflight(recovered)
}

#[cfg(not(target_os = "linux"))]
pub async fn run(_config: DaemonConfig) -> Result<()> {
    Err(Error::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn startup_recovery_must_finish_before_preflight() {
        let order = RefCell::new(Vec::new());
        let result = run_startup_steps(
            || {
                order.borrow_mut().push("recovery");
                Ok(7)
            },
            |recovered| {
                assert_eq!(recovered, 7);
                order.borrow_mut().push("preflight");
                Ok(42)
            },
        )
        .unwrap();

        assert_eq!(result, 42);
        assert_eq!(order.into_inner(), ["recovery", "preflight"]);
    }

    #[test]
    fn injected_startup_recovery_failures_block_preflight_and_listener_preparation() {
        for stage in ["cgroup.kill", "populated 0", "cgroup 제거"] {
            let preflight_called = RefCell::new(false);
            let result = run_startup_steps::<(), ()>(
                || {
                    Err(Error::InvalidArgument(format!(
                        "injected startup {stage} failure"
                    )))
                },
                |_| {
                    *preflight_called.borrow_mut() = true;
                    Ok(())
                },
            );

            assert!(matches!(result, Err(Error::InvalidArgument(_))));
            assert!(!preflight_called.into_inner());
        }
    }
}
