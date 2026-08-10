package org.taskcage.sdk;

import java.time.Instant;
import java.util.Objects;
import java.util.UUID;

/** A task whose final result is not yet available. */
public record RunningTaskSnapshot(UUID taskId, Instant submittedAt, Instant startedAt) implements TaskSnapshot {
    public RunningTaskSnapshot {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(submittedAt, "submittedAt");
        Objects.requireNonNull(startedAt, "startedAt");
    }

    @Override
    public TaskState state() {
        return TaskState.RUNNING;
    }
}
