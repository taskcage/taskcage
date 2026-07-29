package io.github.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** A task accepted by the daemon after cgroup limits were applied. */
public record Task(UUID taskId, ResourceBudget effectiveBudget) implements TaskSubmission {
    public Task {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(effectiveBudget, "effectiveBudget");
    }
}
