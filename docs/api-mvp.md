# TaskCage MVP API 명세 (v1)

## 목적과 MVP 범위

이 문서는 Java SDK와 Rust 데몬이 독립적으로 개발할 수 있도록 Unix domain socket(UDS) 위의 프로토콜 계약을 정의한다.

> 데몬은 cgroup v2 제한이 적용된 상태에서만 신뢰된 외부 명령을 실행한다. 종료·취소·시간 초과·자원 제한 초과 시 작업 cgroup 전체를 정리하고, SDK에 일관된 종료 원인과 사용량을 반환한다.

MVP는 단일 Linux 호스트에서 Java 21+ SDK와 `taskcaged`가 통신하는 경우만 다룬다. 데몬은 systemd나 DBus를 사용하지 않고 cgroup v2 파일 인터페이스와 Linux 프로세스 API를 직접 제어한다. 원격 실행, gRPC, 로그 스트리밍, 작업 재개, FIFO 대기열, 우선순위, CLI와 다른 언어 SDK는 범위 밖이다.

## 핵심 정책

| 정책 | MVP 결정 |
|---|---|
| 실행 API | `submitTask`, `getTask`, `cancelTask`의 비동기 모델 |
| cgroup 진입 | 원자적 cgroup 진입이 불가능하면 작업을 거절하며 무제한 상태로 실행하지 않음 |
| 자원 예산 | CPU·메모리·PID·벽시계 시간 제한을 모두 요청에 명시 |
| 실행 슬롯 | 전역 슬롯이 모두 사용 중이면 즉시 `CAPACITY_EXHAUSTED` 오류. 대기열 없음 |
| 출력 상한 | stdout/stderr의 마지막 N bytes만 유지하고, 상한을 넘으면 잘림 여부 반환 |
| 결과 보관 | 메모리에 최소 10분 보관. 데몬 재시작 뒤 실행 중 작업의 재개는 지원하지 않음 |

## 전송과 호환성

### Unix domain socket

- 전송 계층은 Unix domain socket의 `SOCK_STREAM`이다.
- SDK는 소켓 경로를 설정할 수 있어야 한다. 기본 경로는 배포 설정의 책임이며 이 명세에서 고정하지 않는다.
- 하나의 연결에서는 요청과 응답을 순서대로 처리한다. 동시 호출은 별도 연결 또는 순서를 보장하는 연결 풀을 사용한다.

### 프레임

각 메시지는 다음 프레임 하나로 전송한다.

```text
+----------------------+---------------------+
| 4 bytes              | N bytes             |
| unsigned big-endian N| UTF-8 JSON payload  |
+----------------------+---------------------+
```

- 길이는 JSON payload의 바이트 수다.
- 최대 프레임 크기는 1,048,576 bytes (1 MiB)다.
- 길이가 0이거나 최대치를 넘으면 수신자는 연결을 종료할 수 있다.
- JSON은 객체여야 하며 중복 키를 사용하지 않는다.

### 버전

- 모든 메시지 최상위에 정수 `protocolVersion`을 넣는다.
- 이 문서는 `protocolVersion: 1`을 정의한다.
- 지원하지 않는 버전은 `UNSUPPORTED_PROTOCOL_VERSION` 오류로 거절하며 작업을 만들지 않는다.
- SDK는 알 수 없는 응답 필드를 무시한다. 데몬은 알 수 없는 요청 필드를 `INVALID_REQUEST`로 거절한다.

## 공통 메시지

### 요청

```json
{
  "protocolVersion": 1,
  "requestId": "c2a091d5-2dd7-44aa-b48f-fd3dd82aa684",
  "type": "submitTask",
  "payload": {}
}
```

- `requestId`는 UUID 문자열이며 요청·응답 상관관계와 진단 로그에 사용한다.
- `type`은 `getCapabilities`, `submitTask`, `getTask`, `cancelTask` 중 하나다.
- `payload`는 요청별 본문이다.

### 성공 응답

```json
{
  "protocolVersion": 1,
  "requestId": "c2a091d5-2dd7-44aa-b48f-fd3dd82aa684",
  "type": "taskAccepted",
  "payload": {}
}
```

응답의 `requestId`는 요청의 값을 그대로 복사한다.

### 오류 응답

프로토콜·검증·데몬 오류는 작업 결과가 아니라 오류 응답으로 반환한다.

