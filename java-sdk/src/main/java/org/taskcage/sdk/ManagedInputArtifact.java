package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/**
 * A completed, principal-owned Remote input Artifact selected by daemon-issued ID.
 *
 * <p>An Artifact is single-use: a newly accepted Profile submission transfers it to the daemon's task-owned
 * snapshot. Reuse the same submission idempotency key to recover that Task; do not use this Artifact in another
 * submission after acceptance.
 */
public record ManagedInputArtifact(UUID artifactId) implements RemoteProfileInputValue {
    public ManagedInputArtifact {
        Objects.requireNonNull(artifactId, "artifactId");
    }
}
