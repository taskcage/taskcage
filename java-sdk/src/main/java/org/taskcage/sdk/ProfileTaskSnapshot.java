package org.taskcage.sdk;

import java.util.UUID;

/** An immutable running or cleanup-confirmed Profile Task snapshot. */
public sealed interface ProfileTaskSnapshot
        permits RunningProfileTaskSnapshot, FinishedProfileTaskSnapshot {
    UUID taskId();

    ProfileIdentity profile();

    TaskState state();
}
