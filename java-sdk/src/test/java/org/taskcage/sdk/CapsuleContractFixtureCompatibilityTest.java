package org.taskcage.sdk;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.time.Instant;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.UUID;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class CapsuleContractFixtureCompatibilityTest {
    private static final String FIXTURES_PROPERTY = "taskcage.protocolFixturesDir";
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final Set<String> EXPECTED_FIXTURES = Set.of(
            "error-capsule-profile-mismatch.json",
            "request-valid.json",
            "result-cancelled.json",
            "result-failed.json",
            "result-output-contract-failed.json",
            "result-success.json",
            "result-timeout.json");

    @Test
    void sharedCapsuleV1FixtureCorpusIsCompleteAndValidJson() throws Exception {
        assertEquals(new TreeSet<>(EXPECTED_FIXTURES), fixtureNames());
        for (String name : EXPECTED_FIXTURES) {
            read(name);
        }
    }

    @Test
    void validRequestFixtureConstructsLocalAndRemoteCapsuleRequests() throws Exception {
        ObjectNode fixture = read("request-valid.json");
        CapsuleIdentity capsule = capsuleIdentity(fixture.path("capsule"));
        ProfileIdentity profile = profileIdentity(fixture.path("profile"));
        JsonNode inputs = fixture.path("inputs");
        JsonNode source = inputs.path("source");
        JsonNode limits = fixture.path("resourceOverrides").path("limits");
        ProfileResourceOverrides overrides = ProfileResourceOverrides.builder()
                .wallTimeLimit(Duration.ofMillis(limits.path("wallTimeLimitMs").longValue()))
                .build();

        LocalInputArtifact localSource = new LocalInputArtifact(
                new ArtifactPath("jobs/capsule-v1/source.mp3"),
                new Sha256Digest(source.path("digest").textValue()),
                source.path("sizeBytes").longValue());
        ProfileRequest localProfile = new ProfileRequest(
                profile,
                Map.of(
                        "source", localSource,
                        "sample_rate_hz", new Int64ProfileInput(inputs.path("sample_rate_hz").path("value").longValue()),
                        "channels", new Int64ProfileInput(inputs.path("channels").path("value").longValue())),
                overrides);
        CapsuleRequest local = new CapsuleRequest(capsule, localProfile);

        UUID managedArtifactId = UUID.fromString("33333333-3333-4333-8333-333333333333");
        RemoteProfileRequest remoteProfile = new RemoteProfileRequest(
                profile,
                Map.of(
                        "source", new ManagedInputArtifact(managedArtifactId),
                        "sample_rate_hz", new RemoteInt64Input(inputs.path("sample_rate_hz").path("value").longValue()),
                        "channels", new RemoteInt64Input(inputs.path("channels").path("value").longValue())),
                overrides);
        RemoteCapsuleRequest remote = new RemoteCapsuleRequest(capsule, remoteProfile);

        assertEquals(capsule.name(), profile.name());
        assertEquals(capsule.version(), profile.version());
        assertEquals("ARTIFACT", source.path("kind").textValue());
        assertEquals("audio/mpeg", source.path("mediaType").textValue());
        assertEquals(localSource, local.profileRequest().inputs().get("source"));
        assertEquals(new ManagedInputArtifact(managedArtifactId), remote.profileRequest().inputs().get("source"));
        assertEquals(Duration.ofMinutes(2), local.profileRequest().resourceOverrides().wallTimeLimit().orElseThrow());
        assertEquals(overrides, remote.profileRequest().resourceOverrides());
    }

    @Test
    void mismatchFixtureMapsToStableNonRetryablePreExecutionError() throws Exception {
        ObjectNode fixture = read("error-capsule-profile-mismatch.json");
        JsonNode request = fixture.path("request");
        CapsuleIdentity capsule = capsuleIdentity(request.path("capsule"));
        ProfileIdentity profile = profileIdentity(request.path("profile"));
        ProfileRequest localProfile = new ProfileRequest(
                profile, Map.of("sample_rate_hz", new Int64ProfileInput(16_000)));
        RemoteProfileRequest remoteProfile = new RemoteProfileRequest(
                profile, Map.of("sample_rate_hz", new RemoteInt64Input(16_000)));

        CapsuleContractException local = assertThrows(
                CapsuleContractException.class,
                () -> new CapsuleRequest(capsule, localProfile));
        CapsuleContractException remote = assertThrows(
                CapsuleContractException.class,
                () -> new RemoteCapsuleRequest(capsule, remoteProfile));

        String expectedCode = fixture.path("error").path("code").textValue();
        boolean expectedRetryable = fixture.path("error").path("retryable").booleanValue();
        assertEquals(expectedCode, local.code());
        assertEquals(expectedCode, remote.code());
        assertEquals(expectedRetryable, local.retryable());
        assertEquals(expectedRetryable, remote.retryable());
        assertEquals(0, request.path("inputs").size());
        assertEquals(0, request.path("resourceOverrides").size());
        for (Map.Entry<String, JsonNode> effect : fixture.path("sideEffects").properties()) {
            assertFalse(effect.getValue().booleanValue(), effect.getKey());
        }
    }

    @Test
    void terminalFixturesConstructCleanupConfirmedSdkResults() throws Exception {
        Map<String, String> expectedFailures = Map.of(
                "result-cancelled.json", "CANCELLED",
                "result-failed.json", "PROCESS_FAILED",
                "result-output-contract-failed.json", "OUTPUT_CONTRACT_VIOLATION",
                "result-timeout.json", "TIMEOUT");

        for (String name : EXPECTED_FIXTURES.stream()
                .filter(fixture -> fixture.startsWith("result-"))
                .sorted()
                .toList()) {
            ObjectNode fixture = read(name);
            CapsuleExecutionResult result = resultFrom(fixture);

            assertEquals(fixture.path("capsule").path("name").textValue(), result.capsule().name(), name);
            assertEquals(fixture.path("capsule").path("version").textValue(), result.capsule().version(), name);
            assertEquals(fixture.path("profile").path("name").textValue(), result.profileTask().profile().name(), name);
            assertEquals(TaskState.valueOf(fixture.path("state").textValue()), result.profileTask().state(), name);
            assertEquals(ProfileOutcome.valueOf(fixture.path("outcome").textValue()), result.outcome(), name);
            assertEquals(
                    TerminationReason.valueOf(fixture.path("terminationReason").textValue()),
                    result.execution().terminationReason(),
                    name);
            assertEquals(fixture.path("cleanupConfirmed").booleanValue(), result.cleanupConfirmed(), name);

            if (result.outcome() == ProfileOutcome.SUCCEEDED) {
                assertEquals(Set.of("audio"), result.profileTask().artifacts().keySet(), name);
                assertTrue(fixture.path("failure").isNull(), name);
            } else {
                assertTrue(result.profileTask().artifacts().isEmpty(), name);
                assertEquals(expectedFailures.get(name), result.profileTask().failure().code(), name);
            }
        }
    }

    private static CapsuleExecutionResult resultFrom(ObjectNode fixture) {
        JsonNode process = fixture.path("process");
        JsonNode timing = fixture.path("timing");
        JsonNode usage = fixture.path("usage");
        JsonNode output = fixture.path("output");
        ExecutionResult execution = new ExecutionResult(
                TerminationReason.valueOf(fixture.path("terminationReason").textValue()),
                new ProcessResult(nullableInt(process.path("exitCode")), nullableText(process.path("signal"))),
                new TaskTiming(
                        Instant.parse(timing.path("submittedAt").textValue()),
                        Instant.parse(timing.path("startedAt").textValue()),
                        Instant.parse(timing.path("finishedAt").textValue()),
                        Duration.ofMillis(timing.path("wallTimeMs").longValue())),
                new TaskUsage(
                        usage.path("cpuTimeMicros").longValue(),
                        usage.path("memoryPeakBytes").longValue()),
                new TaskOutput(
                        output.path("stdoutTail").textValue(),
                        output.path("stderrTail").textValue(),
                        output.path("stdoutTruncated").booleanValue(),
                        output.path("stderrTruncated").booleanValue()));

        Map<String, PublishedArtifact> artifacts = new TreeMap<>();
        for (Map.Entry<String, JsonNode> entry : fixture.path("artifacts").properties()) {
            JsonNode artifact = entry.getValue();
            assertEquals("ARTIFACT", artifact.path("kind").textValue());
            artifacts.put(entry.getKey(), new PublishedArtifact(
                    new ArtifactPath(artifact.path("path").textValue()),
                    new Sha256Digest(artifact.path("digest").textValue()),
                    artifact.path("sizeBytes").longValue(),
                    artifact.path("mediaType").textValue()));
        }

        JsonNode failureNode = fixture.path("failure");
        ProfileFailure failure = failureNode.isNull()
                ? null
                : new ProfileFailure(
                        failureNode.path("code").textValue(),
                        failureNode.path("message").textValue());
        ProfileIdentity profile = profileIdentity(fixture.path("profile"));
        FinishedProfileTaskSnapshot snapshot = new FinishedProfileTaskSnapshot(
                UUID.fromString(fixture.path("taskId").textValue()),
                profile,
                ProfileOutcome.valueOf(fixture.path("outcome").textValue()),
                execution,
                artifacts,
                failure);
        return new CapsuleExecutionResult(capsuleIdentity(fixture.path("capsule")), snapshot);
    }

    private static Integer nullableInt(JsonNode value) {
        return value.isNull() ? null : value.intValue();
    }

    private static String nullableText(JsonNode value) {
        return value.isNull() ? null : value.textValue();
    }

    private static CapsuleIdentity capsuleIdentity(JsonNode node) {
        return new CapsuleIdentity(node.path("name").textValue(), node.path("version").textValue());
    }

    private static ProfileIdentity profileIdentity(JsonNode node) {
        return new ProfileIdentity(node.path("name").textValue(), node.path("version").textValue());
    }

    private static ObjectNode read(String fixtureName) throws IOException {
        Path fixture = fixtureRoot().resolve(fixtureName).normalize();
        if (!fixture.startsWith(fixtureRoot()) || !Files.isRegularFile(fixture)) {
            throw new IOException("Capsule fixture does not exist: " + fixture);
        }
        JsonNode value = MAPPER.readTree(fixture.toFile());
        if (!(value instanceof ObjectNode object)) {
            throw new IOException("Capsule fixture must be a JSON object: " + fixture);
        }
        return object;
    }

    private static Set<String> fixtureNames() throws IOException {
        try (var entries = Files.list(fixtureRoot())) {
            return entries
                    .filter(Files::isRegularFile)
                    .map(path -> path.getFileName().toString())
                    .filter(name -> name.endsWith(".json"))
                    .collect(java.util.stream.Collectors.toCollection(TreeSet::new));
        }
    }

    private static Path fixtureRoot() {
        String configuredRoot = System.getProperty(FIXTURES_PROPERTY);
        if (configuredRoot == null || configuredRoot.isBlank()) {
            throw new IllegalStateException(FIXTURES_PROPERTY + " must point to the shared protocol fixtures");
        }
        return Path.of(configuredRoot).toAbsolutePath().normalize().resolve("capsule-v1").normalize();
    }
}
