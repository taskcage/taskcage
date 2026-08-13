package org.taskcage.sdk;

import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;
import java.util.UUID;

/** A cleanup-confirmed Remote Profile Task result with principal-owned downloadable output Artifacts. */
public record FinishedRemoteProfileTaskSnapshot(
        UUID taskId,
        ProfileIdentity profile,
        ProfileOutcome profileOutcome,
        ExecutionResult result,
        Map<String, ManagedOutputArtifact> artifacts,
        ProfileFailure failure)
        implements RemoteProfileTaskSnapshot {
    public FinishedRemoteProfileTaskSnapshot {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(profile, "profile");
        Objects.requireNonNull(profileOutcome, "profileOutcome");
        Objects.requireNonNull(result, "result");
        Objects.requireNonNull(artifacts, "artifacts");
        TreeMap<String, ManagedOutputArtifact> copy = new TreeMap<>();
        artifacts.forEach((name, artifact) -> {
            if (name == null || name.isBlank() || artifact == null) {
                throw new IllegalArgumentException("artifact slots and values must not be blank or null");
            }
            copy.put(name, artifact);
        });
        artifacts = Collections.unmodifiableMap(copy);
        if (profileOutcome == ProfileOutcome.SUCCEEDED) {
            if (failure != null || artifacts.size() != 1
                    || result.terminationReason() != TerminationReason.EXITED
                    || !Integer.valueOf(0).equals(result.process().exitCode())) {
                throw new IllegalArgumentException("a successful Remote Profile Task must have one Artifact and exit with code zero");
            }
        } else if (failure == null || !artifacts.isEmpty()) {
            throw new IllegalArgumentException("a failed Remote Profile Task must have a failure and no Artifacts");
        }
    }

    @Override
    public TaskState state() {
        return TaskState.FINISHED;
    }
}
