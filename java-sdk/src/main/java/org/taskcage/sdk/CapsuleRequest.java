package org.taskcage.sdk;

import java.util.Objects;

/** A request to execute one Profile declared by an exact Capsule identity. */
public record CapsuleRequest(CapsuleIdentity capsule, ProfileRequest profileRequest) {
    public CapsuleRequest {
        Objects.requireNonNull(capsule, "capsule");
        Objects.requireNonNull(profileRequest, "profileRequest");
        ProfileIdentity profile = profileRequest.profile();
        if (!capsule.name().equals(profile.name()) || !capsule.version().equals(profile.version())) {
            throw new IllegalArgumentException(
                    "Capsule identity must match the v1 Profile identity exactly");
        }
    }
}
