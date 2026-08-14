package org.taskcage.release.consumer;

import org.taskcage.sdk.ExternalCommand;
import org.taskcage.sdk.TaskSpec;

public final class ReleaseConsumer {
    private ReleaseConsumer() {}

    public static TaskSpec task(ExternalCommand command) {
        return new TaskSpec(command);
    }
}
