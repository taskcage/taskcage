package org.taskcage.binding.ffmpeg;

import org.taskcage.sdk.FinishedProfileTaskSnapshot;

/** Type-safe success or failure returned by the audio-to-WAV Binding. */
public sealed interface FfmpegAudioToWavResult
        permits FfmpegAudioToWavSuccess, FfmpegAudioToWavFailure {
    /**
     * Returns the complete Core SDK result, including termination evidence and resource usage.
     *
     * @return cleanup-confirmed Profile Task snapshot
     */
    FinishedProfileTaskSnapshot task();
}
