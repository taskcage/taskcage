package org.taskcage.sdk.internal.protocol.local;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.util.UUID;
import org.taskcage.sdk.BooleanProfileInput;
import org.taskcage.sdk.Int64ProfileInput;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileInputValue;
import org.taskcage.sdk.ProfileRequest;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.ResourceBudget;
import org.taskcage.sdk.StringProfileInput;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.TaskSpec;

/** Encodes Local Protocol request payloads and envelopes without invoking a shell. */
public final class LocalRequestEncoder {
    private final ObjectMapper mapper = JsonMapper.builder().build();

    public ObjectNode emptyPayload() {
        return mapper.createObjectNode();
    }

    public ObjectNode taskIdPayload(UUID taskId) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("taskId", taskId.toString());
        return payload;
    }

    public ObjectNode submitTaskPayload(UUID clientRequestId, TaskSpec task) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientRequestId", clientRequestId.toString());
        ObjectNode command = payload.putObject("command");
        command.put("program", task.command().program().toString());
        command.putPOJO("args", task.command().arguments());
        command.put("workingDirectory", task.command().workingDirectory().toString());
        command.set("environment", mapper.valueToTree(task.command().environment()));
        ResourceBudget budget = task.budget();
        ObjectNode limits = payload.putObject("limits");
        ObjectNode cpuMax = limits.putObject("cpuMax");
        cpuMax.put("quotaMicros", budget.cpuMax().quotaMicros());
        cpuMax.put("periodMicros", budget.cpuMax().periodMicros());
        limits.put("memoryMaxBytes", budget.memoryMaxBytes());
        limits.put("pidsMax", budget.pidsMax());
        limits.put("wallTimeLimitMs", budget.wallTimeLimitMillis());
        ObjectNode output = payload.putObject("output");
        output.put("stdoutTailMaxBytes", budget.stdoutTailMaxBytes());
        output.put("stderrTailMaxBytes", budget.stderrTailMaxBytes());
        return payload;
    }

    public ObjectNode profilePayload(UUID clientRequestId, ProfileRequest request) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientRequestId", clientRequestId.toString());
        encodeProfileIdentity(payload.putObject("profile"), request.profile());
        ObjectNode inputs = payload.putObject("inputs");
        request.inputs().forEach((slot, value) -> encodeProfileInput(inputs.putObject(slot), value));
        if (!request.resourceOverrides().isEmpty()) {
            encodeResourceOverrides(payload.putObject("resourceOverrides"), request.resourceOverrides());
        }
        return payload;
    }

    public ObjectNode envelope(int protocolVersion, UUID requestId, String type, ObjectNode payload) {
        ObjectNode request = mapper.createObjectNode();
        request.put("protocolVersion", protocolVersion);
        request.put("requestId", requestId.toString());
        request.put("type", type);
        request.set("payload", payload);
        return request;
    }

    public byte[] write(ObjectNode request) {
        try {
            return mapper.writeValueAsBytes(request);
        } catch (JsonProcessingException exception) {
            throw new TaskCageProtocolException("could not encode TaskCage protocol request", exception);
        }
    }

    private static void encodeProfileIdentity(ObjectNode target, ProfileIdentity profile) {
        target.put("name", profile.name());
        target.put("version", profile.version());
    }

    private static void encodeProfileInput(ObjectNode target, ProfileInputValue value) {
        if (value instanceof StringProfileInput string) {
            target.put("kind", "STRING");
            target.put("value", string.value());
        } else if (value instanceof Int64ProfileInput integer) {
            target.put("kind", "INT64");
            target.put("value", integer.value());
        } else if (value instanceof BooleanProfileInput bool) {
            target.put("kind", "BOOLEAN");
            target.put("value", bool.value());
        } else if (value instanceof LocalInputArtifact artifact) {
            target.put("kind", "LOCAL_INPUT");
            target.put("path", artifact.path().value());
            target.put("digest", artifact.digest().value());
            target.put("sizeBytes", artifact.sizeBytes());
        } else {
            throw new TaskCageProtocolException("unsupported Profile input value type");
        }
    }

    private static void encodeResourceOverrides(ObjectNode target, ProfileResourceOverrides overrides) {
        if (overrides.cpuMax().isPresent()
                || overrides.memoryMaxBytes().isPresent()
                || overrides.pidsMax().isPresent()
                || overrides.wallTimeLimit().isPresent()) {
            ObjectNode limits = target.putObject("limits");
            overrides.cpuMax().ifPresent(cpu -> {
                ObjectNode cpuMax = limits.putObject("cpuMax");
                cpuMax.put("quotaMicros", cpu.quotaMicros());
                cpuMax.put("periodMicros", cpu.periodMicros());
            });
            overrides.memoryMaxBytes().ifPresent(value -> limits.put("memoryMaxBytes", value));
            overrides.pidsMax().ifPresent(value -> limits.put("pidsMax", value));
            overrides.wallTimeLimit().ifPresent(value -> limits.put("wallTimeLimitMs", value.toMillis()));
        }
        if (overrides.stdoutTailMaxBytes().isPresent() || overrides.stderrTailMaxBytes().isPresent()) {
            ObjectNode output = target.putObject("output");
            overrides.stdoutTailMaxBytes().ifPresent(value -> output.put("stdoutTailMaxBytes", value));
            overrides.stderrTailMaxBytes().ifPresent(value -> output.put("stderrTailMaxBytes", value));
        }
    }
}
