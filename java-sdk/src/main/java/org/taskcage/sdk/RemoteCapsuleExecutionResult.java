package org.taskcage.sdk;

import java.util.Objects;

/** Cleanup-confirmed result of one Remote Capsule execution. */
public record RemoteCapsuleExecutionResult(
        CapsuleIdentity capsule,
        FinishedRemoteProfileTaskSnapshot profileTask) {
    public RemoteCapsuleExecutionResult {
        Objects.requireNonNull(capsule, "capsule");
        Objects.requireNonNull(profileTask, "profileTask");
    }

    public ProfileOutcome outcome() {
        return profileTask.profileOutcome();
    }

    public ExecutionResult execution() {
        return profileTask.result();
    }

    /** Remote terminal Profile snapshots are published only after daemon cleanup confirms. */
    public boolean cleanupConfirmed() {
        return true;
    }
}
