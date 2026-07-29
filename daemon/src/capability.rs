//! cgroup 사전 검사 결과를 protocol v1 capability와 submit 차단 조건으로 바꾼다.

use crate::capacity::TaskCapacitySettings;
use crate::preflight::{CapabilityProbe, PreflightError, VerifiedEnvironment};
use crate::protocol::{CapabilitiesPayload, ErrorCode, MAX_FRAME_BYTES, PROTOCOL_VERSION};

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "UDS handler가 다음 단계에서 사전 검사 결과를 보관합니다"
    )
)]
#[derive(Debug)]
enum CgroupReadiness {
    Ready,
    Unavailable(PreflightError),
}

/// 같은 사전 검사 결과로 capability 응답과 submit 실행 여부를 결정한다.
#[derive(Debug)]
pub struct CapabilityAdapter {
    capacity_settings: TaskCapacitySettings,
    cgroup_readiness: CgroupReadiness,
}

/// 한 번의 preflight 성공 토큰을 capability 상태와 실행 코어에 함께 연결한다.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "protocol handler는 Linux 실행 경로에서 성공 토큰을 소비합니다"
    )
)]
#[derive(Debug)]
pub(crate) enum CapabilityInitialization {
    Ready {
        adapter: CapabilityAdapter,
        environment: VerifiedEnvironment,
    },
    Unavailable {
        adapter: CapabilityAdapter,
    },
}

impl CapabilityInitialization {
    #[cfg(test)]
    fn adapter(&self) -> &CapabilityAdapter {
        match self {
            Self::Ready { adapter, .. } | Self::Unavailable { adapter } => adapter,
        }
    }
}

