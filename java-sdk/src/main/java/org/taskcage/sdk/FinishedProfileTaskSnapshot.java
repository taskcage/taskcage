package org.taskcage.sdk;

import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;
import java.util.UUID;

/** A Profile Task whose process, cgroup, and Artifact staging cleanup have completed. */
public record FinishedProfileTaskSnapshot(
        UUID taskId,
        ProfileIdentity profile,
        ProfileOutcome profileOutcome,
        ExecutionResult result,
        Map<String, PublishedArtifact> artifacts,
        ProfileFailure failure)
        implements ProfileTaskSnapshot, ProfileTaskSubmission {
    public FinishedProfileTaskSnapshot {
        Objects.requireNonNull(taskId, "taskId");
        Objects.requireNonNull(profile, "profile");
        Objects.requireNonNull(profileOutcome, "profileOutcome");
        Objects.requireNonNull(result, "result");
        Objects.requireNonNull(artifacts, "artifacts");
        TreeMap<String, PublishedArtifact> copy = new TreeMap<>();
        artifacts.forEach((name, artifact) -> {
            if (name == null || name.isBlank() || artifact == null) {
                throw new IllegalArgumentException("artifact slots and values must not be blank or null");
            }
            copy.put(name, artifact);
        });
        artifacts = Collections.unmodifiableMap(copy);

        if (profileOutcome == ProfileOutcome.SUCCEEDED) {
            if (failure != null || artifacts.size() != 1) {
                throw new IllegalArgumentException("a successful Profile Task must have one Artifact and no failure");
            }
            if (result.terminationReason() != TerminationReason.EXITED
                    || !Integer.valueOf(0).equals(result.process().exitCode())) {
                throw new IllegalArgumentException("a successful Profile Task must exit with code zero");
            }
        } else if (failure == null || !artifacts.isEmpty()) {
            throw new IllegalArgumentException("a failed Profile Task must have a failure and no Artifacts");
        }
    }

    @Override
    public TaskState state() {
        return TaskState.FINISHED;
    }
}