```json
{
  "protocolVersion": 1,
  "requestId": "c2a091d5-2dd7-44aa-b48f-fd3dd82aa684",
  "type": "error",
  "payload": {
    "code": "INVALID_REQUEST",
    "message": "limits.memoryMaxBytes must be greater than zero",
    "retryable": false
  }
}
```

SDK의 분기 기준은 사람이 읽는 `message`가 아니라 `code`다.

## 작업 상태와 종료 원인

### 상태 전이

```text
RUNNING -> FINISHED
```

- `RUNNING`: 데몬이 작업 cgroup을 만들고 모든 제한을 설정한 뒤, 외부 명령을 그 cgroup 안에서 실행 중이다.
- `FINISHED`: 종료 결과를 확정했고 더 이상 상태가 바뀌지 않는다.

데몬은 cgroup v2 controller, 쓰기 권한, `cgroup.kill`, 원자적 cgroup 진입 같은 필수 조건을 충족하지 못하면 작업을 수락하지 않는다.

### 종료 원인

`FINISHED` 상태는 정확히 하나의 `terminationReason`을 갖는다.

| 값 | 의미 |
|---|---|
| `EXITED` | 명령이 시작되어 종료되었다. exit code가 0이 아닐 수 있다. |
| `EXECUTION_FAILED` | 실행 파일을 시작하지 못했거나 실행 중 복구 불가능한 오류가 발생했다. |
| `CANCELLED` | SDK가 취소를 요청했고 데몬이 작업 cgroup 정리를 완료했다. |
| `TIMED_OUT` | 벽시계 실행 시간이 `wallTimeLimitMs`를 넘었다. |
| `MEMORY_LIMIT_EXCEEDED` | cgroup 메모리 이벤트에 근거해 메모리 상한 초과를 판정했다. |
| `PROCESS_LIMIT_EXCEEDED` | cgroup PID 이벤트에 근거해 프로세스 수 상한 초과를 판정했다. |
| `DAEMON_ERROR` | 작업 생성 뒤 데몬 내부 오류로 안전한 결과를 확정하지 못했다. |

데몬은 단일 exit code만으로 종료 원인을 추측하지 않는다. 메모리·PID 한도 초과는 cgroup 이벤트를, timeout과 cancel은 데몬 제어 상태를 함께 확인해 판정한다.

## API

### `getCapabilities`

SDK는 연결 직후 또는 설정 검증에 이 요청을 사용한다. 요청 payload는 빈 객체다.

