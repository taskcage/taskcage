package org.taskcage.sdk;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;
import java.util.UUID;
import java.util.function.Function;
import org.junit.jupiter.api.Test;
import org.taskcage.sdk.support.FakeTaskCageServer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ProfileProtocolFixtureCompatibilityTest {
    private static final String FIXTURES_PROPERTY = "taskcage.protocolFixturesDir";
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final FixtureCorpus PROTOCOL_V2 = new FixtureCorpus("v2");
    private static final UUID CLIENT_REQUEST_ID =
            UUID.fromString("22222222-2222-4222-8222-222222222222");
    private static final UUID TASK_ID = UUID.fromString("44444444-4444-4444-8444-444444444444");
    private static final ProfileIdentity PROFILE = new ProfileIdentity("file-copy", "1.0.0");
    private static final Set<String> EXPECTED_FIXTURES = Set.of(
            "artifact-input-digest-mismatch.json",
            "artifact-input-invalid-path.json",
            "artifact-input-valid.json",
            "artifact-output-undeclared.json",
            "error-artifact-digest-mismatch.json",
            "error-profile-not-found.json",
            "get-profile-result.json",
            "profile-accepted.json",
            "profile-result-output-contract-failed.json",
            "profile-result-running.json",
            "profile-result-success.json",
            "submit-profile-invalid-input.json",
            "submit-profile-valid.json");

    @Test
    void sharedProfileV2FixtureCorpusIsCompleteAndValidJson() throws Exception {
        assertEquals(new TreeSet<>(EXPECTED_FIXTURES), PROTOCOL_V2.names());
        for (String name : EXPECTED_FIXTURES) {
            PROTOCOL_V2.read(name);
        }
    }

    @Test
    void validProfileRequestMatchesTheSharedFixtureAndAcceptedResponseDecodes() throws Exception {
        ObjectNode accepted = PROTOCOL_V2.read("profile-accepted.json");
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        ProfileProtocolFixtureCompatibilityTest::capabilitiesResponse,
                        respondWith(accepted)));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            ProfileTask task = assertInstanceOf(
                    ProfileTask.class, client.submitProfile(CLIENT_REQUEST_ID, validRequest()));

            assertEquals(TASK_ID, task.taskId());
            assertEquals(PROFILE, task.profile());
            assertEquals(Duration.ofMinutes(5), task.effectiveResources().wallTimeLimit());
            assertEquals(65_536, task.effectiveResources().stdoutTailMaxBytes());

            server.awaitRequests(Duration.ofSeconds(2));
            JsonNode actual = server.requests().get(1);
            JsonNode expected = PROTOCOL_V2.read("submit-profile-valid.json");
            assertEquals(expected.path("protocolVersion"), actual.path("protocolVersion"));
            assertEquals(expected.path("type"), actual.path("type"));
            assertEquals(expected.path("payload"), actual.path("payload"));
        }
    }

    @Test
    void runningAndFinishedProfileResultFixturesDecode() throws Exception {
        RunningProfileTaskSnapshot running = assertInstanceOf(
                RunningProfileTaskSnapshot.class,
                getFixture("profile-result-running.json"));
        assertEquals(PROFILE, running.profile());

        FinishedProfileTaskSnapshot succeeded = assertInstanceOf(
                FinishedProfileTaskSnapshot.class,
                getFixture("profile-result-success.json"));
        assertEquals(ProfileOutcome.SUCCEEDED, succeeded.profileOutcome());
        assertEquals("text/plain", succeeded.artifacts().get("result").mediaType());
        assertEquals(12, succeeded.artifacts().get("result").sizeBytes());

        FinishedProfileTaskSnapshot failed = assertInstanceOf(
                FinishedProfileTaskSnapshot.class,
                getFixture("profile-result-output-contract-failed.json"));
        assertEquals(ProfileOutcome.FAILED, failed.profileOutcome());
        assertEquals("OUTPUT_CONTRACT_VIOLATION", failed.failure().code());
        assertTrue(failed.artifacts().isEmpty());
    }

    @Test
    void profileErrorsKeepDaemonErrorClassification() throws Exception {
        assertErrorFixture("error-profile-not-found.json", "PROFILE_NOT_FOUND");
        assertErrorFixture("error-artifact-digest-mismatch.json", "ARTIFACT_DIGEST_MISMATCH");
    }

    @Test
    void unsupportedDaemonIsRejectedWithoutRawCommandFallback() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(
                        request -> capabilitiesResponse(request, List.of(1)));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class,
                    () -> client.submitProfile(CLIENT_REQUEST_ID, validRequest()));

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("getCapabilities"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
        }
    }

    @Test
    void profileRunPollsV2ResultsAndCachesTheFinishedSnapshot() throws Exception {
        ObjectNode accepted = PROTOCOL_V2.read("profile-accepted.json");
        ObjectNode running = PROTOCOL_V2.read("profile-result-running.json");
        ObjectNode success = PROTOCOL_V2.read("profile-result-success.json");
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        ProfileProtocolFixtureCompatibilityTest::capabilitiesResponse,
                        respondWith(accepted),
                        respondWith(running),
                        respondWith(success)));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            ProfileTaskHandle handle = client.submitProfileHandle(CLIENT_REQUEST_ID, validRequest());
            FinishedProfileTaskSnapshot finished = handle.await(
                    Duration.ofSeconds(1), Duration.ofMillis(1));

            assertEquals(ProfileOutcome.SUCCEEDED, finished.profileOutcome());
            assertEquals(finished, handle.get());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(
                    List.of("getCapabilities", "submitProfile", "getProfileResult", "getProfileResult"),
                    server.requests().stream().map(request -> request.path("type").asText()).toList());
            assertEquals(List.of(1, 2, 2, 2), server.requests().stream()
                    .map(request -> request.path("protocolVersion").asInt())
                    .toList());
        }
    }

    @Test
    void profileHandleReusesProtocolV1Cancellation() throws Exception {
        ObjectNode accepted = PROTOCOL_V2.read("profile-accepted.json");
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        ProfileProtocolFixtureCompatibilityTest::capabilitiesResponse,
                        respondWith(accepted),
                        ProfileProtocolFixtureCompatibilityTest::cancelledResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            ProfileTaskHandle handle = client.submitProfileHandle(CLIENT_REQUEST_ID, validRequest());
            TaskCancellation cancellation = handle.cancel();

            assertEquals(TASK_ID, cancellation.taskId());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("getCapabilities", "submitProfile", "cancelTask"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
            assertEquals(List.of(1, 2, 1), server.requests().stream()
                    .map(request -> request.path("protocolVersion").asInt())
                    .toList());
        }
    }

    private static ProfileTaskSnapshot getFixture(String name) throws Exception {
        ObjectNode fixture = PROTOCOL_V2.read(name);
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        ProfileProtocolFixtureCompatibilityTest::capabilitiesResponse,
                        respondWith(fixture)));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            ProfileTaskSnapshot snapshot = client.getProfileResult(TASK_ID);
            server.awaitRequests(Duration.ofSeconds(2));
            JsonNode actual = server.requests().get(1);
            JsonNode expected = PROTOCOL_V2.read("get-profile-result.json");
            assertEquals(expected.path("protocolVersion"), actual.path("protocolVersion"));
            assertEquals(expected.path("type"), actual.path("type"));
            assertEquals(expected.path("payload"), actual.path("payload"));
            return snapshot;
        }
    }

    private static void assertErrorFixture(String name, String expectedCode) throws Exception {
        ObjectNode fixture = PROTOCOL_V2.read(name);
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        ProfileProtocolFixtureCompatibilityTest::capabilitiesResponse,
                        respondWith(fixture)));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageDaemonException error = assertThrows(
                    TaskCageDaemonException.class,
                    () -> client.submitProfile(CLIENT_REQUEST_ID, validRequest()));
            assertEquals(expectedCode, error.code());
            assertEquals(false, error.retryable());
            server.awaitRequests(Duration.ofSeconds(2));
        }
    }

    private static ProfileRequest validRequest() {
        return new ProfileRequest(
                PROFILE,
                Map.of(
                        "source",
                        new LocalInputArtifact(
                                new ArtifactPath("jobs/42/source.txt"),
                                new Sha256Digest(
                                        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                                12),
                        "label",
                        new StringProfileInput("archive"),
                        "retainMetadata",
                        new BooleanProfileInput(true),
                        "priority",
                        new Int64ProfileInput(3)),
                ProfileResourceOverrides.builder()
                        .wallTimeLimit(Duration.ofMinutes(5))
                        .build());
    }

    private static Function<JsonNode, JsonNode> respondWith(ObjectNode fixture) {
        return request -> {
            ObjectNode response = fixture.deepCopy();
            response.put("requestId", request.path("requestId").asText());
            return response;
        };
    }

    private static ObjectNode capabilitiesResponse(JsonNode request) {
        return capabilitiesResponse(request, List.of(1, 2));
    }

    private static ObjectNode capabilitiesResponse(JsonNode request, List<Integer> versions) {
        ObjectNode response = MAPPER.createObjectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "capabilities");
        ObjectNode payload = response.putObject("payload");
        payload.put("daemonVersion", "0.2.0");
        payload.putPOJO("protocolVersions", versions);
        payload.put("maxFrameBytes", 1_048_576);
        payload.put("maxConcurrentTasks", 4);
        payload.put("cgroupV2Ready", true);
        return response;
    }

    private static ObjectNode cancelledResponse(JsonNode request) {
        ObjectNode response = MAPPER.createObjectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "taskCancelled");
        ObjectNode payload = response.putObject("payload");
        payload.put("taskId", TASK_ID.toString());
        payload.put("state", "FINISHED");
        payload.put("terminationReason", "CANCELLED");
        return response;
    }

    private static TaskCageClientConfig configFor(FakeTaskCageServer server) {
        return TaskCageClientConfig.builder()
                .socketPath(server.socketPath())
                .connectTimeout(Duration.ofSeconds(1))
                .requestTimeout(Duration.ofSeconds(1))
                .build();
    }

    private record FixtureCorpus(String protocolVersion) {
        private Path root() {
            String configuredRoot = System.getProperty(FIXTURES_PROPERTY);
            if (configuredRoot == null || configuredRoot.isBlank()) {
                throw new IllegalStateException(FIXTURES_PROPERTY + " must point to the shared protocol fixtures");
            }
            return Path.of(configuredRoot)
                    .toAbsolutePath()
                    .normalize()
                    .resolve(protocolVersion)
                    .normalize();
        }

        TreeSet<String> names() throws IOException {
            try (var entries = Files.list(root())) {
                return entries
                        .filter(path -> path.getFileName().toString().endsWith(".json"))
                        .map(path -> path.getFileName().toString())
                        .collect(java.util.stream.Collectors.toCollection(TreeSet::new));
            }
        }

        ObjectNode read(String fixtureName) throws IOException {
            Path versionRoot = root();
            Path fixturePath = versionRoot.resolve(fixtureName).normalize();
            if (!fixturePath.startsWith(versionRoot) || !Files.isRegularFile(fixturePath)) {
                throw new IOException("protocol fixture does not exist: " + fixturePath);
            }
            JsonNode fixture = MAPPER.readTree(fixturePath.toFile());
            if (!(fixture instanceof ObjectNode object)) {
                throw new IOException("protocol fixture must be a JSON object: " + fixturePath);
            }
            return object;
        }
    }
}