impl CapabilityAdapter {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "UDS handler가 다음 단계에서 capability와 submit에 같은 설정을 전달합니다"
        )
    )]
    pub(crate) fn from_probe<P>(
        probe: &P,
        capacity_settings: TaskCapacitySettings,
    ) -> CapabilityInitialization
    where
        P: CapabilityProbe,
    {
        Self::from_preflight(probe.check(), capacity_settings)
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "UDS handler가 다음 단계에서 사전 검사 결과를 보관합니다"
        )
    )]
    pub(crate) fn from_preflight(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        capacity_settings: TaskCapacitySettings,
    ) -> CapabilityInitialization {
        match preflight {
            Ok(environment) => CapabilityInitialization::Ready {
                adapter: Self {
                    capacity_settings,
                    cgroup_readiness: CgroupReadiness::Ready,
                },
                environment,
            },
            Err(error) => CapabilityInitialization::Unavailable {
                adapter: Self {
                    capacity_settings,
                    cgroup_readiness: CgroupReadiness::Unavailable(error),
                },
            },
        }
    }

    pub fn payload(&self) -> CapabilitiesPayload {
        CapabilitiesPayload {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_versions: vec![PROTOCOL_VERSION],
            max_frame_bytes: MAX_FRAME_BYTES as u32,
            max_concurrent_tasks: self.capacity_settings.max_concurrent_tasks(),
            cgroup_v2_ready: matches!(&self.cgroup_readiness, CgroupReadiness::Ready),
        }
    }

    /// 준비 성공 토큰이 없으면 작업 생성 전에 거절한다.
    pub fn submit_gate(&self) -> Result<(), ErrorCode> {
        match &self.cgroup_readiness {
            CgroupReadiness::Ready => Ok(()),
            CgroupReadiness::Unavailable(_) => Err(ErrorCode::EnvironmentUnavailable),
        }
    }

    /// wire 응답에는 넣지 않는 사전 검사 진단이다.
    pub fn unavailable_diagnostic(&self) -> Option<&PreflightError> {
        match &self.cgroup_readiness {
            CgroupReadiness::Ready => None,
            CgroupReadiness::Unavailable(diagnostic) => Some(diagnostic),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::Value;

    use super::*;
    use crate::capacity::TaskCapacity;

    struct ReadyProbe;

    impl CapabilityProbe for ReadyProbe {
        fn check(&self) -> Result<VerifiedEnvironment, PreflightError> {
            Ok(VerifiedEnvironment::for_test())
        }
    }

    struct UnavailableProbe;

    impl CapabilityProbe for UnavailableProbe {
        fn check(&self) -> Result<VerifiedEnvironment, PreflightError> {
            Err(PreflightError::MissingController {
                controller: "pids".to_owned(),
                path: PathBuf::from("/delegated"),
            })
        }
    }

    #[test]
    fn ready_probe_produces_only_the_public_capability_fields() {
        let initialization =
            CapabilityAdapter::from_probe(&ReadyProbe, TaskCapacitySettings::new(7).unwrap());
        let adapter = initialization.adapter();

        let payload = adapter.payload();
        let value = serde_json::to_value(&payload).unwrap();
        let Value::Object(fields) = value else {
            panic!("capability payload must be an object");
        };

        let mut names: Vec<_> = fields.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "cgroupV2Ready",
                "daemonVersion",
                "maxConcurrentTasks",
                "maxFrameBytes",
                "protocolVersions",
            ]
        );
        assert_eq!(payload.daemon_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(payload.protocol_versions, [PROTOCOL_VERSION]);
        assert_eq!(payload.max_frame_bytes as usize, MAX_FRAME_BYTES);
        assert_eq!(payload.max_concurrent_tasks, 7);
        assert!(payload.cgroup_v2_ready);
        assert!(adapter.unavailable_diagnostic().is_none());

        adapter.submit_gate().expect("ready gate");
        let CapabilityInitialization::Ready { environment, .. } = initialization else {
            panic!("준비된 probe는 검증된 환경을 실행 코어에 넘겨야 합니다");
        };
        assert_eq!(
            environment.report().delegated_root,
            PathBuf::from("/delegated")
        );
    }

    #[test]
    fn unavailable_probe_rejects_submit_before_any_side_effect() {
        let initialization =
            CapabilityAdapter::from_probe(&UnavailableProbe, TaskCapacitySettings::new(7).unwrap());
        let adapter = initialization.adapter();
        let cgroup_creations = Cell::new(0);
        let target_starts = Cell::new(0);

        let result = adapter.submit_gate().map(|_| {
            cgroup_creations.set(cgroup_creations.get() + 1);
            target_starts.set(target_starts.get() + 1);
        });

        assert_eq!(result, Err(ErrorCode::EnvironmentUnavailable));
        assert_eq!(cgroup_creations.get(), 0);
        assert_eq!(target_starts.get(), 0);
        assert!(!adapter.payload().cgroup_v2_ready);
        assert!(matches!(
            adapter.unavailable_diagnostic(),
            Some(PreflightError::MissingController { controller, .. }) if controller == "pids"
        ));
        assert_eq!(
            serde_json::to_string(&ErrorCode::EnvironmentUnavailable).unwrap(),
            r#""ENVIRONMENT_UNAVAILABLE""#
        );
    }

    #[test]
    fn reported_maximum_and_actual_permits_share_one_setting() {
        let settings = TaskCapacitySettings::new(3).unwrap();
        let initialization = CapabilityAdapter::from_probe(&ReadyProbe, settings);
        let adapter = initialization.adapter();
        let capacity = Arc::new(TaskCapacity::new(settings));
        let permits: Vec<_> = (0..3)
            .map(|_| {
                capacity
                    .try_acquire()
                    .expect("설정한 수만큼 슬롯이 있어야 합니다")
            })
            .collect();

        assert_eq!(adapter.payload().max_concurrent_tasks, 3);
        assert_eq!(
            capacity.settings().max_concurrent_tasks(),
            adapter.payload().max_concurrent_tasks
        );
        assert!(capacity.try_acquire().is_none());
        drop(permits);
    }
}
