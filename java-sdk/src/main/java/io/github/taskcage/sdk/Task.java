package io.github.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** Accepted daemon task. Status and cancellation operations are added in the next SDK stage. */
public record Task(UUID taskId, ResourceBudget effectiveBudget) {
    public Task {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(effectiveBudget, "effectiveBudget");
    }
}
