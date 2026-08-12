package org.taskcage.binding.ffmpeg;

import java.util.Objects;
import org.taskcage.sdk.FinishedProfileTaskSnapshot;
import org.taskcage.sdk.ProfileFailure;
import org.taskcage.sdk.ProfileOutcome;

/** Failed audio-to-WAV result that preserves the Core Profile failure contract. */
public record FfmpegAudioToWavFailure(
        ProfileFailure failure, FinishedProfileTaskSnapshot task)
        implements FfmpegAudioToWavResult {
    /**
     * Validates that the failure belongs to the failed Core result.
     *
     * @param failure stable Profile failure details
     * @param task failed cleanup-confirmed Core result
     */
    public FfmpegAudioToWavFailure {
        Objects.requireNonNull(failure, "failure");
        Objects.requireNonNull(task, "task");
        if (task.profileOutcome() != ProfileOutcome.FAILED) {
            throw new IllegalArgumentException("a failed Binding result requires a failed Profile Task");
        }
        if (!failure.equals(task.failure())) {
            throw new IllegalArgumentException("failure must be the Profile Task's failure");
        }
    }
}
