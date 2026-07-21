package io.github.taskcage.sdk.support;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.github.taskcage.sdk.internal.transport.LengthPrefixedFrameCodec;
import java.io.IOException;
import java.net.StandardProtocolFamily;
import java.net.UnixDomainSocketAddress;
import java.nio.channels.ServerSocketChannel;
import java.nio.channels.SocketChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.Function;

/** A protocol-level UDS peer used by SDK tests; it is not part of the published artifact. */
public final class FakeTaskCageServer implements AutoCloseable {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private final Path directory;
    private final Path socketPath;
    private final ServerSocketChannel server;
    private final List<Function<JsonNode, JsonNode>> handlers;
    private final List<JsonNode> requests = new ArrayList<>();
    private final CountDownLatch handled;
    private final AtomicReference<Throwable> failure = new AtomicReference<>();
    private final Thread thread;

    private FakeTaskCageServer(List<Function<JsonNode, JsonNode>> handlers) throws IOException {
        this.directory = Files.createTempDirectory("taskcage-sdk-test-");
        this.socketPath = directory.resolve("taskcaged.sock");
        this.server = ServerSocketChannel.open(StandardProtocolFamily.UNIX);
        this.server.bind(UnixDomainSocketAddress.of(socketPath));
        this.handlers = List.copyOf(handlers);
        this.handled = new CountDownLatch(handlers.size());
        this.thread = Thread.ofPlatform().name("fake-taskcage-server").start(this::serve);
    }

    public static FakeTaskCageServer start(Function<JsonNode, JsonNode> handler) throws IOException {
        return new FakeTaskCageServer(List.of(handler));
    }

    public static FakeTaskCageServer start(List<Function<JsonNode, JsonNode>> handlers) throws IOException {
        return new FakeTaskCageServer(handlers);
    }

    public Path socketPath() {
        return socketPath;
    }

    public List<JsonNode> requests() {
        synchronized (requests) {
            return List.copyOf(requests);
        }
    }

    public void awaitRequests(Duration timeout) throws InterruptedException {
        if (!handled.await(timeout.toMillis(), TimeUnit.MILLISECONDS)) {
            throw new AssertionError("fake daemon did not receive every expected request");
        }
        Throwable serverFailure = failure.get();
        if (serverFailure != null) {
            throw new AssertionError("fake daemon failed", serverFailure);
        }
    }

    @Override
    public void close() throws IOException {
        server.close();
        try {
            thread.join(Duration.ofSeconds(2));
        } catch (InterruptedException exception) {
            Thread.currentThread().interrupt();
            throw new IOException("interrupted while closing fake daemon", exception);
        } finally {
            Files.deleteIfExists(socketPath);
            Files.deleteIfExists(directory);
        }
    }

    private void serve() {
        try {
            for (Function<JsonNode, JsonNode> handler : handlers) {
                try (SocketChannel client = server.accept()) {
                    JsonNode request = MAPPER.readTree(
                            LengthPrefixedFrameCodec.read(client, Duration.ofSeconds(2)));
                    synchronized (requests) {
                        requests.add(request);
                    }
                    JsonNode response = handler.apply(request);
                    if (response != null) {
                        LengthPrefixedFrameCodec.write(
                                client,
                                MAPPER.writeValueAsBytes(response),
                                Duration.ofSeconds(2));
                    }
                }
                handled.countDown();
            }
        } catch (Throwable exception) {
            if (server.isOpen()) {
                failure.compareAndSet(null, exception);
            }
        }
    }
}
