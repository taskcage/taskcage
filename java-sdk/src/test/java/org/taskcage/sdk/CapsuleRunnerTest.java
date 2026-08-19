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
        ProfileRequest profileRequest = new ProfileRequest(
                profile, Map.of("label", new StringProfileInput("archive")));
        CapsuleRequest request = new CapsuleRequest(
                new CapsuleIdentity("file-copy-capsule", "1.0.0"), profileRequest);
        FinishedProfileTaskSnapshot snapshot = success(profile);

        CapsuleExecutionResult result = CapsuleRunner.external(new StubRuntime(snapshot))
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
        CapsuleRequest request = new CapsuleRequest(
                new CapsuleIdentity("file-copy-capsule", "1.0.0"),
                new ProfileRequest(profile, Map.of("label", new StringProfileInput("archive"))));

        assertThrows(
                TimeoutException.class,
                () -> CapsuleRunner.external(new StubRuntime(null))
                        .execute(request, Duration.ofSeconds(1)));
    }

    private static final class StubRuntime implements ProfileRuntime {
        private final FinishedProfileTaskSnapshot snapshot;

        private StubRuntime(FinishedProfileTaskSnapshot snapshot) {
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
