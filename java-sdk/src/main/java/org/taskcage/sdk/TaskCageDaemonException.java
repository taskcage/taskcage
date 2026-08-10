package org.taskcage.sdk;

import java.util.Objects;

/** A protocol-valid error response returned by the TaskCage daemon. */
public final class TaskCageDaemonException extends TaskCageException {
    private static final long serialVersionUID = 1L;

    private final String code;
    private final boolean retryable;

    public TaskCageDaemonException(String code, String message, boolean retryable) {
        super(message);
        this.code = requireText(code, "code");
        this.retryable = retryable;
    }

    public String code() {
        return code;
    }

    public boolean retryable() {
        return retryable;
    }

    private static String requireText(String value, String name) {
        Objects.requireNonNull(value, name);
        if (value.isEmpty()) {
            throw new IllegalArgumentException(name + " must not be empty");
        }
        return value;
    }
}
