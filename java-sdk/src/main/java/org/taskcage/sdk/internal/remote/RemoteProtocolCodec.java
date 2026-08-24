package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.UUID;
import org.taskcage.sdk.ManagedOutputArtifact;
import org.taskcage.sdk.RemoteArtifactChunk;
import org.taskcage.sdk.RemoteArtifactChunkProgress;
import org.taskcage.sdk.RemoteArtifactUpload;
import org.taskcage.sdk.RemoteArtifactUploadStart;
import org.taskcage.sdk.RemoteCapabilities;
import org.taskcage.sdk.RemoteProfileRequest;
import org.taskcage.sdk.RemoteProfileTask;
import org.taskcage.sdk.RemoteProfileTaskSnapshot;
import org.taskcage.sdk.ServiceCredentials;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageDaemonException;
import org.taskcage.sdk.TaskCancellation;

/** Compatibility facade over the Remote Protocol v1 encoder, validator, and decoder. */
public final class RemoteProtocolCodec {
    public static final int VERSION = 1;

    private final RemoteRequestEncoder requestEncoder = new RemoteRequestEncoder();
    private final EnvelopeValidator envelopeValidator = new EnvelopeValidator();
    private final RemoteResponseDecoder responseDecoder = new RemoteResponseDecoder();

    public byte[] authenticate(UUID requestId, ServiceCredentials credentials) {
        return requestEncoder.authenticate(requestId, credentials);
    }

    public byte[] getCapabilities(UUID requestId) {
        return requestEncoder.getCapabilities(requestId);
    }

    public byte[] beginArtifactUpload(
            UUID requestId,
            UUID clientArtifactId,
            Sha256Digest digest,
            long sizeBytes,
            String mediaType) {
        return requestEncoder.beginArtifactUpload(
                requestId, clientArtifactId, digest, sizeBytes, mediaType);
    }

    public byte[] uploadArtifactChunk(UUID requestId, UUID artifactId, long offset, byte[] bytes) {
        return requestEncoder.uploadArtifactChunk(requestId, artifactId, offset, bytes);
    }

    public byte[] completeArtifactUpload(UUID requestId, UUID artifactId) {
        return requestEncoder.completeArtifactUpload(requestId, artifactId);
    }

    /** Encodes the owner-only request to discard an unreferenced managed input Artifact. */
    public byte[] abortArtifactUpload(UUID requestId, UUID artifactId) {
        return requestEncoder.abortArtifactUpload(requestId, artifactId);
    }

    public byte[] readArtifactChunk(UUID requestId, UUID artifactId, long offset, int maxBytes) {
        return requestEncoder.readArtifactChunk(requestId, artifactId, offset, maxBytes);
    }

    /**
     * Encodes a Profile submission with a caller-owned idempotency key.
     *
     * <p>After a lost response, callers must reuse {@code clientRequestId} with the same request. The daemon
     * resolves that key before input Artifact lookup, so this remains the only valid recovery path after a managed
     * input Artifact has transferred to task ownership.
     */
    public byte[] submitProfile(UUID requestId, UUID clientRequestId, RemoteProfileRequest request) {
        return requestEncoder.submitProfile(requestId, clientRequestId, request);
    }

    public byte[] getProfileResult(UUID requestId, UUID taskId) {
        return requestEncoder.getProfileResult(requestId, taskId);
    }

    public byte[] cancelTask(UUID requestId, UUID taskId) {
        return requestEncoder.cancelTask(requestId, taskId);
    }

    public JsonNode readAndValidate(byte[] bytes, UUID requestId) {
        return envelopeValidator.readAndValidate(bytes, requestId);
    }

    public void requireAuthenticated(JsonNode response) {
        responseDecoder.requireAuthenticated(response);
    }

    public TaskCageDaemonException decodeError(JsonNode response) {
        return responseDecoder.decodeError(response);
    }

    public RemoteCapabilities decodeCapabilities(JsonNode response) {
        return responseDecoder.decodeCapabilities(response);
    }

    public RemoteArtifactUpload decodeArtifactUploaded(JsonNode response) {
        return responseDecoder.decodeArtifactUploaded(response);
    }

    /** Decodes a begin-upload response that is also used to recover after a lost acknowledgement. */
    public RemoteArtifactUploadStart decodeArtifactUploadStarted(JsonNode response) {
        return responseDecoder.decodeArtifactUploadStarted(response);
    }

    /** Decodes a chunk acknowledgement, including an idempotent acknowledgement after retry. */
    public RemoteArtifactChunkProgress decodeArtifactChunkAccepted(JsonNode response) {
        return responseDecoder.decodeArtifactChunkAccepted(response);
    }

    public ManagedOutputArtifact decodeManagedOutputArtifact(JsonNode artifact) {
        return responseDecoder.decodeManagedOutputArtifact(artifact);
    }

    public RemoteProfileTask decodeProfileAccepted(JsonNode response) {
        return responseDecoder.decodeProfileAccepted(response);
    }

    public RemoteProfileTaskSnapshot decodeProfileResult(JsonNode response) {
        return responseDecoder.decodeProfileResult(response);
    }

    public TaskCancellation decodeTaskCancelled(JsonNode response) {
        return responseDecoder.decodeTaskCancelled(response);
    }

    public RemoteArtifactChunk decodeArtifactChunk(JsonNode response) {
        return responseDecoder.decodeArtifactChunk(response);
    }
}
