package io.github.taskcage.sdk;

/** The mutually exclusive reason a finished task stopped. */
public enum TerminationReason {
    EXITED,
    EXECUTION_FAILED,
    CANCELLED,
    TIMED_OUT,
    MEMORY_LIMIT_EXCEEDED,
    PROCESS_LIMIT_EXCEEDED,
    DAEMON_ERROR
}
