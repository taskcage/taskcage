package org.taskcage.sdk;

import java.time.Instant;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.UUID;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

class RemoteProfileRequestTest {
    private static final UUID ARTIFACT_ID = UUID.fromString("55555555-5555-4555-8555-555555555555");
    private static final Sha256Digest DIGEST = new Sha256Digest(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    @Test
    void remoteRequestUsesOnlyDaemonIssuedManagedInputIds() {
        Map<String, RemoteProfileInputValue> mutable = new LinkedHashMap<>();
        mutable.put("source", new ManagedInputArtifact(ARTIFACT_ID));
        mutable.put("priority", new RemoteInt64Input(3));

        RemoteProfileRequest request = new RemoteProfileRequest(
                new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0"), mutable);
        mutable.clear();

        assertEquals(java.util.List.of("priority", "source"), request.inputs().keySet().stream().toList());
        assertThrows(UnsupportedOperationException.class,
                () -> request.inputs().put("source", new ManagedInputArtifact(ARTIFACT_ID)));
        assertThrows(IllegalArgumentException.class, () -> new RemoteProfileRequest(
                new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0"),
                Map.of("Source", new ManagedInputArtifact(ARTIFACT_ID))));
    }

    @Test
    void managedArtifactsExposeOnlyIdsAndVerifiedMetadata() {
        RemoteArtifactUpload upload = new RemoteArtifactUpload(
                ARTIFACT_ID, DIGEST, 12, Instant.parse("2026-08-13T12:10:00Z"));
        ManagedOutputArtifact output = new ManagedOutputArtifact(
                UUID.fromString("88888888-8888-4888-8888-888888888888"),
                DIGEST,
                12,
                "audio/wav",
                Instant.parse("2026-08-13T12:12:00Z"));

        assertEquals(new ManagedInputArtifact(ARTIFACT_ID), upload.asInput());
        assertEquals("audio/wav", output.mediaType());
        assertThrows(IllegalArgumentException.class, () -> new RemoteArtifactUpload(
                ARTIFACT_ID, DIGEST, 0, Instant.parse("2026-08-13T12:10:00Z")));
    }

    @Test
    void remoteCapsuleRequestRequiresTheExactProfileIdentity() {
        RemoteProfileRequest profile = new RemoteProfileRequest(
                new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0"),
                Map.of("source", new ManagedInputArtifact(ARTIFACT_ID)));

        assertThrows(
                IllegalArgumentException.class,
                () -> new RemoteCapsuleRequest(new CapsuleIdentity("another-capsule", "1.0.0"), profile));
    }
}
