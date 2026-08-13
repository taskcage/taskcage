package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** A running Profile Task accepted by an authenticated Remote Runtime. */
public record RemoteProfileTask(UUID taskId, ProfileIdentity profile, ResourceBudget effectiveResources) {
    public RemoteProfileTask {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(profile, "profile");
        Objects.requireNonNull(effectiveResources, "effectiveResources");
    }
}
