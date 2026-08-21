package org.taskcage.example.ffmpeg;

import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.HexFormat;
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.CapsuleExecutionResult;
import org.taskcage.sdk.CapsuleRequest;
import org.taskcage.sdk.CapsuleRunner;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.PublishedArtifact;
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
            CapsuleExecutionResult result = CapsuleRunner.external(taskCage).execute(
                    CapsuleRequest.builder("ffmpeg-audio-to-wav", "1.0.0")
                            .artifact("source", source)
                            .int64("sample_rate_hz", 16_000)
                            .int64("channels", 1)
                            .build(),
                    Duration.ofSeconds(30));
            if (result.outcome() != ProfileOutcome.SUCCEEDED) {
                throw new IllegalStateException(
                        result.execution().terminationReason() + ": " + result.execution());
            }
            PublishedArtifact audio = result.profileTask().artifacts().get("audio");
            System.out.println(artifactRoot.resolve(audio.path().value()));
        }
    }
}
