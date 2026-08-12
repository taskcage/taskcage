package org.taskcage.binding.ffmpeg;

import java.util.Objects;
import org.taskcage.sdk.FinishedProfileTaskSnapshot;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.PublishedArtifact;

/** Successful audio-to-WAV result with its typed output Artifact. */
public record FfmpegAudioToWavSuccess(
        PublishedArtifact audio, FinishedProfileTaskSnapshot task)
        implements FfmpegAudioToWavResult {
    /**
     * Validates that the Artifact belongs to the successful Core result.
     *
     * @param audio typed output Artifact
     * @param task successful cleanup-confirmed Core result
     */
    public FfmpegAudioToWavSuccess {
        Objects.requireNonNull(audio, "audio");
        Objects.requireNonNull(task, "task");
        if (task.profileOutcome() != ProfileOutcome.SUCCEEDED) {
            throw new IllegalArgumentException("a successful Binding result requires a successful Profile Task");
        }
        if (!audio.equals(task.artifacts().get(FfmpegAudioToWavBinding.OUTPUT_SLOT))) {
            throw new IllegalArgumentException("audio must be the Profile Task's audio output Artifact");
        }
    }
}