```json
{ "protocolVersion": 1, "requestId": "...", "type": "getCapabilities", "payload": {} }
```

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "capabilities",
  "payload": {
    "daemonVersion": "0.1.0",
    "protocolVersions": [1],
    "maxFrameBytes": 1048576,
    "maxConcurrentTasks": 4,
    "cgroupV2Ready": true
  }
}
```

`cgroupV2Ready`가 `false`이면 `submitTask`는 `ENVIRONMENT_UNAVAILABLE` 오류를 반환한다.

### `submitTask`

SDK는 응답 유실이나 재연결 뒤 같은 작업을 재전송할 수 있으므로 호출 단위의 `clientRequestId`를 생성하고 재사용한다.

```json
{
  "protocolVersion": 1,
  "requestId": "a1f6d5f2-2bf7-4a8d-b6d9-2e4d0a8860e2",
  "type": "submitTask",
  "payload": {
    "clientRequestId": "3a4edb15-fab7-4746-9209-909e406ae829",
    "command": {
      "program": "/usr/bin/pdftotext",
      "args": ["input.pdf", "output.txt"],
      "workingDirectory": "/srv/taskcage/jobs/42",
      "environment": { "LANG": "C.UTF-8" }
    },
    "limits": {
      "cpuMax": { "quotaMicros": 100000, "periodMicros": 100000 },
      "memoryMaxBytes": 536870912,
      "pidsMax": 32,
      "wallTimeLimitMs": 120000
    },
    "output": {
      "stdoutTailMaxBytes": 65536,
      "stderrTailMaxBytes": 65536
    }
  }
}
```

필드 규칙:

- `clientRequestId`는 UUID다. 같은 값의 요청 본문이 다르면 `IDEMPOTENCY_CONFLICT` 오류를 반환한다.
- `program`과 `workingDirectory`는 절대 경로다. `program`은 shell 문자열이 아니며, 데몬은 shell을 거쳐 실행하지 않는다.
- `args`의 각 원소와 environment의 key/value는 UTF-8 문자열이다.
- `cpuMax.quotaMicros`, `cpuMax.periodMicros`, `memoryMaxBytes`, `pidsMax`, `wallTimeLimitMs`는 모두 0보다 큰 정수다.
- `stdoutTailMaxBytes`와 `stderrTailMaxBytes`는 각각 1 이상 65,536 이하의 정수이며, 두 값의 합계는 131,072 이하이어야 한다. 이 상한은 UTF-8 치환 뒤의 응답도 1 MiB 프레임 한도 안에 남도록 보장한다.
- 모든 자원 제한은 필수다. 데몬은 임의의 기본 자원 예산으로 작업을 실행하지 않는다.
- `cpuMax`는 cgroup v2 `cpu.max`의 quota와 period를 그대로 표현한다.
- 환경 변수는 데몬 환경을 상속하지 않는다. 명세에 전달한 값과 데몬이 안전하게 추가한 최소 환경만 사용한다.

```json
{
  "protocolVersion": 1,
  "requestId": "a1f6d5f2-2bf7-4a8d-b6d9-2e4d0a8860e2",
  "type": "taskAccepted",
  "payload": {
    "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50",
    "state": "RUNNING",
    "effectiveLimits": {
      "cpuMax": { "quotaMicros": 100000, "periodMicros": 100000 },
      "memoryMaxBytes": 536870912,
      "pidsMax": 32,
      "wallTimeLimitMs": 120000
    }
  }
}
```

`taskAccepted`는 데몬이 작업 cgroup을 만들고 모든 제한을 설정한 뒤, 외부 명령을 해당 cgroup 안에서 시작한 후에만 반환한다. `effectiveLimits`는 실제 cgroup에 적용된 값이며 요청값과 다르면 데몬은 작업을 수락하지 않고 `LIMIT_EXCEEDS_POLICY` 오류를 반환한다. 슬롯이 없으면 데몬은 작업을 만들지 않고 `CAPACITY_EXHAUSTED` 오류를 반환한다. 같은 `clientRequestId` 재전송에는 새 작업 대신 기존 `taskId`와 현재 상태를 반환한다.

### `getTask`

작업 상태와, 완료된 경우 최종 결과를 하나의 스냅샷으로 조회한다. SDK는 완료 전과 후에 같은 API를 사용한다.

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "getTask",
  "payload": { "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50" }
}
```

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "task",
  "payload": {
    "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50",
    "state": "RUNNING",
    "submittedAt": "2026-07-20T09:00:00Z",
    "startedAt": "2026-07-20T09:00:00Z"
  }
}
```

`FINISHED` 상태에는 `finishedAt`, `terminationReason`, `process`, `timing`, `usage`, `output`을 포함한다. `RUNNING` 상태에는 이 최종 결과 필드를 포함하지 않는다.

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "task",
  "payload": {
    "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50",
    "state": "FINISHED",
    "terminationReason": "TIMED_OUT",
    "process": { "exitCode": null, "signal": "SIGKILL" },
    "timing": {
      "submittedAt": "2026-07-20T09:00:00Z",
      "startedAt": "2026-07-20T09:00:00Z",
      "finishedAt": "2026-07-20T09:02:00Z",
      "wallTimeMs": 120000
    },
    "usage": { "cpuTimeMicros": 48000, "memoryPeakBytes": 8290304 },
    "output": {
      "stdoutTail": "",
      "stderrTail": "",
      "stdoutTruncated": false,
      "stderrTruncated": false
    }
  }
}
```

- `wallTimeMs`는 실행 시작부터 종료 확정까지의 단조 시간 차이다.
- `cpuTimeMicros`와 `memoryPeakBytes`는 cgroup 통계에서 수집한다.
- `exitCode`와 `signal`은 해당하지 않으면 `null`이다.
- 출력은 UTF-8 문자열로 반환하며, 유효하지 않은 UTF-8 바이트열은 U+FFFD로 치환한다.

### `cancelTask`

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "cancelTask",
  "payload": { "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50" }
}
```

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "taskCancelled",
  "payload": {
    "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50",
    "state": "FINISHED",
    "terminationReason": "CANCELLED"
  }
}
```

`taskCancelled`은 취소 요청 접수가 아니라, 작업 cgroup 전체 정리와 `cgroup.events`의 `populated 0` 확인이 끝난 뒤에만 반환한다. 이미 완료된 작업 취소는 `TASK_ALREADY_FINISHED` 오류를 반환한다.

