package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeoutException;

/**
 * Backend-neutral contract for executing an installed Capsule.
 *
 * <p>An implementation owns its backend resources, while the caller owns the runner lifecycle.
 * Embedded and daemon-backed runners must preserve the same request, timeout, cleanup, and result
 * semantics.
 */
public interface CapsuleRunner extends AutoCloseable {
    /** Executes a Capsule and waits for cleanup-confirmed completion. */
    CapsuleExecutionResult execute(CapsuleRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException;

    /** Executes with a caller-owned idempotency key for response-loss recovery. */
    CapsuleExecutionResult execute(
            UUID clientRequestId, CapsuleRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException;

    @Override
    default void close() {
        // Backends with resources override this method. Stateless runners need no cleanup.
    }

    /** Adapts the current daemon-backed ProfileRuntime to the Capsule contract. */
    static CapsuleRunner external(ProfileRuntime runtime) {
        Objects.requireNonNull(runtime, "runtime");
        return new ExternalCapsuleRunner(runtime);
    }

    /** Current daemon-backed adapter; EmbeddedRunner is introduced in the next phase. */
    final class ExternalCapsuleRunner implements CapsuleRunner {
        private final ProfileRuntime runtime;

        private ExternalCapsuleRunner(ProfileRuntime runtime) {
            this.runtime = runtime;
        }

        @Override
        public CapsuleExecutionResult execute(CapsuleRequest request, Duration waitTimeout)
                throws InterruptedException, TimeoutException {
            Objects.requireNonNull(request, "request");
            return result(request, runtime.run(request.profileRequest(), waitTimeout));
        }

        @Override
        public CapsuleExecutionResult execute(
                UUID clientRequestId, CapsuleRequest request, Duration waitTimeout)
                throws InterruptedException, TimeoutException {
            Objects.requireNonNull(clientRequestId, "clientRequestId");
            Objects.requireNonNull(request, "request");
            return result(
                    request,
                    runtime.run(clientRequestId, request.profileRequest(), waitTimeout));
        }

        private static CapsuleExecutionResult result(
                CapsuleRequest request, FinishedProfileTaskSnapshot snapshot) {
            return new CapsuleExecutionResult(request.capsule(), snapshot);
        }
    }
}
