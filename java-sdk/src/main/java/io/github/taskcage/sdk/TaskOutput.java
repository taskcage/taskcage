package io.github.taskcage.sdk;

import java.util.Objects;

/** Bounded stdout and stderr tails captured for a finished task. */
public record TaskOutput(String stdoutTail, String stderrTail, boolean stdoutTruncated, boolean stderrTruncated) {
    public TaskOutput {
        Objects.requireNonNull(stdoutTail, "stdoutTail");
        Objects.requireNonNull(stderrTail, "stderrTail");
    }
}
