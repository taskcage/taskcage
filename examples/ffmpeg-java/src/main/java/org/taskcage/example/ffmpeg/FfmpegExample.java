package org.taskcage.example.ffmpeg;

import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.HexFormat;
import org.taskcage.binding.ffmpeg.AudioChannels;
import org.taskcage.binding.ffmpeg.AudioSampleRate;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavBinding;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavFailure;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavRequest;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavResult;
import org.taskcage.binding.ffmpeg.FfmpegAudioToWavSuccess;
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;

public final class FfmpegExample {
    private FfmpegExample() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 3) {
            throw new IllegalArgumentException(
                    "usage: FfmpegExample <socket-path> <artifact-root> <input-relative-path>");
        }
        Path socket = Path.of(args[0]);
        Path artifactRoot = Path.of(args[1]);
        ArtifactPath inputPath = new ArtifactPath(args[2]);
        byte[] input = Files.readAllBytes(artifactRoot.resolve(inputPath.value()));
        LocalInputArtifact source = new LocalInputArtifact(
                inputPath,
                new Sha256Digest("sha256:" + HexFormat.of().formatHex(
                        MessageDigest.getInstance("SHA-256").digest(input))),
                input.length);

        try (TaskCageClient taskCage = TaskCageClient.connect(
                TaskCageClientConfig.builder().socketPath(socket).build())) {
            FfmpegAudioToWavResult result = FfmpegAudioToWavBinding.using(taskCage)
                    .run(
                            new FfmpegAudioToWavRequest(
                                    source,
                                    AudioSampleRate.HZ_16000,
                                    AudioChannels.MONO),
                            Duration.ofSeconds(30));

            if (result instanceof FfmpegAudioToWavSuccess success) {
                System.out.println(artifactRoot.resolve(success.audio().path().value()));
            } else if (result instanceof FfmpegAudioToWavFailure failure) {
                throw new IllegalStateException(
                        failure.failure().code() + ": " + failure.failure().message());
            }
        }
    }
}
