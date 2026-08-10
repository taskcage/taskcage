package org.taskcage.sdk;

import java.util.UUID;

/** An immutable daemon snapshot of a running or finished task. */
public sealed interface TaskSnapshot permits RunningTaskSnapshot, FinishedTaskSnapshot {
    UUID taskId();

    TaskState state();
}
