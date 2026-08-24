//! 낮은 cardinality의 daemon-wide Prometheus runtime metrics다.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "linux")]
use std::sync::Arc;

use taskcage_core::task::{TaskSnapshot, TerminationReason};

const DURATION_BUCKETS_SECONDS: [f64; 8] = [0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 30.0, 60.0];
const TERMINATION_REASONS: [TerminationReason; 7] = [
    TerminationReason::Exited,
    TerminationReason::ExecutionFailed,
    TerminationReason::Cancelled,
    TerminationReason::TimedOut,
    TerminationReason::MemoryLimitExceeded,
    TerminationReason::ProcessLimitExceeded,
    TerminationReason::DaemonError,
];

#[derive(Debug, Default)]
pub(crate) struct RuntimeMetrics {
    tasks_started: AtomicU64,
    tasks_finished: [AtomicU64; 7],
    cleanup_verified: AtomicU64,
    duration_count: AtomicU64,
    duration_sum_millis: AtomicU64,
    duration_buckets: [AtomicU64; DURATION_BUCKETS_SECONDS.len()],
}

impl RuntimeMetrics {
    pub(crate) fn task_started(&self) {
        self.tasks_started.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn task_finished(&self, snapshot: &TaskSnapshot) {
        let TaskSnapshot::Finished {
            termination_reason,
            timing,
            ..
        } = snapshot
        else {
            return;
        };
        self.tasks_finished[outcome_index(*termination_reason)].fetch_add(1, Ordering::Relaxed);
        self.cleanup_verified.fetch_add(1, Ordering::Relaxed);
        self.duration_count.fetch_add(1, Ordering::Relaxed);
        self.duration_sum_millis
            .fetch_add(timing.wall_time_ms, Ordering::Relaxed);
        let seconds = timing.wall_time_ms as f64 / 1_000.0;
        for (index, bound) in DURATION_BUCKETS_SECONDS.iter().enumerate() {
            if seconds <= *bound {
                self.duration_buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn render(&self, running_tasks: u32, capacity_slots: u32) -> String {
        let mut output = String::new();
        output.push_str("# HELP taskcage_up Whether the TaskCage daemon is ready to serve metrics.\n# TYPE taskcage_up gauge\ntaskcage_up 1\n");
        output.push_str("# HELP taskcage_running_tasks Task execution slots currently in use.\n# TYPE taskcage_running_tasks gauge\n");
        let _ = writeln!(output, "taskcage_running_tasks {running_tasks}");
        output.push_str("# HELP taskcage_capacity_slots Configured task execution slots.\n# TYPE taskcage_capacity_slots gauge\n");
        let _ = writeln!(output, "taskcage_capacity_slots {capacity_slots}");
        output.push_str("# HELP taskcage_tasks_started_total Tasks admitted for execution.\n# TYPE taskcage_tasks_started_total counter\n");
        let _ = writeln!(
            output,
            "taskcage_tasks_started_total {}",
            self.tasks_started.load(Ordering::Relaxed)
        );
        output.push_str("# HELP taskcage_tasks_finished_total Cleanup-confirmed task completions by outcome.\n# TYPE taskcage_tasks_finished_total counter\n");
        for reason in TERMINATION_REASONS {
            let _ = writeln!(
                output,
                "taskcage_tasks_finished_total{{outcome=\"{}\"}} {}",
                outcome_name(reason),
                self.tasks_finished[outcome_index(reason)].load(Ordering::Relaxed)
            );
        }
        output.push_str("# HELP taskcage_cleanup_total Cleanup-confirmed task completions.\n# TYPE taskcage_cleanup_total counter\n");
        let _ = writeln!(
            output,
            "taskcage_cleanup_total{{result=\"verified\"}} {}",
            self.cleanup_verified.load(Ordering::Relaxed)
        );
        output.push_str("# HELP taskcage_task_duration_seconds Task wall-clock duration after execution admission.\n# TYPE taskcage_task_duration_seconds histogram\n");
        for (index, bound) in DURATION_BUCKETS_SECONDS.iter().enumerate() {
            let _ = writeln!(
                output,
                "taskcage_task_duration_seconds_bucket{{le=\"{bound}\"}} {}",
                self.duration_buckets[index].load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            output,
            "taskcage_task_duration_seconds_bucket{{le=\"+Inf\"}} {}",
            self.duration_count.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            output,
            "taskcage_task_duration_seconds_sum {:.3}",
            self.duration_sum_millis.load(Ordering::Relaxed) as f64 / 1_000.0
        );
        let _ = writeln!(
            output,
            "taskcage_task_duration_seconds_count {}",
            self.duration_count.load(Ordering::Relaxed)
        );
        output
    }
}

fn outcome_index(reason: TerminationReason) -> usize {
    match reason {
        TerminationReason::Exited => 0,
        TerminationReason::ExecutionFailed => 1,
        TerminationReason::Cancelled => 2,
        TerminationReason::TimedOut => 3,
        TerminationReason::MemoryLimitExceeded => 4,
        TerminationReason::ProcessLimitExceeded => 5,
        TerminationReason::DaemonError => 6,
    }
}

fn outcome_name(reason: TerminationReason) -> &'static str {
    match reason {
        TerminationReason::Exited => "exited",
        TerminationReason::ExecutionFailed => "execution_failed",
        TerminationReason::Cancelled => "cancelled",
        TerminationReason::TimedOut => "timed_out",
        TerminationReason::MemoryLimitExceeded => "memory_limit_exceeded",
        TerminationReason::ProcessLimitExceeded => "process_limit_exceeded",
        TerminationReason::DaemonError => "daemon_error",
    }
}

#[cfg(target_os = "linux")]
pub(crate) async fn serve(
    listener: tokio::net::TcpListener,
    handlers: Arc<crate::handlers::ProtocolHandlers<crate::adapters::task_service::TaskService>>,
) -> Result<(), std::io::Error> {
    loop {
        let (stream, _) = listener.accept().await?;
        let handlers = Arc::clone(&handlers);
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                write_response(stream, handlers.render_metrics()),
            )
            .await;
        });
    }
}

#[cfg(any(target_os = "linux", test))]
async fn write_response(
    mut stream: tokio::net::TcpStream,
    body: String,
) -> Result<(), std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut request = [0_u8; 1024];
    let bytes = stream.read(&mut request).await?;
    let is_metrics = request[..bytes].starts_with(b"GET /metrics ")
        || request[..bytes].starts_with(b"GET /metrics?");
    let (status, response) = if is_metrics {
        ("200 OK", body)
    } else {
        ("404 Not Found", "not found\n".to_owned())
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use taskcage_core::task::{ProcessResult, TaskOutput, TaskResult, TaskTiming, TaskUsage};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn renders_low_cardinality_lifecycle_metrics() {
        let metrics = RuntimeMetrics::default();
        metrics.task_started();
        metrics.task_finished(&TaskSnapshot::from_result(TaskResult {
            task_id: "must-not-appear".to_owned(),
            termination_reason: TerminationReason::TimedOut,
            process: ProcessResult {
                exit_code: None,
                signal: Some("SIGKILL".to_owned()),
            },
            timing: TaskTiming {
                submitted_at: "a".to_owned(),
                started_at: "b".to_owned(),
                finished_at: "c".to_owned(),
                wall_time_ms: 120,
            },
            usage: TaskUsage {
                cpu_time_micros: 1,
                memory_peak_bytes: 1,
            },
            output: TaskOutput {
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        }));
        let rendered = metrics.render(1, 4);
        assert!(rendered.contains("taskcage_running_tasks 1"));
        assert!(rendered.contains("taskcage_tasks_finished_total{outcome=\"timed_out\"} 1"));
        assert!(rendered.contains("taskcage_cleanup_total{result=\"verified\"} 1"));
        assert!(!rendered.contains("must-not-appear"));
    }

    #[tokio::test]
    async fn serves_prometheus_text_from_the_metrics_path() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            write_response(stream, "taskcage_up 1\n".to_owned())
                .await
                .unwrap();
        });

        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        server.await.unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/plain; version=0.0.4"));
        assert!(response.ends_with("taskcage_up 1\n"));
    }
}
