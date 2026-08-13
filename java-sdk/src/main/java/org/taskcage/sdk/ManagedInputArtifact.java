package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** A completed, principal-owned Remote input Artifact selected by daemon-issued ID. */
public record ManagedInputArtifact(UUID artifactId) implements RemoteProfileInputValue {
    public ManagedInputArtifact {
        Objects.requireNonNull(artifactId, "artifactId");
    }
}
