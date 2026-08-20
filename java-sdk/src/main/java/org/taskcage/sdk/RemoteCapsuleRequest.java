package org.taskcage.sdk;

import java.util.Objects;

/** A typed request to execute one exact Capsule through a Remote Runtime. */
public record RemoteCapsuleRequest(CapsuleIdentity capsule, RemoteProfileRequest profileRequest) {
    public RemoteCapsuleRequest {
        Objects.requireNonNull(capsule, "capsule");
        Objects.requireNonNull(profileRequest, "profileRequest");
        CapsuleContractException.requireProfileMatch(capsule, profileRequest.profile());
    }
}
