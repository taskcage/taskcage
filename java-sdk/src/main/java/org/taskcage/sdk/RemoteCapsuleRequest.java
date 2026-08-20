package org.taskcage.sdk;

import java.util.Objects;

/** A typed request to execute one exact Capsule through a Remote Runtime. */
public record RemoteCapsuleRequest(CapsuleIdentity capsule, RemoteProfileRequest profileRequest) {
    public RemoteCapsuleRequest {
        Objects.requireNonNull(capsule, "capsule");
        Objects.requireNonNull(profileRequest, "profileRequest");
        ProfileIdentity profile = profileRequest.profile();
        if (!capsule.name().equals(profile.name()) || !capsule.version().equals(profile.version())) {
            throw new IllegalArgumentException(
                    "Capsule identity must match the v1 Profile identity exactly");
        }
    }
}
