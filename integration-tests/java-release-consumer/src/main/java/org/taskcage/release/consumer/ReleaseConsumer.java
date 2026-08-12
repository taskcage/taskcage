package org.taskcage.release.consumer;

import java.time.Duration;
import java.util.concurrent.TimeoutException;
import org.taskcage.binding.ffmpeg.AudioChannels;
import org.taskcage.binding.ffmpeg.AudioSampleRate;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavBinding;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavRequest;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavResult;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.TaskCageClient;

public final class ReleaseConsumer {
    private ReleaseConsumer() {}

    public static FfmpegAudioToWavResult transcode(
            TaskCageClient client, LocalInputArtifact source)
            throws InterruptedException, TimeoutException {
        return FfmpegAudioToWavBinding.using(client)
                .run(
                        new FfmpegAudioToWavRequest(
                                source, AudioSampleRate.HZ_16000, AudioChannels.MONO),
                        Duration.ofSeconds(30));
    }
}
