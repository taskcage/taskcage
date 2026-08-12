package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** A Profile Task accepted after effective resources were applied. */
public record ProfileTask(
        UUID taskId, ProfileIdentity profile, ResourceBudget effectiveResources)
        implements ProfileTaskSubmission {
    public ProfileTask {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(profile, "profile");
        Objects.requireNonNull(effectiveResources, "effectiveResources");
    }
}
