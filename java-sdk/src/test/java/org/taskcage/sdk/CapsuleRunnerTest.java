package org.taskcage.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.time.Duration;
import java.time.Instant;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.Test;

class CapsuleRunnerTest {
    @Test
    void capsuleIdentityUsesTheSharedStrictIdentityShape() {
        CapsuleIdentity identity = new CapsuleIdentity("ffmpeg-audio-to-wav", "1.0.0");

        assertEquals("ffmpeg-audio-to-wav", identity.name());
        assertEquals("1.0.0", identity.version());
        assertThrows(IllegalArgumentException.class, () -> new CapsuleIdentity("FFmpeg", "1.0.0"));
        assertThrows(IllegalArgumentException.class, () -> new CapsuleIdentity("tool", "1.0"));
    }

    @Test
    void externalRunnerPreservesTheCapsuleIdentityAndProfileResult() throws Exception {
        ProfileIdentity profile = new ProfileIdentity("file-copy", "1.0.0");
        CapsuleRequest request = CapsuleRequest.builder("file-copy", "1.0.0")
                .string("label", "archive")
                .build();
        FinishedProfileTaskSnapshot snapshot = success(profile);

        CapsuleExecutionResult result = CapsuleRunner.external(new StubClient(snapshot))
                .execute(UUID.randomUUID(), request, Duration.ofSeconds(1));

        assertEquals(request.capsule(), result.capsule());
        assertEquals(snapshot, result.profileTask());
        assertEquals(ProfileOutcome.SUCCEEDED, result.outcome());
        assertTrue(result.cleanupConfirmed());
        assertTrue(result.execution().process().exitCode() != null);
    }

    @Test
    void externalRunnerDoesNotConvertWaitTimeout() {
        ProfileIdentity profile = new ProfileIdentity("file-copy", "1.0.0");
        CapsuleRequest request = CapsuleRequest.builder("file-copy", "1.0.0")
                .string("label", "archive")
                .build();

        assertThrows(
                TimeoutException.class,
                () -> CapsuleRunner.external(new StubClient(null))
                        .execute(request, Duration.ofSeconds(1)));
    }

    @Test
    void builderDerivesTheProfileIdentityAndRejectsInvalidInputs() {
        CapsuleRequest request = CapsuleRequest.builder("file-copy", "1.0.0")
                .string("label", "archive")
                .int64("attempt", 2)
                .bool("overwrite", true)
                .build();

        ProfileRequest internal = request.toProfileRequest();
        assertEquals(new ProfileIdentity("file-copy", "1.0.0"), internal.profile());
        assertEquals(new StringProfileInput("archive"), request.inputs().get("label"));
        assertEquals(new Int64ProfileInput(2), request.inputs().get("attempt"));
        assertEquals(new BooleanProfileInput(true), request.inputs().get("overwrite"));
        assertThrows(IllegalArgumentException.class,
                () -> CapsuleRequest.builder("file-copy", "1.0.0").build());
        assertThrows(IllegalArgumentException.class,
                () -> CapsuleRequest.builder("file-copy", "1.0.0")
                        .string("Bad", "archive")
                        .build());
    }

    private static final class StubClient implements TaskCageClient {
        private final FinishedProfileTaskSnapshot snapshot;

        private StubClient(FinishedProfileTaskSnapshot snapshot) {
            this.snapshot = snapshot;
        }

        @Override
        public FinishedProfileTaskSnapshot run(ProfileRequest request, Duration waitTimeout)
                throws TimeoutException {
            if (snapshot == null) {
                throw new TimeoutException("stub timeout");
            }
            return snapshot;
        }

        @Override
        public FinishedProfileTaskSnapshot run(
                UUID clientRequestId, ProfileRequest request, Duration waitTimeout)
                throws TimeoutException {
            return run(request, waitTimeout);
        }

        @Override
        public TaskCageCapabilities capabilities() {
            throw new UnsupportedOperationException();
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
        public TaskCancellation cancelTask(UUID taskId) {
            throw new UnsupportedOperationException();
        }

        @Override
        public void close() {
            // Test client owns no resources.
        }
    }

    private static FinishedProfileTaskSnapshot success(ProfileIdentity profile) {
        Instant started = Instant.parse("2026-08-12T04:00:01Z");
        ExecutionResult result = new ExecutionResult(
                TerminationReason.EXITED,
                new ProcessResult(0, null),
                new TaskTiming(started.minusSeconds(1), started, started.plusSeconds(1), Duration.ofSeconds(1)),
                new TaskUsage(7_000, 1_048_576),
                new TaskOutput("", "", false, false));
        PublishedArtifact artifact = new PublishedArtifact(
                new ArtifactPath("tasks/44444444-4444-4444-8444-444444444444/result.txt"),
                new Sha256Digest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                12,
                "text/plain");
        return new FinishedProfileTaskSnapshot(
                UUID.fromString("44444444-4444-4444-8444-444444444444"),
                profile,
                ProfileOutcome.SUCCEEDED,
                result,
                Map.of("result", artifact),
                null);
    }
}
