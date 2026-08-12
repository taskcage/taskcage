package org.taskcage.sdk;

import java.time.Instant;
import java.util.Objects;
import java.util.UUID;

/** A Profile Task whose final result is not yet available. */
public record RunningProfileTaskSnapshot(
        UUID taskId, ProfileIdentity profile, Instant submittedAt, Instant startedAt)
        implements ProfileTaskSnapshot {
    public RunningProfileTaskSnapshot {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(profile, "profile");
        Objects.requireNonNull(submittedAt, "submittedAt");
        Objects.requireNonNull(startedAt, "startedAt");
    }

    @Override
    public TaskState state() {
        return TaskState.RUNNING;
    }
}
