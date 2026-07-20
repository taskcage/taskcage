#[cfg(unix)]
mod unix_tests {
    use std::io;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use taskcaged::preflight::{
        CapabilityProbe, CapabilityReport, PreflightError, SystemProbe, with_verified_environment,
    };

    static MARKER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    enum SimulatedFailure {
        MissingController,
        WriteDenied,
        AtomicEntryUnavailable,
    }

    struct FailingProbe(SimulatedFailure);

    impl CapabilityProbe for FailingProbe {
        fn check(&self) -> Result<CapabilityReport, PreflightError> {
            match self.0 {
                SimulatedFailure::MissingController => Err(PreflightError::MissingController {
                    controller: "pids".to_owned(),
                    path: PathBuf::from("/delegated"),
                }),
                SimulatedFailure::WriteDenied => Err(PreflightError::NotWritable {
                    path: PathBuf::from("/delegated/cgroup.procs"),
                    source: io::Error::new(io::ErrorKind::PermissionDenied, "시험용 권한 거부"),
                }),
                SimulatedFailure::AtomicEntryUnavailable => {
                    Err(PreflightError::AtomicEntryUnsupported {
                        source: io::Error::from_raw_os_error(libc::ENOSYS),
                    })
                }
            }
        }
    }

    fn marker_path(case: &str) -> PathBuf {
        let sequence = MARKER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "taskcage-preflight-{case}-{}-{sequence}",
            std::process::id()
        ))
    }

    fn assert_target_not_started<P: CapabilityProbe>(probe: &P, case: &str) {
        let marker = marker_path(case);
        let result = with_verified_environment(probe, |_| {
            // 이 외부 프로그램은 검사가 성공했을 때만 호출되어야 한다.
            Command::new("touch").arg(&marker).status()
        });

        assert!(result.is_err());
        assert!(!marker.exists(), "사전 검사 실패 뒤 target이 실행됐습니다");
    }

    #[test]
    fn simulated_failures_never_start_the_target() {
        assert_target_not_started(
            &FailingProbe(SimulatedFailure::MissingController),
            "controller",
        );
        assert_target_not_started(&FailingProbe(SimulatedFailure::WriteDenied), "permission");
        assert_target_not_started(
            &FailingProbe(SimulatedFailure::AtomicEntryUnavailable),
            "atomic-entry",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_cgroup_root_never_starts_the_target() {
        // 일반 임시 디렉터리는 cgroup v2가 아니므로 어떠한 제어 파일도 만들기 전에 실패한다.
        assert_target_not_started(&SystemProbe::with_root(std::env::temp_dir()), "wrong-root");
    }
}
