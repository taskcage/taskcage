package io.github.taskcage.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class ResourceBudgetTest {
    @Test
    void safeDefaultsMatchThePublishedAlphaContract() {
        ResourceBudget budget = ResourceBudget.safeDefaults();

        assertEquals(new CpuQuota(100_000, 100_000), budget.cpuMax());
        assertEquals(512L * 1024 * 1024, budget.memoryMaxBytes());
        assertEquals(32, budget.pidsMax());
        assertEquals(Duration.ofMinutes(2), budget.wallTimeLimit());
        assertEquals(65_536, budget.stdoutTailMaxBytes());
        assertEquals(65_536, budget.stderrTailMaxBytes());
    }

    @Test
    void taskSpecConvenienceConstructorUsesSafeDefaults() {
        Path workingDirectory = Path.of(System.getProperty("java.io.tmpdir")).toAbsolutePath();
        ExternalCommand command = new ExternalCommand(
                workingDirectory.resolve("taskcage-test-tool"),
                List.of(),
                workingDirectory,
                Map.of());

        assertEquals(ResourceBudget.safeDefaults(), new TaskSpec(command).budget());
    }
}
