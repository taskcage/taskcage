//! Backend-independent Task lifecycle snapshots and results.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    Exited,
    ExecutionFailed,
    Cancelled,
    TimedOut,
    MemoryLimitExceeded,
    ProcessLimitExceeded,
    DaemonError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTiming {
    pub submitted_at: String,
    pub started_at: String,
    pub finished_at: String,
    pub wall_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskUsage {
    pub cpu_time_micros: u64,
    pub memory_peak_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutput {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Cleanup-confirmed terminal data for one Task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskResult {
    pub task_id: String,
    pub termination_reason: TerminationReason,
    pub process: ProcessResult,
    pub timing: TaskTiming,
    pub usage: TaskUsage,
    pub output: TaskOutput,
}

/// A Task is FINISHED only after the supervising adapter has confirmed cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSnapshot {
    Running {
        task_id: String,
        submitted_at: String,
        started_at: String,
    },
    Finished {
        task_id: String,
        termination_reason: TerminationReason,
        process: ProcessResult,
        timing: TaskTiming,
        usage: TaskUsage,
        output: TaskOutput,
    },
}

impl TaskSnapshot {
    pub const fn state(&self) -> TaskState {
        match self {
            Self::Running { .. } => TaskState::Running,
            Self::Finished { .. } => TaskState::Finished,
        }
    }

    pub fn task_id(&self) -> &str {
        match self {
            Self::Running { task_id, .. } | Self::Finished { task_id, .. } => task_id,
        }
    }

    pub fn from_result(result: TaskResult) -> Self {
        Self::Finished {
            task_id: result.task_id,
            termination_reason: result.termination_reason,
            process: result.process,
            timing: result.timing,
            usage: result.usage,
            output: result.output,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_state_is_explicitly_derived_from_a_terminal_result() {
        let snapshot = TaskSnapshot::from_result(TaskResult {
            task_id: "task-1".to_owned(),
            termination_reason: TerminationReason::Exited,
            process: ProcessResult {
                exit_code: Some(0),
                signal: None,
            },
            timing: TaskTiming {
                submitted_at: "submitted".to_owned(),
                started_at: "started".to_owned(),
                finished_at: "finished".to_owned(),
                wall_time_ms: 1,
            },
            usage: TaskUsage {
                cpu_time_micros: 0,
                memory_peak_bytes: 0,
            },
            output: TaskOutput {
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        });

        assert_eq!(snapshot.state(), TaskState::Finished);
        assert_eq!(snapshot.task_id(), "task-1");
    }
}
