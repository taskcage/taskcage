package io.github.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;

/** Mandatory resource limits for a TaskCage task. */
public record ResourceBudget(CpuQuota cpuMax, long memoryMaxBytes, long pidsMax, Duration wallTimeLimit,
                             int stdoutTailMaxBytes, int stderrTailMaxBytes) {
    public ResourceBudget {
        Objects.requireNonNull(cpuMax, "cpuMax");
        Objects.requireNonNull(wallTimeLimit, "wallTimeLimit");
        if (memoryMaxBytes <= 0 || pidsMax <= 0 || wallTimeLimit.isZero() || wallTimeLimit.isNegative()) {
            throw new IllegalArgumentException("resource limits must be positive");
        }
        if (stdoutTailMaxBytes < 1 || stderrTailMaxBytes < 1 || stdoutTailMaxBytes > 65_536
                || stderrTailMaxBytes > 65_536 || stdoutTailMaxBytes + stderrTailMaxBytes > 131_072) {
            throw new IllegalArgumentException("output tail limits must comply with Protocol v1");
        }
    }
}
