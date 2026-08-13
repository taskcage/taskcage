package org.taskcage.sdk;

import java.util.UUID;

/** Current Remote Profile Task state visible only to its authenticated principal. */
public sealed interface RemoteProfileTaskSnapshot
        permits RunningRemoteProfileTaskSnapshot, FinishedRemoteProfileTaskSnapshot {
    UUID taskId();

    ProfileIdentity profile();

    TaskState state();
}
