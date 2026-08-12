package org.taskcage.sdk;

import java.time.Duration;
import java.time.Instant;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ProfileApiTest {
    private static final UUID TASK_ID = UUID.fromString("44444444-4444-4444-8444-444444444444");
    private static final ProfileIdentity PROFILE = new ProfileIdentity("file-copy", "1.0.0");
    private static final Sha256Digest DIGEST = new Sha256Digest(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    @Test
    void profileValuesEnforceTheApprovedWireSyntax() {
        assertThrows(IllegalArgumentException.class, () -> new ProfileIdentity("FileCopy", "1.0.0"));
        assertThrows(IllegalArgumentException.class, () -> new ProfileIdentity("file-copy", "1.0"));
        assertThrows(IllegalArgumentException.class, () -> new ArtifactPath("jobs/../secret"));
        assertThrows(IllegalArgumentException.class, () -> new ArtifactPath(".taskcage/input"));
        assertThrows(IllegalArgumentException.class, () -> new Sha256Digest("sha256:ABCDEF"));
        assertThrows(IllegalArgumentException.class,
                () -> new LocalInputArtifact(new ArtifactPath("jobs/source.txt"), DIGEST, -1));
    }

    @Test
    void requestCopiesAndOrdersInputsWithoutAddingProfileDefaults() {
        Map<String, ProfileInputValue> mutable = new LinkedHashMap<>();
        mutable.put("source", new LocalInputArtifact(new ArtifactPath("jobs/42/source.txt"), DIGEST, 12));
        mutable.put("label", new StringProfileInput("archive"));

        ProfileRequest request = new ProfileRequest(PROFILE, mutable);
        mutable.clear();

        assertEquals(List.of("label", "source"), request.inputs().keySet().stream().toList());
        assertTrue(request.resourceOverrides().isEmpty());
        assertThrows(UnsupportedOperationException.class,
                () -> request.inputs().put("priority", new Int64ProfileInput(3)));
    }

    @Test
    void resourceOverridesPreserveOmittedFields() {
        ProfileResourceOverrides overrides = ProfileResourceOverrides.builder()
                .wallTimeLimit(Duration.ofMinutes(5))
                .stdoutTailMaxBytes(1024)
                .build();

        assertEquals(Duration.ofMinutes(5), overrides.wallTimeLimit().orElseThrow());
        assertEquals(1024, overrides.stdoutTailMaxBytes().orElseThrow());
        assertTrue(overrides.cpuMax().isEmpty());
        assertTrue(overrides.memoryMaxBytes().isEmpty());
        assertThrows(IllegalArgumentException.class,
                () -> ProfileResourceOverrides.builder().wallTimeLimit(Duration.ofNanos(1)));
    }

    @Test
    void finishedProfileSnapshotsEnforceOutcomeAndArtifactInvariants() {
        PublishedArtifact artifact = new PublishedArtifact(
                new ArtifactPath("tasks/44444444-4444-4444-8444-444444444444/result.txt"),
                DIGEST,
                12,
                "text/plain");
        FinishedProfileTaskSnapshot succeeded = new FinishedProfileTaskSnapshot(
                TASK_ID, PROFILE, ProfileOutcome.SUCCEEDED, successResult(), Map.of("result", artifact), null);

        assertEquals(TaskState.FINISHED, succeeded.state());
        assertEquals(artifact, succeeded.artifacts().get("result"));
        assertThrows(IllegalArgumentException.class, () -> new FinishedProfileTaskSnapshot(
                TASK_ID,
                PROFILE,
                ProfileOutcome.FAILED,
                successResult(),
                Map.of("result", artifact),
                new ProfileFailure("OUTPUT_CONTRACT_VIOLATION", "unexpected output")));
    }

    @Test
    void existingCustomClientsRemainCompatibleThroughDefaultMethods() {
        TaskCageClient client = new ProtocolV1OnlyClient();
        ProfileRequest request = new ProfileRequest(
                PROFILE,
                Map.of("source", new LocalInputArtifact(new ArtifactPath("jobs/source.txt"), DIGEST, 12)));

        assertThrows(UnsupportedOperationException.class,
                () -> client.submitProfile(UUID.randomUUID(), request));
        assertThrows(UnsupportedOperationException.class, () -> client.getProfileResult(TASK_ID));
    }

    @Test
    void profileHandleCachesImmediateFinishedResult() throws InterruptedException, TimeoutException {
        FinishedProfileTaskSnapshot finished = new FinishedProfileTaskSnapshot(
                TASK_ID,
                PROFILE,
                ProfileOutcome.FAILED,
                failureResult(),
                Map.of(),
                new ProfileFailure("CANCELLED", "cancelled"));
        ProtocolV1OnlyClient client = new ProtocolV1OnlyClient();
        ProfileTaskHandle handle = ProfileTaskHandle.from(client, finished);

        assertEquals(finished, handle.await(Duration.ofSeconds(1)));
        assertEquals(finished, handle.get());
        assertFalse(client.profileResultRequested);
    }

    private static ExecutionResult successResult() {
        Instant started = Instant.parse("2026-08-12T04:00:01Z");
        return new ExecutionResult(
                TerminationReason.EXITED,
                new ProcessResult(0, null),
                new TaskTiming(started.minusSeconds(1), started, started.plusSeconds(1), Duration.ofSeconds(1)),
                new TaskUsage(7_000, 1_048_576),
                new TaskOutput("", "", false, false));
    }

    private static ExecutionResult failureResult() {
        Instant started = Instant.parse("2026-08-12T04:00:01Z");
        return new ExecutionResult(
                TerminationReason.CANCELLED,
                new ProcessResult(null, "SIGKILL"),
                new TaskTiming(started.minusSeconds(1), started, started.plusSeconds(1), Duration.ofSeconds(1)),
                new TaskUsage(7_000, 1_048_576),
                new TaskOutput("", "", false, false));
    }

    private static final class ProtocolV1OnlyClient implements TaskCageClient {
        private boolean profileResultRequested;

        @Override
        public TaskCageCapabilities capabilities() {
            return new TaskCageCapabilities("0.1.0", List.of(1), 1_048_576, 4, true);
        }

        @Override
        public TaskSubmission submit(TaskSpec task) {
            throw new UnsupportedOperationException();
        }

        @Override
        public TaskSubmission submit(UUID clientRequestId, TaskSpec task) {
            throw new UnsupportedOperationException();
        }

        @Override
        public TaskSnapshot getTask(UUID taskId) {
            throw new UnsupportedOperationException();
        }

        @Override
        public ProfileTaskSnapshot getProfileResult(UUID taskId) {
            profileResultRequested = true;
            return TaskCageClient.super.getProfileResult(taskId);
        }

        @Override
        public TaskCancellation cancelTask(UUID taskId) {
            throw new UnsupportedOperationException();
        }

        @Override
        public void close() {}
    }
}
