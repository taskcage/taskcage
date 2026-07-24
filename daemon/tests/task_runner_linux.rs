#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, Instant};

use taskcaged::preflight::{CapabilityProbe, SystemProbe};
use taskcaged::protocol::{
    CommandSpec, CpuMax, OutputLimits, ProcessResult, ResourceLimits, TaskOutput, TaskPayload,
    TaskUsage, TerminationReason,
};
use taskcaged::resource_budget::ResourceBudget;
use taskcaged::{CompletedTask, TaskRunConfig, TaskRunner};

fn budget(wall_time_limit_ms: u64) -> ResourceBudget {
    ResourceBudget::try_from_protocol(
        ResourceLimits {
            cpu_max: CpuMax {
                quota_micros: 50_000,
                period_micros: 100_000,
            },
            memory_max_bytes: 64 * 1024 * 1024,
            pids_max: 8,
            wall_time_limit_ms,
        },
        OutputLimits {
            stdout_tail_max_bytes: 1_024,
            stderr_tail_max_bytes: 1_024,
        },
    )
    .unwrap()
}

fn config(task_id: &str, program: &str, args: &[&str], timeout_ms: u64) -> TaskRunConfig {
    TaskRunConfig {
        task_id: task_id.to_owned(),
        submitted_at: "2026-07-24T10:00:00.000Z".to_owned(),
        started_at: "2026-07-24T10:00:00.010Z".to_owned(),
        started_monotonic: Instant::now(),
        cleanup_timeout: Duration::from_secs(5),
        command: CommandSpec {
            program: program.to_owned(),
            args: args.iter().map(|value| (*value).to_owned()).collect(),
            working_directory: "/".to_owned(),
            environment: BTreeMap::new(),
        },
        budget: budget(timeout_ms),
    }
}

fn assert_running(payload: TaskPayload, task_id: &str) {
    assert!(matches!(
        payload,
        TaskPayload::Running { task_id: actual, .. } if actual == task_id
    ));
}

fn assert_finished(
    completed: CompletedTask,
    expected_reason: TerminationReason,
) -> (ProcessResult, TaskUsage, TaskOutput) {
    match completed.into_payload() {
        TaskPayload::Finished {
            termination_reason,
            process,
            usage,
            output,
            ..
        } => {
            assert_eq!(termination_reason, expected_reason);
            (process, usage, output)
        }
        TaskPayload::Running { .. } => panic!("정리가 끝난 FINISHED 결과가 필요합니다"),
    }
}

#[tokio::test]
async fn atomic_runner_completes_the_real_single_task_lifecycle() {
    if std::env::var_os("TASKCAGE_RUN_LINUX_INTEGRATION").is_none() {
        eprintln!("NOT EXECUTED: 실제 cgroup v2 위임 환경이 필요합니다");
        return;
    }

    let environment = SystemProbe::from_environment().check().unwrap();
    let jobs_path = environment.report().delegated_root.join("jobs");
    let runner = TaskRunner::initialize(environment).unwrap();

    let started = Instant::now();
    let (running_sender, running_receiver) = tokio::sync::oneshot::channel();
    let normal = runner
        .run_task(
            config("lifecycle-normal", "/bin/echo", &["runner-output"], 5_000),
            running_sender,
            || {
                (
                    "2026-07-24T10:00:01.000Z".to_owned(),
                    started + Duration::from_secs(1),
                )
            },
        )
        .await
        .unwrap();
    assert_running(running_receiver.await.unwrap(), "lifecycle-normal");
    let (process, usage, output) = assert_finished(normal, TerminationReason::Exited);
    assert_eq!(process.exit_code, Some(0));
    assert_eq!(process.signal, None);
    assert!(usage.memory_peak_bytes > 0);
    assert_eq!(output.stdout_tail, "runner-output\n");
    assert_eq!(output.stderr_tail, "");
    assert!(!output.stdout_truncated);
    assert!(!output.stderr_truncated);

    let started = Instant::now();
    let (running_sender, running_receiver) = tokio::sync::oneshot::channel();
    let nonzero = runner
        .run_task(
            config("lifecycle-nonzero", "/bin/false", &[], 5_000),
            running_sender,
            || {
                (
                    "2026-07-24T10:00:01.000Z".to_owned(),
                    started + Duration::from_secs(1),
                )
            },
        )
        .await
        .unwrap();
    assert_running(running_receiver.await.unwrap(), "lifecycle-nonzero");
    let (process, _, _) = assert_finished(nonzero, TerminationReason::Exited);
    assert_eq!(process.exit_code, Some(1));

    let started = Instant::now();
    let (running_sender, running_receiver) = tokio::sync::oneshot::channel();
    let timed_out = runner
        .run_task(
            config("lifecycle-timeout", "/bin/sleep", &["30"], 100),
            running_sender,
            || {
                (
                    "2026-07-24T10:00:01.000Z".to_owned(),
                    started + Duration::from_secs(1),
                )
            },
        )
        .await
        .unwrap();
    assert_running(running_receiver.await.unwrap(), "lifecycle-timeout");
    let (process, _, _) = assert_finished(timed_out, TerminationReason::TimedOut);
    assert_eq!(process.exit_code, None);
    assert_eq!(process.signal.as_deref(), Some("SIGKILL"));

    let started = Instant::now();
    let (running_sender, running_receiver) = tokio::sync::oneshot::channel();
    let exec_failed = runner
        .run_task(
            config(
                "lifecycle-exec-failed",
                "/definitely/missing/taskcage-target",
                &[],
                5_000,
            ),
            running_sender,
            || {
                (
                    "2026-07-24T10:00:01.000Z".to_owned(),
                    started + Duration::from_secs(1),
                )
            },
        )
        .await
        .unwrap();
    assert!(running_receiver.await.is_err());
    let (process, _, output) = assert_finished(exec_failed, TerminationReason::ExecutionFailed);
    assert_eq!(process.exit_code, None);
    assert_eq!(process.signal, None);
    assert_eq!(output.stdout_tail, "");
    assert_eq!(output.stderr_tail, "");

    assert!(!runner.cleanup_is_uncertain());
    let remaining_jobs = fs::read_dir(jobs_path)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
        .count();
    assert_eq!(remaining_jobs, 0, "작업 cgroup이 남아 있습니다");
}
