package org.taskcage.sdk;

/** cgroup resource usage recorded when a task finished. */
public record TaskUsage(long cpuTimeMicros, long memoryPeakBytes) {
    public TaskUsage {
        if (cpuTimeMicros < 0 || memoryPeakBytes < 0) {
            throw new IllegalArgumentException("usage values must not be negative");
        }
    }
}
