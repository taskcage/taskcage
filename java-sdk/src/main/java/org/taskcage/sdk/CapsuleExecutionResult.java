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

    /**
     * Returns whether the result was published after process, cgroup, and staging cleanup.
     *
     * <p>A {@link FinishedProfileTaskSnapshot} is only constructible for a cleanup-confirmed
     * terminal task, so this value is always {@code true}. Exposing it keeps the Capsule result
     * contract explicit for callers that do not know the Profile snapshot type.
     */
    public boolean cleanupConfirmed() {
        return true;
    }
}
