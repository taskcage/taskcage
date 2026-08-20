package org.taskcage.sdk;

import java.util.Objects;

/** A stable, pre-execution rejection of the transport-neutral Capsule contract. */
public final class CapsuleContractException extends IllegalArgumentException {
    private static final long serialVersionUID = 1L;

    private final String code;
    private final boolean retryable;

    public CapsuleContractException(String code, String message, boolean retryable) {
        super(message);
        Objects.requireNonNull(code, "code");
        if (code.isBlank()) {
            throw new IllegalArgumentException("code must not be blank");
        }
        this.code = code;
        this.retryable = retryable;
    }

    public String code() {
        return code;
    }

    public boolean retryable() {
        return retryable;
    }

    static void requireProfileMatch(CapsuleIdentity capsule, ProfileIdentity profile) {
        if (!capsule.name().equals(profile.name()) || !capsule.version().equals(profile.version())) {
            throw profileMismatch();
        }
    }

    static CapsuleContractException profileMismatch() {
        return new CapsuleContractException(
                "CAPSULE_PROFILE_MISMATCH",
                "Capsule identity must match the v1 Profile identity exactly",
                false);
    }
}
