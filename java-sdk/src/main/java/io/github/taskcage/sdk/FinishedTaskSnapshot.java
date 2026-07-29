package io.github.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** A task whose process and cgroup cleanup have completed. */
public record FinishedTaskSnapshot(UUID taskId, ExecutionResult result) implements TaskSnapshot, TaskSubmission {
    public FinishedTaskSnapshot {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(result, "result");
    }

    @Override
    public TaskState state() {
        return TaskState.FINISHED;
    }
}
