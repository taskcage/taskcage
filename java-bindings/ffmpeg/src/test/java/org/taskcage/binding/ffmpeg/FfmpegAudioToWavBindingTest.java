package org.taskcage.binding.ffmpeg;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.time.Duration;
import java.time.Instant;
import java.util.Map;
import java.util.UUID;
import org.junit.jupiter.api.Test;
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.FinishedProfileTaskSnapshot;
import org.taskcage.sdk.Int64ProfileInput;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProcessResult;
import org.taskcage.sdk.ProfileFailure;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.ProfileRequest;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.ProfileTaskSnapshot;
import org.taskcage.sdk.ProfileTaskSubmission;
import org.taskcage.sdk.PublishedArtifact;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCancellation;
import org.taskcage.sdk.TaskCageCapabilities;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.TaskOutput;
import org.taskcage.sdk.TaskSnapshot;
import org.taskcage.sdk.TaskSpec;
import org.taskcage.sdk.TaskSubmission;
import org.taskcage.sdk.TaskTiming;
import org.taskcage.sdk.TaskUsage;
import org.taskcage.sdk.TerminationReason;

class FfmpegAudioToWavBindingTest {
    private static final String INPUT_DIGEST =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    private static final String OUTPUT_DIGEST =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    @Test
    void mapsTypedRequestToPinnedProfileAndSlots() {
        ProfileResourceOverrides overrides = ProfileResourceOverrides.builder()
                .wallTimeLimit(Duration.ofMinutes(3))
                .build();
        LocalInputArtifact source = source();

        ProfileRequest request = FfmpegAudioToWavBinding.toProfileRequest(
                new FfmpegAudioToWavRequest(
                        source, AudioSampleRate.HZ_16000, AudioChannels.MONO, overrides));

        assertEquals(new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0"), request.profile());
        assertEquals(source, request.inputs().get("source"));
        assertEquals(new Int64ProfileInput(16_000), request.inputs().get("sample_rate_hz"));
        assertEquals(new Int64ProfileInput(1), request.inputs().get("channels"));
        assertEquals(3, request.inputs().size());
        assertSame(overrides, request.resourceOverrides());
    }

    @Test
    void runReturnsTypedSuccessAndPreservesCoreTask() throws Exception {
        UUID taskId = UUID.randomUUID();
        FinishedProfileTaskSnapshot finished = succeeded(taskId, "audio", "audio/wav", "result.wav");
        RecordingClient client = new RecordingClient(finished);

        FfmpegAudioToWavResult result = FfmpegAudioToWavBinding.using(client)
                .run(request(), Duration.ofSeconds(5));

        FfmpegAudioToWavSuccess success =
                assertInstanceOf(FfmpegAudioToWavSuccess.class, result);
        assertSame(finished, success.task());
        assertEquals(finished.artifacts().get("audio"), success.audio());
        assertEquals(FfmpegAudioToWavBinding.PROFILE, client.request.profile());
    }

    @Test
    void callerOwnedIdempotencyKeyIsPassedToCoreClient() throws Exception {
        UUID taskId = UUID.randomUUID();
        UUID clientRequestId = UUID.randomUUID();
        RecordingClient client = new RecordingClient(
                succeeded(taskId, "audio", "audio/wav", "result.wav"));

        FfmpegAudioToWavBinding.using(client)
                .run(clientRequestId, request(), Duration.ofSeconds(5));

        assertEquals(clientRequestId, client.clientRequestId);
    }

    @Test
    void processFailureRemainsATypedResult() throws Exception {
        FinishedProfileTaskSnapshot finished = failed(UUID.randomUUID());
        RecordingClient client = new RecordingClient(finished);

        FfmpegAudioToWavResult result = FfmpegAudioToWavBinding.using(client)
                .run(request(), Duration.ofSeconds(5));

        FfmpegAudioToWavFailure failure =
                assertInstanceOf(FfmpegAudioToWavFailure.class, result);
        assertSame(finished, failure.task());
        assertEquals("PROCESS_EXITED_NONZERO", failure.failure().code());
    }

    @Test
    void rejectsResultForAnotherProfile() {
        UUID taskId = UUID.randomUUID();
        FinishedProfileTaskSnapshot task = new FinishedProfileTaskSnapshot(
                taskId,
                new ProfileIdentity("file-copy", "1.0.0"),
                ProfileOutcome.SUCCEEDED,
                successfulExecution(),
                Map.of("audio", artifact(taskId, "audio/wav", "result.wav")),
                null);

        assertThrows(
                TaskCageProtocolException.class,
                () -> FfmpegAudioToWavBinding.toBindingResult(task));
    }

    @Test
    void rejectsUnexpectedOutputSlot() {
        FinishedProfileTaskSnapshot task =
                succeeded(UUID.randomUUID(), "result", "audio/wav", "result.wav");

        assertThrows(
                TaskCageProtocolException.class,
                () -> FfmpegAudioToWavBinding.toBindingResult(task));
    }

    @Test
    void rejectsUnexpectedOutputMediaType() {
        FinishedProfileTaskSnapshot task = succeeded(
                UUID.randomUUID(), "audio", "application/octet-stream", "result.wav");

        assertThrows(
                TaskCageProtocolException.class,
                () -> FfmpegAudioToWavBinding.toBindingResult(task));
    }

    @Test
    void rejectsUnexpectedOutputFileName() {
        FinishedProfileTaskSnapshot task =
                succeeded(UUID.randomUUID(), "audio", "audio/wav", "other.wav");

        assertThrows(
                TaskCageProtocolException.class,
                () -> FfmpegAudioToWavBinding.toBindingResult(task));
    }

    private static FfmpegAudioToWavRequest request() {
        return new FfmpegAudioToWavRequest(
                source(), AudioSampleRate.HZ_16000, AudioChannels.MONO);
    }

    private static LocalInputArtifact source() {
        return new LocalInputArtifact(
                new ArtifactPath("jobs/42/source.mov"), new Sha256Digest(INPUT_DIGEST), 1_024);
    }

    private static FinishedProfileTaskSnapshot succeeded(
            UUID taskId, String slot, String mediaType, String fileName) {
        return new FinishedProfileTaskSnapshot(
                taskId,
                FfmpegAudioToWavBinding.PROFILE,
                ProfileOutcome.SUCCEEDED,
                successfulExecution(),
                Map.of(slot, artifact(taskId, mediaType, fileName)),
                null);
    }

    private static FinishedProfileTaskSnapshot failed(UUID taskId) {
        return new FinishedProfileTaskSnapshot(
                taskId,
                FfmpegAudioToWavBinding.PROFILE,
                ProfileOutcome.FAILED,
                new ExecutionResult(
                        TerminationReason.EXITED,
                        new ProcessResult(1, null),
                        timing(),
                        new TaskUsage(10, 20),
                        new TaskOutput("", "invalid input", false, false)),
                Map.of(),
                new ProfileFailure("PROCESS_EXITED_NONZERO", "FFmpeg exited with code 1"));
    }

    private static PublishedArtifact artifact(
            UUID taskId, String mediaType, String fileName) {
        return new PublishedArtifact(
                new ArtifactPath("tasks/" + taskId + "/" + fileName),
                new Sha256Digest(OUTPUT_DIGEST),
                2_048,
                mediaType);
    }

    private static ExecutionResult successfulExecution() {
        return new ExecutionResult(
                TerminationReason.EXITED,
                new ProcessResult(0, null),
                timing(),
                new TaskUsage(10, 20),
                new TaskOutput("", "", false, false));
    }

    private static TaskTiming timing() {
        Instant started = Instant.parse("2026-08-12T00:00:00Z");
        return new TaskTiming(started, started, started.plusSeconds(1), Duration.ofSeconds(1));
    }

    private static final class RecordingClient implements TaskCageClient {
        private final FinishedProfileTaskSnapshot result;
        private UUID clientRequestId;
        private ProfileRequest request;

        private RecordingClient(FinishedProfileTaskSnapshot result) {
            this.result = result;
        }

        @Override
        public FinishedProfileTaskSnapshot run(ProfileRequest request, Duration waitTimeout) {
            this.request = request;
            return result;
        }

        @Override
        public FinishedProfileTaskSnapshot run(
                UUID clientRequestId, ProfileRequest request, Duration waitTimeout) {
            this.clientRequestId = clientRequestId;
            this.request = request;
            return result;
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
        public ProfileTaskSubmission submitProfile(
                UUID clientRequestId, ProfileRequest request) {
            throw new UnsupportedOperationException();
        }

        @Override
        public TaskSnapshot getTask(UUID taskId) {
            throw new UnsupportedOperationException();
        }

        @Override
        public ProfileTaskSnapshot getProfileResult(UUID taskId) {
            throw new UnsupportedOperationException();
        }

        @Override
        public TaskCancellation cancelTask(UUID taskId) {
            throw new UnsupportedOperationException();
        }

        @Override
        public void close() {}
    }
}
