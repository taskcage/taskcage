package org.taskcage.binding.ffmpeg;

/** Channel layouts supported by the first audio-to-WAV Execution Profile. */
public enum AudioChannels {
    /** One output channel. */
    MONO(1),
    /** Two output channels. */
    STEREO(2);

    private final long count;

    AudioChannels(long count) {
        this.count = count;
    }

    /**
     * Returns the channel count sent to the Profile input slot.
     *
     * @return one for mono or two for stereo
     */
    public long count() {
        return count;
    }
}
