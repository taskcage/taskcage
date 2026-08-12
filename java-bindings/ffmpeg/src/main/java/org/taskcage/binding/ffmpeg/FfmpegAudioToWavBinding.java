package org.taskcage.binding.ffmpeg;

import java.time.Duration;
import java.util.Map;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeoutException;
import org.taskcage.sdk.FinishedProfileTaskSnapshot;
import org.taskcage.sdk.Int64ProfileInput;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.ProfileRequest;
import org.taskcage.sdk.PublishedArtifact;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageProtocolException;

/** Java Binding for the installed {@code ffmpeg-audio-to-wav@1.0.0} Execution Profile. */
public final class FfmpegAudioToWavBinding {
    static final ProfileIdentity PROFILE = new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0");
    static final String OUTPUT_SLOT = "audio";
    private static final String OUTPUT_MEDIA_TYPE = "audio/wav";
    private static final String OUTPUT_FILE_NAME = "result.wav";

    private final TaskCageClient client;

    private FfmpegAudioToWavBinding(TaskCageClient client) {
        this.client = Objects.requireNonNull(client, "client");
    }

    /**
     * Creates a lightweight Binding view without taking ownership of the Core client.
     *
     * @param client connected or lazily connecting Core SDK client
     * @return Binding backed by the supplied client
     */
    public static FfmpegAudioToWavBinding using(TaskCageClient client) {
        return new FfmpegAudioToWavBinding(client);
    }

    /**
     * Runs the Profile and preserves process failures as a typed Binding result.
     *
     * @param request typed FFmpeg input
     * @param waitTimeout positive Core completion wait timeout
     * @return cleanup-confirmed typed success or failure
     * @throws InterruptedException if the waiting thread is interrupted
     * @throws TimeoutException if the Core wait deadline expires without cancelling the Task
     */
    public FfmpegAudioToWavResult run(
            FfmpegAudioToWavRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        FinishedProfileTaskSnapshot task = client.run(toProfileRequest(request), waitTimeout);
        return toBindingResult(task);
    }

    /**
     * Runs with a caller-owned idempotency key for lost-response recovery.
     *
     * @param clientRequestId caller-owned Core idempotency key
     * @param request typed FFmpeg input
     * @param waitTimeout positive Core completion wait timeout
     * @return cleanup-confirmed typed success or failure
     * @throws InterruptedException if the waiting thread is interrupted
     * @throws TimeoutException if the Core wait deadline expires without cancelling the Task
     */
    public FfmpegAudioToWavResult run(
            UUID clientRequestId,
            FfmpegAudioToWavRequest request,
            Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        FinishedProfileTaskSnapshot task =
                client.run(clientRequestId, toProfileRequest(request), waitTimeout);
        return toBindingResult(task);
    }

    static ProfileRequest toProfileRequest(FfmpegAudioToWavRequest request) {
        Objects.requireNonNull(request, "request");
        return new ProfileRequest(
                PROFILE,
                Map.of(
                        "source", request.source(),
                        "sample_rate_hz", new Int64ProfileInput(request.sampleRate().hertz()),
                        "channels", new Int64ProfileInput(request.channels().count())),
                request.resourceOverrides());
    }

    static FfmpegAudioToWavResult toBindingResult(FinishedProfileTaskSnapshot task) {
        Objects.requireNonNull(task, "task");
        if (!PROFILE.equals(task.profile())) {
            throw new TaskCageProtocolException(
                    "FFmpeg Binding received result for unexpected Profile " + task.profile());
        }
        if (task.profileOutcome() == ProfileOutcome.FAILED) {
            return new FfmpegAudioToWavFailure(task.failure(), task);
        }

        PublishedArtifact audio = task.artifacts().get(OUTPUT_SLOT);
        if (audio == null || task.artifacts().size() != 1) {
            throw new TaskCageProtocolException(
                    "FFmpeg Profile success must contain only the audio output Artifact");
        }
        if (!OUTPUT_MEDIA_TYPE.equals(audio.mediaType())) {
            throw new TaskCageProtocolException(
                    "FFmpeg Profile audio output must use media type " + OUTPUT_MEDIA_TYPE);
        }
        String expectedPath = "tasks/" + task.taskId() + "/" + OUTPUT_FILE_NAME;
        if (!expectedPath.equals(audio.path().value())) {
            throw new TaskCageProtocolException(
                    "FFmpeg Profile audio output must use path " + expectedPath);
        }
        return new FfmpegAudioToWavSuccess(audio, task);
    }
}
