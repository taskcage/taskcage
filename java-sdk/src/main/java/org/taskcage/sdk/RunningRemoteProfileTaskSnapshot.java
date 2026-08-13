package org.taskcage.sdk;

import java.time.Instant;
import java.util.Objects;
import java.util.UUID;

/** A Remote Profile Task whose target is running inside its task cgroup. */
public record RunningRemoteProfileTaskSnapshot(
        UUID taskId, ProfileIdentity profile, Instant submittedAt, Instant startedAt)
        implements RemoteProfileTaskSnapshot {
    public RunningRemoteProfileTaskSnapshot {
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
