package org.taskcage.binding.ffmpeg;

import java.util.Objects;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProfileResourceOverrides;

/** Type-safe input for the {@code ffmpeg-audio-to-wav@1.0.0} Profile. */
public record FfmpegAudioToWavRequest(
        LocalInputArtifact source,
        AudioSampleRate sampleRate,
        AudioChannels channels,
        ProfileResourceOverrides resourceOverrides) {

    /**
     * Creates a request that uses the installed Profile's resource defaults.
     *
     * @param source caller-owned Local input Artifact
     * @param sampleRate fixed output sample rate
     * @param channels mono or stereo output
     */
    public FfmpegAudioToWavRequest(
            LocalInputArtifact source, AudioSampleRate sampleRate, AudioChannels channels) {
        this(source, sampleRate, channels, ProfileResourceOverrides.none());
    }

    /**
     * Validates all required Binding inputs.
     *
     * @param source caller-owned Local input Artifact
     * @param sampleRate fixed output sample rate
     * @param channels mono or stereo output
     * @param resourceOverrides optional Profile resource overrides
     */
    public FfmpegAudioToWavRequest {
        Objects.requireNonNull(source, "source");
        Objects.requireNonNull(sampleRate, "sampleRate");
        Objects.requireNonNull(channels, "channels");
        Objects.requireNonNull(resourceOverrides, "resourceOverrides");
    }
}
