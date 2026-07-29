package io.github.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** Confirmation that the daemon cancelled a task and completed its cleanup. */
public record TaskCancellation(UUID taskId, TaskState state, TerminationReason terminationReason) {
    public TaskCancellation {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(state, "state");
        Objects.requireNonNull(terminationReason, "terminationReason");
        if (state != TaskState.FINISHED || terminationReason != TerminationReason.CANCELLED) {
            throw new IllegalArgumentException("a cancellation result must be FINISHED and CANCELLED");
        }
    }
}
