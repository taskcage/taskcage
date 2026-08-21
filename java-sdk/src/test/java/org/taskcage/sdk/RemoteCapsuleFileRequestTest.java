package org.taskcage.sdk;

import java.nio.file.Path;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

class RemoteCapsuleFileRequestTest {
    @Test
    void buildsAPathOwnedRequestWithoutExposingThePathAsAProfileInput() {
        RemoteCapsuleFileRequest request = RemoteCapsuleFileRequest
                .builder("ffmpeg-audio-to-wav", "1.0.0")
                .inputFile("source", Path.of("input.wav"), "audio/wav")
                .int64("sample_rate_hz", 16_000)
                .int64("channels", 1)
                .outputFile("audio", Path.of("output.wav"))
                .build();

        assertEquals(new CapsuleIdentity("ffmpeg-audio-to-wav", "1.0.0"), request.capsule());
        assertEquals(java.util.List.of("channels", "sample_rate_hz"), request.inputs().keySet().stream().toList());
        assertEquals("source", request.inputSlot());
        assertEquals("audio", request.outputSlot());
    }

    @Test
    void rejectsAnInputValueThatWouldReplaceTheManagedFileArtifact() {
        assertThrows(IllegalArgumentException.class, () -> RemoteCapsuleFileRequest
                .builder("ffmpeg-audio-to-wav", "1.0.0")
                .inputFile("source", Path.of("input.wav"), "audio/wav")
                .input("source", new RemoteStringInput("not-an-artifact"))
                .outputFile("audio", Path.of("output.wav"))
                .build());
    }
}
