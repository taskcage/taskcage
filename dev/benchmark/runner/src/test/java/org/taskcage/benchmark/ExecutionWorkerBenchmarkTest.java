package org.taskcage.benchmark;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;

class ExecutionWorkerBenchmarkTest {
    @Test
    void acceptsMemoryLimitEvidenceFromTaskCage() {
        var task = task("memory_limit", "MEMORY_LIMIT_EXCEEDED", 100L, 16L * 1024 * 1024, 0, true, true);

        assertTrue(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.TASK_CAGE,
                ExecutionWorkerBenchmark.Scenario.MEMORY_LIMIT,
                List.of(task)).isEmpty());
    }

    @Test
    void rejectsTimeoutWhenTheMemoryScenarioExpectedAnOomKill() {
        var task = task("memory_limit", "TIMED_OUT", 100L, 16L * 1024 * 1024, 0, true, true);

        assertFalse(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.TASK_CAGE,
                ExecutionWorkerBenchmark.Scenario.MEMORY_LIMIT,
                List.of(task)).isEmpty());
    }

    @Test
    void acceptsContainerMemoryLimitEvidence() {
        var task = task("memory_limit", "MEMORY_LIMIT_EXCEEDED", null, null, 0, true, true);

        assertTrue(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.DOCKER_PER_TASK,
                ExecutionWorkerBenchmark.Scenario.MEMORY_LIMIT,
                List.of(task)).isEmpty());
    }

    @Test
    void requiresThePerTaskContainerTimeoutToRemoveTheTask() {
        var task = task("timeout_child", "TIMED_OUT_CONTAINER", null, null, 0, false, true);

        assertFalse(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.DOCKER_PER_TASK,
                ExecutionWorkerBenchmark.Scenario.TIMEOUT_CHILD,
                List.of(task)).isEmpty());
    }

    @Test
    void rejectsAZeroExitWhenNormalOutputIsInvalid() {
        var task = task("normal", "EXITED", null, null, 0, true, false);

        assertFalse(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.PROCESS_BUILDER,
                ExecutionWorkerBenchmark.Scenario.NORMAL,
                List.of(task)).isEmpty());
    }

    @Test
    void requiresTheProcessBuilderTimeoutToDemonstrateAResidualDescendant() {
        var task = task("timeout_child", "TIMED_OUT_ROOT_ONLY", null, null, 0, true, true);

        assertFalse(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.PROCESS_BUILDER,
                ExecutionWorkerBenchmark.Scenario.TIMEOUT_CHILD,
                List.of(task)).isEmpty());
    }

    @Test
    void requiresARequestedFailureScenarioToRunItsFailureWorkload() {
        var normal = task("normal", "EXITED", 100L, 1024L, 0, true, true);

        assertFalse(ExecutionWorkerBenchmark.validate(
                ExecutionWorkerBenchmark.Mode.TASK_CAGE,
                ExecutionWorkerBenchmark.Scenario.MEMORY_LIMIT,
                List.of(normal)).isEmpty());
    }

    private static ExecutionWorkerBenchmark.TaskMetric task(
            String workload,
            String termination,
            Long cpuMicros,
            Long memoryBytes,
            int residual,
            boolean cleanup,
            boolean output) {
        return new ExecutionWorkerBenchmark.TaskMetric(
                10, workload, termination, cpuMicros, memoryBytes, residual, cleanup, output);
    }
}
