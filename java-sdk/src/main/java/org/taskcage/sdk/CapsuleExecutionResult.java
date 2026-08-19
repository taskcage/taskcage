package org.taskcage.sdk;

import java.util.Objects;

/** Cleanup-confirmed result of one Capsule execution. */
public record CapsuleExecutionResult(
        CapsuleIdentity capsule,
        FinishedProfileTaskSnapshot profileTask) {
    public CapsuleExecutionResult {
        Objects.requireNonNull(capsule, "capsule");
        Objects.requireNonNull(profileTask, "profileTask");
    }

    public ProfileOutcome outcome() {
        return profileTask.profileOutcome();
    }

    public ExecutionResult execution() {
        return profileTask.result();
    }
}
