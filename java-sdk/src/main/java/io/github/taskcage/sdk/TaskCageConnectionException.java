package io.github.taskcage.sdk;

/** The Unix domain socket could not be connected to or used safely. */
public final class TaskCageConnectionException extends TaskCageException {
    private static final long serialVersionUID = 1L;

    public TaskCageConnectionException(String message, Throwable cause) {
        super(message, cause);
    }
}
