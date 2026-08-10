package org.taskcage.sdk;

/** Base exception for SDK and protocol failures. */
public class TaskCageException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    public TaskCageException(String message) {
        super(message);
    }

    public TaskCageException(String message, Throwable cause) {
        super(message, cause);
    }
}