## 실행·출력·보관 정책

- 데몬은 전역 최대 실행 수만 사용한다. 모든 슬롯이 사용 중이면 `submitTask`는 `CAPACITY_EXHAUSTED` 오류를 반환하며 작업을 만들지 않는다.
- stdout 또는 stderr가 각자의 상한을 초과하면 데몬은 해당 스트림의 오래된 바이트를 버리고 마지막 N raw bytes만 유지한다. 작업은 계속 실행하며, `stdoutTail` 또는 `stderrTail`과 `*Truncated: true`를 결과에 반환한다. 유효하지 않은 UTF-8 바이트는 응답 직전 U+FFFD로 치환한다.
- 결과와 `clientRequestId` 매핑은 완료 시점부터 최소 10분 보관한다. 기간 뒤 조회와 취소는 `TASK_NOT_FOUND`를 반환할 수 있다.
- 데몬 재시작으로 연결이 끊기면 SDK는 같은 `clientRequestId`로 제출을 재시도할 수 있다. 진행 중 작업의 재개는 지원하지 않으며, 데몬은 시작 시 남은 TaskCage cgroup을 안전하게 정리한다.

## 오류 코드

| 코드 | 의미 | 재시도 |
|---|---|---|
| `INVALID_REQUEST` | 필수 필드, 타입, 경로 또는 상한 검증 실패 | 아니오 |
| `UNSUPPORTED_PROTOCOL_VERSION` | 지원하지 않는 프로토콜 버전 | 아니오 |
| `FRAME_TOO_LARGE` | 프레임 크기가 최대치를 넘음 | 아니오 |
| `ENVIRONMENT_UNAVAILABLE` | cgroup v2 또는 필수 controller·권한을 사용할 수 없음 | 아니오 |
| `CAPACITY_EXHAUSTED` | 전역 실행 슬롯이 모두 사용 중임 | 예 |
| `TASK_NOT_FOUND` | 작업이 없거나 보관 기간이 지남 | 아니오 |
| `TASK_ALREADY_FINISHED` | 완료된 작업 취소 요청 | 아니오 |
| `IDEMPOTENCY_CONFLICT` | 같은 clientRequestId에 다른 요청 본문을 사용함 | 아니오 |
| `LIMIT_EXCEEDS_POLICY` | 요청 제한이 데몬의 배포 정책을 벗어남 | 아니오 |
| `DAEMON_UNAVAILABLE` | SDK가 UDS 연결 또는 응답을 얻지 못함 | 예 |
| `INTERNAL_ERROR` | 작업을 만들기 전 데몬의 예상하지 못한 오류 | 상황에 따라 |

`DAEMON_UNAVAILABLE`은 소켓 연결·읽기·쓰기 실패를 Java SDK가 표현하는 로컬 오류이며, 데몬이 전송하는 JSON 오류가 아니다.

## 병렬 개발과 fixture

Java SDK와 Rust 데몬은 아래 fixture를 공유 계약으로 사용한다. 실제 파일은 `protocol-fixtures/v1/`에 둔다.

```text
submit-task-valid.json
task-accepted.json
task-running.json
task-result-timeout.json
task-result-output-truncated.json
error-capacity-exhausted.json
```

- Java SDK는 fixture의 요청·응답 역직렬화, 오류 매핑, 프레임 처리를 단위 테스트한다.
- Rust 데몬은 같은 fixture의 역직렬화·직렬화와 상태 전이를 단위 테스트한다.
- Linux 통합 테스트는 timeout, memory limit, process limit, 자식 프로세스 정리의 실제 결과를 이 명세의 `terminationReason`으로 검증한다.
- 요청·응답 필드, enum, 오류 코드 변경은 양쪽 구현과 fixture를 같은 변경에 포함한다.

현재 fixture는 프로토콜 형식을 보여 주는 최소 예시다. 메모리·PID 제한, 중복 요청, 환경 오류 fixture는 해당 기능 구현 PR에서 추가한다.

## MVP 이후 후보

- FIFO 대기열, 대기 시간 제한, 우선순위와 공정성
- 실시간 stdout/stderr 및 상태 스트리밍
- CPU weight, I/O·디스크 제한, 결과 영속화, 재시작 뒤 작업 복구
- gRPC/Protobuf, 원격 데몬, Python SDK, CLI, Docker·Kubernetes 지원
