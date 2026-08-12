package org.taskcage.binding.ffmpeg;

/** Sample rates accepted by the first audio-to-WAV Execution Profile. */
public enum AudioSampleRate {
    /** 8,000 Hz. */
    HZ_8000(8_000),
    /** 16,000 Hz. */
    HZ_16000(16_000),
    /** 22,050 Hz. */
    HZ_22050(22_050),
    /** 44,100 Hz. */
    HZ_44100(44_100),
    /** 48,000 Hz. */
    HZ_48000(48_000);

    private final long hertz;

    AudioSampleRate(long hertz) {
        this.hertz = hertz;
    }

    /**
     * Returns the integer Hertz value sent to the Profile input slot.
     *
     * @return sample rate in Hertz
     */
    public long hertz() {
        return hertz;
    }
}
