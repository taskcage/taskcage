package org.taskcage.sdk;

import java.time.Duration;
import java.time.Instant;
import java.util.Objects;

/** Wall-clock timing captured by the daemon for a finished task. */
public record TaskTiming(Instant submittedAt, Instant startedAt, Instant finishedAt, Duration wallTime) {
    public TaskTiming {
        Objects.requireNonNull(submittedAt, "submittedAt");
        Objects.requireNonNull(startedAt, "startedAt");
        Objects.requireNonNull(finishedAt, "finishedAt");
        Objects.requireNonNull(wallTime, "wallTime");
        if (wallTime.isNegative()) {
            throw new IllegalArgumentException("wallTime must not be negative");
        }
    }
}
