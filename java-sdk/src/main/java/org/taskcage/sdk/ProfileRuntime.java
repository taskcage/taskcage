package org.taskcage.sdk;

import java.time.Duration;
import java.util.UUID;
import java.util.concurrent.TimeoutException;

/**
 * Minimal caller-owned runtime contract for synchronous Local Execution Profile calls.
 *
 * <p>Bindings use this interface without taking ownership of a client connection or other runtime
 * resources. Implementations retain their existing lifecycle responsibilities.
 */
public interface ProfileRuntime {
    /**
     * Runs a Local Profile Task and waits for cleanup-confirmed completion.
     *
     * @param request installed Profile request
     * @param waitTimeout positive completion wait timeout
     * @return cleanup-confirmed terminal Profile snapshot
     * @throws InterruptedException if the waiting thread is interrupted
     * @throws TimeoutException if the wait deadline expires without cancelling the Task
     */
    FinishedProfileTaskSnapshot run(ProfileRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException;

    /**
     * Runs a Local Profile Task with a caller-owned idempotency key.
     *
     * @param clientRequestId caller-owned Core idempotency key
     * @param request installed Profile request
     * @param waitTimeout positive completion wait timeout
     * @return cleanup-confirmed terminal Profile snapshot
     * @throws InterruptedException if the waiting thread is interrupted
     * @throws TimeoutException if the wait deadline expires without cancelling the Task
     */
    FinishedProfileTaskSnapshot run(
            UUID clientRequestId, ProfileRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException;
}
