# TaskCage Local Protocol v1

## 1. Transport

- Unix Domain Socket: `/run/taskcage/taskcaged.sock`
- one connection per synchronous job
- 4-byte unsigned big-endian payload length followed by UTF-8 JSON
- maximum request frame: 64 KiB
- maximum result frame: configured captured-output allowance plus 64 KiB metadata, capped by the daemon

Unknown protocol versions, unknown message types, oversized frames, invalid UTF-8 and invalid budget values are rejected before creating a cgroup or process.

## 2. Run request

```json
{
  "type": "run",
  "protocolVersion": 1,
  "jobId": "01J...",
  "command": ["libreoffice", "--headless", "--convert-to", "pdf", "input.docx"],
  "workingDirectory": "/srv/app/jobs/01J...",
  "environment": {
    "LANG": "C.UTF-8"
  },
  "budget": {
    "memoryBytes": 268435456,
    "cpuQuotaMicros": 50000,
    "cpuPeriodMicros": 100000,
    "maxProcesses": 8,
    "wallTimeNanos": 10000000000,
    "maxOutputBytes": 1048576
  }
}
```

Rules:

- `command` must contain an executable and may not contain NUL values.
- The daemon never invokes a shell to interpret the array.
- Environment variables are explicit additions or overrides to the configured base environment.
- A duplicate active `jobId` is rejected.
- All numeric budgets must be positive and must not exceed daemon policy ceilings.
- The daemon owns final policy validation even if the SDK already validated the request.

## 3. Execution result

```json
{
  "type": "result",
  "protocolVersion": 1,
  "jobId": "01J...",
  "status": "KILLED",
  "reason": "MEMORY_LIMIT_EXCEEDED",
  "exitCode": 137,
  "queueTimeNanos": 1020000,
  "wallTimeNanos": 2341000000,
  "cpuTimeMicros": 1818000,
  "peakMemoryBytes": 268435456,
  "peakProcesses": 4,
  "stdout": {
    "dataBase64": "",
    "truncated": false
  },
  "stderr": {
    "dataBase64": "Li4u",
    "truncated": true
  }
}
```

Initial status values:

- `SUCCEEDED`
- `FAILED`
- `KILLED`
- `CANCELLED`
- `REJECTED`
- `UNSUPPORTED`
- `INTERNAL_ERROR`

Initial reason values:

- `COMPLETED`
- `NON_ZERO_EXIT`
- `WALL_TIMEOUT`
- `MEMORY_LIMIT_EXCEEDED`
- `PID_LIMIT_REACHED`
- `OUTPUT_LIMIT_EXCEEDED`
- `QUEUE_CAPACITY_EXCEEDED`
- `QUEUE_TIMEOUT`
- `CANCELLED_BY_CALLER`
- `PROCESS_SIGNALLED`
- `BACKEND_UNAVAILABLE`
- `UNKNOWN`

Byte output is base64 encoded in v1 so Java and Rust share one deterministic JSON contract. Output is drained concurrently and bounded before encoding.

## 4. Cancellation

In protocol v1 the SDK cancels an active job by closing its socket connection before receiving a terminal result. The daemon records `CANCELLED_BY_CALLER`, invokes `cgroup.kill`, verifies an empty cgroup and cleans up.

Explicit reconnectable cancel messages and result replay are deferred until protocol v2.

## 5. Authentication and authorization

- systemd creates the runtime directory and the daemon owns the socket.
- the socket is not world accessible.
- the daemon validates Linux peer credentials and the configured allowed UID/GID.
- the MVP supports a single local application account model.
- executable allowlists, budget ceilings and working-root restrictions are daemon policy, not caller-provided authority.

## 6. Compatibility rules

- Additive fields may be ignored only when explicitly documented as optional.
- A required field never changes meaning within protocol v1.
- Enum values unknown to a client must map to an SDK `UNKNOWN` representation rather than causing output loss.
- Protocol changes require fixture vectors shared by Rust and Java tests.
