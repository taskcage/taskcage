package org.taskcage.sdk;

/** The daemon response did not conform to Protocol v1. */
public final class TaskCageProtocolException extends TaskCageException {
    private static final long serialVersionUID = 1L;

    public TaskCageProtocolException(String message) {
        super(message);
    }

    public TaskCageProtocolException(String message, Throwable cause) {
        super(message, cause);
    }
}
