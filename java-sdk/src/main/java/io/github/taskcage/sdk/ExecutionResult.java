package io.github.taskcage.sdk;

import java.util.Objects;

/** Final process, timing, usage, and output data for a finished task. */
public record ExecutionResult(
        TerminationReason terminationReason,
        ProcessResult process,
        TaskTiming timing,
        TaskUsage usage,
        TaskOutput output) {
    public ExecutionResult {
        Objects.requireNonNull(terminationReason, "terminationReason");
        Objects.requireNonNull(process, "process");
        Objects.requireNonNull(timing, "timing");
        Objects.requireNonNull(usage, "usage");
        Objects.requireNonNull(output, "output");
    }
}
