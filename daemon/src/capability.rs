//! cgroup 사전 검사 결과를 protocol v1 capability와 submit 차단 조건으로 바꾼다.

use crate::preflight::{CapabilityProbe, PreflightError, VerifiedEnvironment};
use crate::protocol::{CapabilitiesPayload, ErrorCode, MAX_FRAME_BYTES, PROTOCOL_VERSION};

#[derive(Debug)]
enum CgroupReadiness {
    Ready(VerifiedEnvironment),
    Unavailable(PreflightError),
}

/// 같은 사전 검사 결과로 capability 응답과 submit 실행 여부를 결정한다.
#[derive(Debug)]
pub struct CapabilityAdapter {
    max_concurrent_tasks: u32,
    cgroup_readiness: CgroupReadiness,
}

impl CapabilityAdapter {
    pub fn from_probe<P>(probe: &P, max_concurrent_tasks: u32) -> Self
    where
        P: CapabilityProbe,
    {
        Self::from_preflight(probe.check(), max_concurrent_tasks)
    }

    pub fn from_preflight(
        preflight: Result<VerifiedEnvironment, PreflightError>,
        max_concurrent_tasks: u32,
    ) -> Self {
        let cgroup_readiness = match preflight {
            Ok(environment) => CgroupReadiness::Ready(environment),
            Err(error) => CgroupReadiness::Unavailable(error),
        };

        Self {
            max_concurrent_tasks,
            cgroup_readiness,
        }
    }

    pub fn payload(&self) -> CapabilitiesPayload {
        CapabilitiesPayload {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_versions: vec![PROTOCOL_VERSION],
            max_frame_bytes: MAX_FRAME_BYTES as u32,
            max_concurrent_tasks: self.max_concurrent_tasks,
            cgroup_v2_ready: matches!(&self.cgroup_readiness, CgroupReadiness::Ready(_)),
        }
    }

    /// 준비 성공 토큰이 없으면 작업 생성 전에 거절한다.
    pub fn submit_gate(&self) -> Result<&VerifiedEnvironment, ErrorCode> {
        match &self.cgroup_readiness {
            CgroupReadiness::Ready(environment) => Ok(environment),
            CgroupReadiness::Unavailable(_) => Err(ErrorCode::EnvironmentUnavailable),
        }
    }

    /// wire 응답에는 넣지 않는 사전 검사 진단이다.
    pub fn unavailable_diagnostic(&self) -> Option<&PreflightError> {
        match &self.cgroup_readiness {
            CgroupReadiness::Ready(_) => None,
            CgroupReadiness::Unavailable(diagnostic) => Some(diagnostic),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;

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
        let adapter = CapabilityAdapter::from_probe(&ReadyProbe, 7);

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

        let environment = adapter.submit_gate().expect("ready gate");
        assert_eq!(
            environment.report().delegated_root,
            PathBuf::from("/delegated")
        );
    }

    #[test]
    fn unavailable_probe_rejects_submit_before_any_side_effect() {
        let adapter = CapabilityAdapter::from_probe(&UnavailableProbe, 7);
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
}
