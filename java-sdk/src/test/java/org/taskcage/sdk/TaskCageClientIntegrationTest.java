package org.taskcage.sdk;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.fasterxml.jackson.databind.node.JsonNodeFactory;
import org.taskcage.sdk.support.FakeTaskCageServer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.locks.LockSupport;
import java.util.function.Function;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class TaskCageClientIntegrationTest {
    @Test
    void capabilitiesUsesProtocolV1OverUnixDomainSocket() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::capabilitiesResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageCapabilities capabilities = client.capabilities();

            assertEquals("0.1.0", capabilities.daemonVersion());
            assertEquals(List.of(1), capabilities.protocolVersions());
            assertEquals(4, capabilities.maxConcurrentTasks());
            assertTrue(capabilities.cgroupV2Ready());

            server.awaitRequests(Duration.ofSeconds(2));
            JsonNode request = server.requests().get(0);
            assertEquals(1, request.path("protocolVersion").asInt());
            assertEquals("getCapabilities", request.path("type").asText());
            assertTrue(request.path("payload").isObject());
        }
    }

    @Test
    void capabilitiesReconnectsOnceAfterResponseIsLost() throws Exception {
        Function<JsonNode, JsonNode> closeWithoutResponse = ignored -> null;
        try (FakeTaskCageServer server = FakeTaskCageServer.start(
                List.of(closeWithoutResponse, TaskCageClientIntegrationTest::capabilitiesResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageCapabilities capabilities = client.capabilities();

            assertTrue(capabilities.cgroupV2Ready());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(2, server.requests().size());
        }
    }

    @Test
    void mismatchedRequestIdIsReportedAsProtocolFailure() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(request -> {
            ObjectNode response = capabilitiesResponse(request);
            response.put("requestId", "different-request-id");
            return response;
        });
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, client::capabilities);
            server.awaitRequests(Duration.ofSeconds(2));
        }
    }

    @Test
    void malformedJsonResponseIsReportedAsProtocolFailureWithoutRetrying() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startRaw(ignored -> "{".getBytes(StandardCharsets.UTF_8));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, client::capabilities);
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(1, server.requests().size());
        }
    }

    @Test
    void duplicateJsonKeysAreReportedAsProtocolFailure() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startRaw(request -> (
                "{\"protocolVersion\":1,\"requestId\":\"" + request.path("requestId").asText()
                        + "\",\"requestId\":\"other\",\"type\":\"capabilities\",\"payload\":{}}")
                .getBytes(StandardCharsets.UTF_8));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, client::capabilities);
            server.awaitRequests(Duration.ofSeconds(2));
        }
    }

    @Test
    void submitEncodesMandatoryLimitsAndDecodesAcceptedTask() throws Exception {
        Path program = absoluteTestPath("true");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskSubmission submission = client.submit(new TaskSpec(
                    new ExternalCommand(program, List.of(), absoluteTestPath("jobs/42"),
                            java.util.Map.of("LANG", "C.UTF-8")),
                    new ResourceBudget(new CpuQuota(100_000, 100_000), 536_870_912, 32,
                            Duration.ofMinutes(2), 1_024, 2_048)));

            Task task = (Task) submission;
            assertEquals("b5309d98-f51e-45e1-9866-b1a080c1ba50", task.taskId().toString());
            assertEquals(1_024, task.effectiveBudget().stdoutTailMaxBytes());
            server.awaitRequests(Duration.ofSeconds(2));
            JsonNode payload = server.requests().get(0).path("payload");
            assertEquals("submitTask", server.requests().get(0).path("type").asText());
            assertEquals(program.toString(), payload.path("command").path("program").asText());
            assertEquals(536_870_912, payload.path("limits").path("memoryMaxBytes").asLong());
        }
    }

    @Test
    void submitRejectsAnInvalidWireDurationBeforeAnyDaemonRequest() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            ExternalCommand command = new ExternalCommand(
                    absoluteTestPath("true"), List.of(), absoluteTestPath("tmp"), java.util.Map.of());

            assertThrows(IllegalArgumentException.class, () -> client.submit(new TaskSpec(
                    command,
                    new ResourceBudget(new CpuQuota(100_000, 100_000), 64L * 1024 * 1024, 8,
                            Duration.ofNanos(1), 1_024, 1_024))));

            assertTrue(server.requests().isEmpty());
        }
    }

    @Test
    void submitUsesCallerSuppliedIdempotencyKey() throws Exception {
        UUID requestId = UUID.fromString("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            client.submit(requestId, new TaskSpec(
                    new ExternalCommand(absoluteTestPath("true"), List.of(),
                            absoluteTestPath("tmp"), java.util.Map.of()),
                    new ResourceBudget(new CpuQuota(100_000, 100_000), 64L * 1024 * 1024, 8,
                            Duration.ofSeconds(10), 1_024, 1_024)));
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(requestId.toString(), server.requests().get(0).path("payload").path("clientRequestId").asText());
        }
    }

    @Test
    void submitHandlePreservesCallerSuppliedIdempotencyKey() throws Exception {
        UUID requestId = UUID.fromString("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(requestId, testTaskSpec());

            assertEquals(UUID.fromString("b5309d98-f51e-45e1-9866-b1a080c1ba50"), handle.taskId());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(requestId.toString(), server.requests().get(0).path("payload").path("clientRequestId").asText());
        }
    }

    @Test
    void submitHandleCachesAnImmediateFinishedSubmission() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::executionFailedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());

            FinishedTaskSnapshot finished = handle.await(Duration.ofSeconds(1));
            assertEquals(TerminationReason.EXECUTION_FAILED, finished.result().terminationReason());
            assertEquals(finished, handle.get());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(1, server.requests().size());
        }
    }

    @Test
    void taskHandleAwaitsFinishedSnapshotWithoutBackgroundPolling() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        TaskCageClientIntegrationTest::taskAcceptedResponse,
                        TaskCageClientIntegrationTest::runningTaskResponse,
                        TaskCageClientIntegrationTest::finishedTaskResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());

            FinishedTaskSnapshot finished = handle.await(Duration.ofSeconds(1), Duration.ofMillis(1));

            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
            assertEquals(finished, handle.get());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("submitTask", "getTask", "getTask"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
        }
    }

    @Test
    void taskHandleTimeoutDoesNotCancelTheTask() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        TaskCageClientIntegrationTest::taskAcceptedResponse,
                        TaskCageClientIntegrationTest::runningTaskResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());

            assertThrows(TimeoutException.class,
                    () -> handle.await(Duration.ofMillis(20), Duration.ofMillis(100)));

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("submitTask", "getTask"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
        }
    }

    @Test
    void taskHandleBoundsSlowGetRequestByOverallDeadline() throws Exception {
        Function<JsonNode, JsonNode> slowNoResponse = ignored -> {
            LockSupport.parkNanos(Duration.ofMillis(200).toNanos());
            return null;
        };
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(
                        List.of(TaskCageClientIntegrationTest::taskAcceptedResponse, slowNoResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());

            assertThrows(TimeoutException.class,
                    () -> handle.await(Duration.ofMillis(30), Duration.ofMillis(1)));

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("submitTask", "getTask"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
        }
    }

    @Test
    void taskHandleAwaitPreservesInterruptStatus() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());
            Thread.currentThread().interrupt();
            try {
                assertThrows(InterruptedException.class, () -> handle.await(Duration.ofSeconds(1)));
                assertTrue(Thread.currentThread().isInterrupted());
            } finally {
                Thread.interrupted();
            }

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(1, server.requests().size());
        }
    }

    @Test
    void taskHandleCancelUsesCleanupConfirmedDaemonCancellation() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        TaskCageClientIntegrationTest::taskAcceptedResponse,
                        TaskCageClientIntegrationTest::taskCancelledResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());

            TaskCancellation cancellation = handle.cancel();

            assertEquals(handle.taskId(), cancellation.taskId());
            assertEquals(TaskState.FINISHED, cancellation.state());
            assertEquals(TerminationReason.CANCELLED, cancellation.terminationReason());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals("cancelTask", server.requests().get(1).path("type").asText());
        }
    }

    @Test
    void taskHandleRejectsInvalidWaitDurationsBeforePolling() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskHandle handle = client.submitHandle(testTaskSpec());

            assertThrows(IllegalArgumentException.class, () -> handle.await(Duration.ZERO));
            assertThrows(IllegalArgumentException.class, () -> handle.await(Duration.ofNanos(-1)));
            assertThrows(IllegalArgumentException.class, () -> handle.await(Duration.ofSeconds(Long.MAX_VALUE)));
            assertThrows(IllegalArgumentException.class,
                    () -> handle.await(Duration.ofSeconds(1), Duration.ZERO));

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(1, server.requests().size());
            assertFalse(Thread.currentThread().isInterrupted());
        }
    }

    @Test
    void runWaitsForCleanupConfirmedFinishedSnapshot() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        TaskCageClientIntegrationTest::taskAcceptedResponse,
                        TaskCageClientIntegrationTest::runningTaskResponse,
                        TaskCageClientIntegrationTest::finishedTaskResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            FinishedTaskSnapshot finished = client.run(testTaskSpec(), Duration.ofSeconds(1));

            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("submitTask", "getTask", "getTask"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
        }
    }

    @Test
    void runReturnsImmediateFinishedSubmissionWithoutPolling() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::executionFailedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            FinishedTaskSnapshot finished = client.run(testTaskSpec(), Duration.ofSeconds(1));

            assertEquals(TerminationReason.EXECUTION_FAILED, finished.result().terminationReason());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(1, server.requests().size());
        }
    }

    @Test
    void runPreservesCallerSuppliedIdempotencyKey() throws Exception {
        UUID requestId = UUID.fromString("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        TaskCageClientIntegrationTest::taskAcceptedResponse,
                        TaskCageClientIntegrationTest::finishedTaskResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            client.run(requestId, testTaskSpec(), Duration.ofSeconds(1));

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(requestId.toString(), server.requests().get(0).path("payload").path("clientRequestId").asText());
        }
    }

    @Test
    void runRejectsInvalidArgumentsBeforeAnyDaemonRequest() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(NullPointerException.class, () -> client.run((TaskSpec) null, Duration.ofSeconds(1)));
            assertThrows(NullPointerException.class, () -> client.run(testTaskSpec(), null));
            assertThrows(IllegalArgumentException.class, () -> client.run(testTaskSpec(), Duration.ZERO));
            assertThrows(IllegalArgumentException.class, () -> client.run(testTaskSpec(), Duration.ofNanos(-1)));
            assertThrows(IllegalArgumentException.class,
                    () -> client.run(testTaskSpec(), Duration.ofSeconds(Long.MAX_VALUE)));
            assertThrows(NullPointerException.class,
                    () -> client.run(null, testTaskSpec(), Duration.ofSeconds(1)));

            assertTrue(server.requests().isEmpty());
        }
    }

    @Test
    void runWaitTimeoutDoesNotCancelTheTask() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startSession(List.of(
                        TaskCageClientIntegrationTest::taskAcceptedResponse,
                        TaskCageClientIntegrationTest::runningTaskResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TimeoutException.class,
                    () -> client.run(testTaskSpec(), Duration.ofMillis(20)));

            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(List.of("submitTask", "getTask"), server.requests().stream()
                    .map(request -> request.path("type").asText())
                    .toList());
        }
    }

    @Test
    void runRejectsPreInterruptedCallWithoutSubmitting() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            Thread.currentThread().interrupt();
            try {
                assertThrows(InterruptedException.class,
                        () -> client.run(testTaskSpec(), Duration.ofSeconds(1)));
                assertTrue(Thread.currentThread().isInterrupted());
            } finally {
                Thread.interrupted();
            }

            assertTrue(server.requests().isEmpty());
        }
    }

    @Test
    void submitDecodesFinishedSnapshotWhenExecutionCannotStart() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::executionFailedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskSubmission submission = client.submit(new TaskSpec(
                    new ExternalCommand(absoluteTestPath("missing"), List.of(),
                            absoluteTestPath("jobs/42"), java.util.Map.of()),
                    new ResourceBudget(new CpuQuota(100_000, 100_000), 536_870_912, 32,
                            Duration.ofMinutes(2), 1_024, 2_048)));

            FinishedTaskSnapshot finished = (FinishedTaskSnapshot) submission;
            assertEquals(TerminationReason.EXECUTION_FAILED, finished.result().terminationReason());
            assertEquals(null, finished.result().process().exitCode());
            assertEquals(null, finished.result().process().signal());
        }
    }

    @Test
    void getTaskDecodesRunningSnapshot() throws Exception {
        UUID taskId = UUID.fromString("b5309d98-f51e-45e1-9866-b1a080c1ba50");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::runningTaskResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskSnapshot snapshot = client.getTask(taskId);

            RunningTaskSnapshot running = (RunningTaskSnapshot) snapshot;
            assertEquals(TaskState.RUNNING, running.state());
            assertEquals(taskId, running.taskId());
            assertEquals("2026-07-20T09:00:00Z", running.startedAt().toString());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals("getTask", server.requests().get(0).path("type").asText());
            assertEquals(taskId.toString(), server.requests().get(0).path("payload").path("taskId").asText());
        }
    }

    @Test
    void getTaskDecodesFinishedSnapshotAndOutput() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::finishedTaskResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            FinishedTaskSnapshot finished = (FinishedTaskSnapshot) client.getTask(
                    UUID.fromString("b5309d98-f51e-45e1-9866-b1a080c1ba50"));

            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
            assertEquals("SIGKILL", finished.result().process().signal());
            assertEquals(120_000, finished.result().timing().wallTime().toMillis());
            assertEquals(48_000, finished.result().usage().cpuTimeMicros());
            assertEquals("", finished.result().output().stdoutTail());
        }
    }

    @Test
    void getTaskRejectsSnapshotForAnotherTask() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::runningTaskResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, () -> client.getTask(UUID.randomUUID()));
        }
    }

    @Test
    void daemonErrorsExposeCodeAndRetryability() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskNotFoundResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageDaemonException exception = assertThrows(TaskCageDaemonException.class,
                    () -> client.getTask(UUID.randomUUID()));

            assertEquals("TASK_NOT_FOUND", exception.code());
            assertTrue(!exception.retryable());
        }
    }

    @Test
    void deploymentPolicyErrorsExposeCodeAndNonRetryability() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(
                        TaskCageClientIntegrationTest::limitExceedsPolicyResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            ExternalCommand command = new ExternalCommand(
                    absoluteTestPath("true"), List.of(), absoluteTestPath("tmp"), java.util.Map.of());

            TaskCageDaemonException exception = assertThrows(
                    TaskCageDaemonException.class, () -> client.submit(new TaskSpec(command)));

            assertEquals("LIMIT_EXCEEDS_POLICY", exception.code());
            assertTrue(!exception.retryable());
        }
    }

    @Test
    void cancelTaskEncodesTaskIdAndDecodesCancellation() throws Exception {
        UUID taskId = UUID.fromString("b5309d98-f51e-45e1-9866-b1a080c1ba50");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskCancelledResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCancellation cancellation = client.cancelTask(taskId);

            assertEquals(taskId, cancellation.taskId());
            assertEquals(TaskState.FINISHED, cancellation.state());
            assertEquals(TerminationReason.CANCELLED, cancellation.terminationReason());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals("cancelTask", server.requests().get(0).path("type").asText());
            assertEquals(taskId.toString(), server.requests().get(0).path("payload").path("taskId").asText());
        }
    }

    private static TaskCageClientConfig configFor(FakeTaskCageServer server) {
        return TaskCageClientConfig.builder()
                .socketPath(server.socketPath())
                .connectTimeout(Duration.ofSeconds(1))
                .requestTimeout(Duration.ofSeconds(1))
                .build();
    }

    private static Path absoluteTestPath(String name) {
        return Path.of("").toAbsolutePath().resolve(name);
    }

    private static TaskSpec testTaskSpec() {
        return new TaskSpec(
                new ExternalCommand(absoluteTestPath("true"), List.of(), absoluteTestPath("tmp"), java.util.Map.of()),
                new ResourceBudget(
                        new CpuQuota(100_000, 100_000),
                        64L * 1024 * 1024,
                        8,
                        Duration.ofSeconds(10),
                        1_024,
                        1_024));
    }

    private static ObjectNode capabilitiesResponse(JsonNode request) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "capabilities");
        ObjectNode payload = response.putObject("payload");
        payload.put("daemonVersion", "0.1.0");
        payload.putArray("protocolVersions").add(1);
        payload.put("maxFrameBytes", 1_048_576);
        payload.put("maxConcurrentTasks", 4);
        payload.put("cgroupV2Ready", request.path("requestId").asText().length() > 0);
        return response;
    }

    private static ObjectNode taskAcceptedResponse(JsonNode request) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "taskAccepted");
        ObjectNode payload = response.putObject("payload");
        payload.put("taskId", "b5309d98-f51e-45e1-9866-b1a080c1ba50");
        payload.put("state", "RUNNING");
        ObjectNode limits = payload.putObject("effectiveLimits");
        ObjectNode cpu = limits.putObject("cpuMax");
        cpu.put("quotaMicros", 100_000);
        cpu.put("periodMicros", 100_000);
        limits.put("memoryMaxBytes", 536_870_912);
        limits.put("pidsMax", 32);
        limits.put("wallTimeLimitMs", 120_000);
        return response;
    }

    private static ObjectNode runningTaskResponse(JsonNode request) {
        ObjectNode response = taskResponse(request, "RUNNING");
        ObjectNode payload = (ObjectNode) response.path("payload");
        payload.put("submittedAt", "2026-07-20T09:00:00Z");
        payload.put("startedAt", "2026-07-20T09:00:00Z");
        return response;
    }

    private static ObjectNode finishedTaskResponse(JsonNode request) {
        ObjectNode response = taskResponse(request, "FINISHED");
        ObjectNode payload = (ObjectNode) response.path("payload");
        payload.put("terminationReason", "TIMED_OUT");
        ObjectNode process = payload.putObject("process");
        process.putNull("exitCode");
        process.put("signal", "SIGKILL");
        ObjectNode timing = payload.putObject("timing");
        timing.put("submittedAt", "2026-07-20T09:00:00Z");
        timing.put("startedAt", "2026-07-20T09:00:00Z");
        timing.put("finishedAt", "2026-07-20T09:02:00Z");
        timing.put("wallTimeMs", 120_000);
        ObjectNode usage = payload.putObject("usage");
        usage.put("cpuTimeMicros", 48_000);
        usage.put("memoryPeakBytes", 8_290_304);
        ObjectNode output = payload.putObject("output");
        output.put("stdoutTail", "");
        output.put("stderrTail", "");
        output.put("stdoutTruncated", false);
        output.put("stderrTruncated", false);
        return response;
    }

    private static ObjectNode executionFailedResponse(JsonNode request) {
        ObjectNode response = finishedTaskResponse(request);
        ObjectNode payload = (ObjectNode) response.path("payload");
        payload.put("terminationReason", "EXECUTION_FAILED");
        ObjectNode process = (ObjectNode) payload.path("process");
        process.putNull("exitCode");
        process.putNull("signal");
        return response;
    }

    private static ObjectNode taskNotFoundResponse(JsonNode request) {
        return errorResponse(request, "TASK_NOT_FOUND", "task was not found", false);
    }

    private static ObjectNode limitExceedsPolicyResponse(JsonNode request) {
        return errorResponse(
                request, "LIMIT_EXCEEDS_POLICY", "task budget exceeds the deployment maximum", false);
    }

    private static ObjectNode errorResponse(
            JsonNode request, String code, String message, boolean retryable) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "error");
        ObjectNode payload = response.putObject("payload");
        payload.put("code", code);
        payload.put("message", message);
        payload.put("retryable", retryable);
        return response;
    }

    private static ObjectNode taskCancelledResponse(JsonNode request) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "taskCancelled");
        ObjectNode payload = response.putObject("payload");
        payload.put("taskId", "b5309d98-f51e-45e1-9866-b1a080c1ba50");
        payload.put("state", "FINISHED");
        payload.put("terminationReason", "CANCELLED");
        return response;
    }

    private static ObjectNode taskResponse(JsonNode request, String state) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "task");
        ObjectNode payload = response.putObject("payload");
        payload.put("taskId", "b5309d98-f51e-45e1-9866-b1a080c1ba50");
        payload.put("state", state);
        return response;
    }
}
