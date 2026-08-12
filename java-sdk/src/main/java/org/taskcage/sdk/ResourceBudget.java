package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;

/** Mandatory resource limits for a TaskCage task. */
public record ResourceBudget(CpuQuota cpuMax, long memoryMaxBytes, long pidsMax, Duration wallTimeLimit,
                             int stdoutTailMaxBytes, int stderrTailMaxBytes) {
    private static final long MEBIBYTE = 1024L * 1024L;

    public ResourceBudget {
        Objects.requireNonNull(cpuMax, "cpuMax");
        Objects.requireNonNull(wallTimeLimit, "wallTimeLimit");
        if (memoryMaxBytes <= 0 || pidsMax <= 0 || wallTimeLimit.isZero() || wallTimeLimit.isNegative()) {
            throw new IllegalArgumentException("resource limits must be positive");
        }
        requirePositiveWholeMilliseconds(wallTimeLimit);
        if (stdoutTailMaxBytes < 1 || stderrTailMaxBytes < 1 || stdoutTailMaxBytes > 65_536
                || stderrTailMaxBytes > 65_536 || stdoutTailMaxBytes + stderrTailMaxBytes > 131_072) {
            throw new IllegalArgumentException("output tail limits must comply with Protocol v1");
        }
    }

    /** Returns the wall-time limit in Protocol v1's positive whole-millisecond representation. */
    public long wallTimeLimitMillis() {
        return requirePositiveWholeMilliseconds(wallTimeLimit);
    }

    static long requirePositiveWholeMilliseconds(Duration duration) {
        if (duration.getNano() % 1_000_000 != 0) {
            throw new IllegalArgumentException("wallTimeLimit must be an exact whole number of milliseconds");
        }
        try {
            long milliseconds = duration.toMillis();
            if (milliseconds <= 0) {
                throw new IllegalArgumentException("wallTimeLimit must be positive");
            }
            return milliseconds;
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException(
                    "wallTimeLimit must fit in a signed 64-bit millisecond value", exception);
        }
    }

    /**
     * Returns the Local Public Alpha convenience budget: one CPU, 512 MiB, 32 PIDs,
     * two minutes, and 64 KiB for each output tail.
     *
     * <p>This is an SDK request default, not a negotiated daemon capability. A deployment may
     * configure a lower maximum and reject it with {@code LIMIT_EXCEEDS_POLICY}.</p>
     */
    public static ResourceBudget safeDefaults() {
        return new ResourceBudget(
                new CpuQuota(100_000, 100_000),
                512L * MEBIBYTE,
                32,
                Duration.ofMinutes(2),
                65_536,
                65_536);
    }
}
