package org.taskcage.sdk;

import java.util.Objects;

/** A request to execute one Profile declared by an exact Capsule identity. */
public record CapsuleRequest(CapsuleIdentity capsule, ProfileRequest profileRequest) {
    public CapsuleRequest {
        Objects.requireNonNull(capsule, "capsule");
        Objects.requireNonNull(profileRequest, "profileRequest");
    }
}
