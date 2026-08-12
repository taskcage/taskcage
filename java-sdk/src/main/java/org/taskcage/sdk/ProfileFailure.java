package org.taskcage.sdk;

import java.util.Objects;

/** Stable failure code and diagnostic message for a failed Profile Task. */
public record ProfileFailure(String code, String message) {
    public ProfileFailure {
        Objects.requireNonNull(code, "code");
        Objects.requireNonNull(message, "message");
        if (code.isBlank() || message.isBlank()) {
            throw new IllegalArgumentException("profile failure code and message must not be blank");
        }
    }
}
