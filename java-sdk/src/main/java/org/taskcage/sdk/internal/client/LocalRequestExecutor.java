package org.taskcage.sdk.internal.client;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.time.Duration;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.ReentrantLock;
import org.taskcage.sdk.TaskCageConnectionException;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.internal.protocol.local.LocalRequestEncoder;
import org.taskcage.sdk.internal.protocol.local.LocalResponseDecoder;
import org.taskcage.sdk.internal.transport.UnixDomainSocketConnection;

/** Serializes Local Protocol request execution over one managed connection. */
final class LocalRequestExecutor {
    private final LocalConnectionManager connectionManager;
    private final LocalRequestEncoder requestEncoder;
    private final LocalResponseDecoder responseDecoder;
    private final ReentrantLock requestLock = new ReentrantLock();

    LocalRequestExecutor(
            LocalConnectionManager connectionManager,
            LocalRequestEncoder requestEncoder,
            LocalResponseDecoder responseDecoder) {
        this.connectionManager = connectionManager;
        this.requestEncoder = requestEncoder;
        this.responseDecoder = responseDecoder;
    }

    JsonNode request(int protocolVersion, String type, ObjectNode payload, Duration totalTimeout) {
        long startedAt = System.nanoTime();
        long totalTimeoutNanos = totalTimeout == null ? 0 : requirePositiveNanos(totalTimeout, "requestTimeout");
        lockForRequest(totalTimeoutNanos);
        try {
            if (connectionManager.isClosed()) {
                throw new IllegalStateException("TaskCageClient is closed");
            }
            UUID requestId = UUID.randomUUID();
            ObjectNode request = requestEncoder.envelope(protocolVersion, requestId, type, payload);

            UnixDomainSocketConnection activeConnection;
            Duration responseTimeout;
            if (totalTimeout == null) {
                activeConnection = connectionManager.requireConnection();
                responseTimeout = connectionManager.requestTimeout();
            } else {
                activeConnection = connectionManager.requireConnectionWithin(
                        remainingDuration(startedAt, totalTimeoutNanos));
                responseTimeout = shorter(
                        connectionManager.requestTimeout(), remainingDuration(startedAt, totalTimeoutNanos));
            }
            byte[] responseBytes = activeConnection.request(requestEncoder.write(request), responseTimeout);
            JsonNode response = responseDecoder.read(responseBytes);
            responseDecoder.validateEnvelope(response, requestId, protocolVersion);
            if ("error".equals(response.path("type").asText())) {
                throw responseDecoder.decodeDaemonError(response);
            }
            return response;
        } catch (JsonProcessingException exception) {
            connectionManager.closeConnection();
            throw new TaskCageProtocolException("invalid JSON response from TaskCage daemon", exception);
        } catch (IOException exception) {
            connectionManager.closeConnection();
            throw new TaskCageConnectionException("TaskCage daemon connection failed", exception);
        } finally {
            requestLock.unlock();
        }
    }

    void close() {
        requestLock.lock();
        try {
            connectionManager.close();
        } finally {
            requestLock.unlock();
        }
    }

    static long requirePositiveNanos(Duration duration, String name) {
        if (duration == null) {
            throw new NullPointerException(name);
        }
        try {
            long nanos = duration.toNanos();
            if (nanos <= 0) {
                throw new IllegalArgumentException(name + " must be positive and representable in nanoseconds");
            }
            return nanos;
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException(name + " must be representable in nanoseconds", exception);
        }
    }

    static Duration remainingRequestDuration(long startedAt, long timeoutNanos) {
        long remainingNanos = timeoutNanos - (System.nanoTime() - startedAt);
        if (remainingNanos <= 0) {
            throw new TaskCageConnectionException(
                    "timed out waiting to send a TaskCage daemon request",
                    new IOException("request timeout"));
        }
        return Duration.ofNanos(remainingNanos);
    }

    private void lockForRequest(long totalTimeoutNanos) {
        if (totalTimeoutNanos == 0) {
            requestLock.lock();
            return;
        }
        try {
            if (!requestLock.tryLock(totalTimeoutNanos, TimeUnit.NANOSECONDS)) {
                throw new TaskCageConnectionException(
                        "timed out waiting to send a TaskCage daemon request",
                        new IOException("request lock timeout"));
            }
        } catch (InterruptedException exception) {
            Thread.currentThread().interrupt();
            throw new TaskCageConnectionException(
                    "interrupted while waiting to send a TaskCage daemon request", exception);
        }
    }

    private static Duration remainingDuration(long startedAt, long timeoutNanos) throws IOException {
        long remainingNanos = timeoutNanos - (System.nanoTime() - startedAt);
        if (remainingNanos <= 0) {
            throw new IOException("timed out waiting for a TaskCage daemon response");
        }
        return Duration.ofNanos(remainingNanos);
    }

    private static Duration shorter(Duration first, Duration second) {
        return first.compareTo(second) <= 0 ? first : second;
    }
}
