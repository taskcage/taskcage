# TaskCage Protocol v1 API 명세

## 목적과 범위

이 문서는 Java SDK와 Rust 데몬 사이의 공개 wire 계약을 정의한다. Protocol v1은 같은 Linux 호스트에서 Unix domain socket(UDS)을 통해 신뢰된 외부 명령을 제한된 Task로 실행한다.

현재 범위:

- `getCapabilities`, `submitTask`, `getTask`, `cancelTask`
- CPU·메모리·PID·벽시계 시간과 출력 tail 상한
- 전역 동시 실행 제한과 메모리 Registry 상한
- 작업 cgroup 전체 정리와 최종 결과
- 데몬 생존 기간 내 멱등 제출

원격 전송, 작업 대기열, 스트리밍, 영속 Registry, 재시작 뒤 작업 재개, Profile·Bundle·Hub는 Protocol v1 범위가 아니다.

## 실행 불변 조건

| 조건 | 계약 |
|---|---|
| 제한 우선 | cgroup과 모든 제한을 적용하고 read-back한 뒤에만 외부 명령을 시작한다. |
| 원자적 진입 | 외부 프로세스가 생성 시점부터 작업 cgroup에 들어가는 조건을 만족하지 못하면 실행하지 않는다. |
| 전체 정리 | timeout·취소·오류 시 개별 PID가 아니라 작업 cgroup 전체를 종료한다. |
| 완료 의미 | 프로세스, cgroup과 출력 reader 정리를 확인한 뒤에만 `FINISHED`를 공개한다. |
| 실패 차단 | 안전한 실행이나 정리를 확인할 수 없으면 신규 작업을 받지 않는다. |

## 전송

### Unix domain socket

- `SOCK_STREAM`을 사용한다.
- socket 절대 경로는 배포 설정과 SDK 설정으로 명시하며 프로토콜 기본값은 없다.
- socket mode는 owner-only `0600`이다.
- 한 연결의 요청과 응답은 순서대로 처리한다.
- 데몬의 UDS 연결 상한은 실행 작업 상한과 별도인 서비스 설정이다. 초과 연결은 Protocol 오류 없이 닫힐 수 있으며 SDK는 이를 연결 오류로 처리한다.

데몬은 systemd나 DBus에 의존하지 않는다. 다만 배포 환경은 데몬이 사용할 cgroup subtree와 socket 부모 디렉터리를 안전하게 준비해야 한다.

### 프레임

```text
+-----------------------+--------------------+
| 4-byte unsigned N     | N-byte UTF-8 JSON  |
| big-endian            | object             |
+-----------------------+--------------------+
```

- 최대 JSON payload는 1,048,576 bytes(1 MiB)다.
- 길이가 0이거나 상한을 넘으면 수신자는 연결을 종료할 수 있다.
- JSON 최상위 값은 객체이며 중복 키를 허용하지 않는다.

### 호환성

- 모든 메시지는 정수 `protocolVersion`을 가진다. 이 문서는 버전 `1`을 정의한다.
- 지원하지 않는 요청 버전은 `UNSUPPORTED_PROTOCOL_VERSION`으로 거절한다.
- 데몬은 알 수 없는 요청 필드를 `INVALID_REQUEST`로 거절한다.
- SDK는 알 수 없는 응답 필드를 무시한다.

## 공통 메시지

요청:

```json
{
  "protocolVersion": 1,
  "requestId": "c2a091d5-2dd7-44aa-b48f-fd3dd82aa684",
  "type": "getCapabilities",
  "payload": {}
}
```

- `requestId`는 요청·응답 상관관계에 사용하는 UUID다.
- 요청 `type`은 `getCapabilities`, `submitTask`, `getTask`, `cancelTask` 중 하나다.

성공 응답은 요청의 `requestId`를 그대로 반환한다.

오류 응답:

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

호출자는 사람이 읽는 `message`가 아니라 `code`와 `retryable`을 기준으로 분기한다.

## Task 상태와 종료 원인

상태는 다음 한 방향으로만 전이한다.

```text
RUNNING -> FINISHED
```

- `RUNNING`: 제한이 적용된 작업 cgroup 안에서 외부 명령이 시작되었다.
- `FINISHED`: 결과가 확정되었고 작업 cgroup과 출력 reader 정리가 완료되었다.

`FINISHED`는 정확히 하나의 `terminationReason`을 가진다.

| 값 | 의미 |
|---|---|
| `EXITED` | 시작된 명령이 종료되었다. exit code는 0이 아닐 수 있다. |
| `EXECUTION_FAILED` | `execve`를 시작하지 못했거나 실행 경로에서 복구 불가능한 오류가 발생했다. |
| `CANCELLED` | 취소가 먼저 관찰되었고 전체 정리를 완료했다. |
| `TIMED_OUT` | 벽시계 제한이 먼저 관찰되었고 전체 정리를 완료했다. |
| `MEMORY_LIMIT_EXCEEDED` | cgroup 메모리 이벤트가 한도 초과를 나타낸다. |
| `PROCESS_LIMIT_EXCEEDED` | cgroup PID 이벤트가 한도 초과를 나타낸다. |
| `DAEMON_ERROR` | 데몬 내부 오류 뒤에도 안전한 정리와 결과 근거를 확인했다. |

timeout과 취소가 경합하면 먼저 관찰한 원인을 유지한다. 메모리·PID 제한은 exit code가 아니라 cgroup 이벤트를 근거로 판정한다.

## API

### `getCapabilities`

요청 payload는 빈 객체다.

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

`cgroupV2Ready`가 `false`이면 새 `submitTask`는 `ENVIRONMENT_UNAVAILABLE`로 거절된다.

### `submitTask`

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

- `clientRequestId`는 UUID이며 멱등 제출 키다.
- `program`과 `workingDirectory`는 절대 경로다.
- `args`는 shell 해석 없이 전달되는 UTF-8 문자열 배열이다.
- `environment`는 명시적으로 전달할 UTF-8 key/value다. 데몬 환경 전체를 자동 상속하지 않는다.
- CPU quota/period, 메모리, PID, 벽시계 제한은 모두 필수인 양의 정수다.
- stdout/stderr tail 상한은 각각 1~65,536 bytes이며 합계는 131,072 bytes 이하다.

수락 응답:

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

`taskAccepted`는 제한을 적용·확인하고 명령이 해당 cgroup 안에서 시작된 뒤에만 반환한다. `execve`가 시작되지 못하면 정리를 완료한 `task`/`FINISHED` 응답을 직접 반환하며 `terminationReason`은 `EXECUTION_FAILED`, exit code와 signal은 `null`이다.

`effectiveLimits`가 요청과 다르면 target을 시작하거나 공개 `taskId`를 만들지 않고 `INTERNAL_ERROR`, `retryable: false`를 반환한다.

멱등 규칙:

- 같은 `clientRequestId`와 같은 payload는 새 실행 없이 기존 task의 현재 응답을 반환한다.
- 같은 ID와 다른 payload는 `IDEMPOTENCY_CONFLICT`다.
- 이 보장은 메모리 Registry가 유지되는 같은 데몬 프로세스 안에서만 유효하다.

실행 슬롯 또는 Registry 여유가 없으면 side effect 없이 `CAPACITY_EXHAUSTED`를 반환한다. Protocol v1에는 대기열이 없다.

### `getTask`

```json
{
  "protocolVersion": 1,
  "requestId": "...",
  "type": "getTask",
  "payload": { "taskId": "b5309d98-f51e-45e1-9866-b1a080c1ba50" }
}
```

실행 중 응답:

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

완료 응답:

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
    "usage": {
      "cpuTimeMicros": 48000,
      "memoryPeakBytes": 8290304
    },
    "output": {
      "stdoutTail": "",
      "stderrTail": "",
      "stdoutTruncated": false,
      "stderrTruncated": false
    }
  }
}
```

- `wallTimeMs`는 외부 명령의 실행 gate를 연 시점부터 정리 확정까지의 시간이다. 준비 시간은 제외하고 cleanup 시간은 포함하므로 제한보다 클 수 있다.
- 사용량은 작업 cgroup 통계에서 수집한다.
- signal은 `SIGKILL` 같은 Linux 표준 이름이다.
- 출력은 각 stream의 마지막 N raw bytes다. 유효하지 않은 UTF-8은 응답에서 U+FFFD로 치환한다.

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

`taskCancelled`은 취소 접수가 아니라 작업 cgroup 전체 정리, `cgroup.events`의 `populated 0`,
`FINISHED` 저장과 정상 재사용 가능한 실행 슬롯 반환이 끝났다는 확인이다. process-wide fail-stop이
슬롯을 보존한 경우에는 새 작업을 받지 않는다. 이미 완료된 작업은 `TASK_ALREADY_FINISHED`를 반환한다.

## 보관과 장애 계약

- 완료 snapshot과 `clientRequestId` mapping은 완료 시점부터 최소 10분 동안 메모리에 보관한다.
- 보관 기간이 지나면 조회·취소는 `TASK_NOT_FOUND`가 될 수 있다.
- Registry 상한은 예약·실행 중·보관 중인 완료 작업을 모두 포함한다.
- 데몬 재시작 전 snapshot과 멱등 mapping은 복구하지 않는다. 이전 `taskId`는 `TASK_NOT_FOUND`가 될 수 있다.
- 시작 시 검증된 stale socket과 TaskCage 잔여 cgroup을 정리하고 환경 검사를 통과한 뒤에만 요청을 받는다.
- `RUNNING` 이후 정리를 확인할 수 없으면 readiness를 false로 바꾸고 신규 연결·작업을 차단하며 모든 활성 작업의 정리를 시도한 뒤 데몬을 비정상 종료한다.
- 정리를 확인하지 못한 작업을 근거 없는 `FINISHED`로 바꾸지 않는다.

따라서 Protocol v1은 데몬 재시작을 가로지르는 exactly-once 실행이나 작업 재개를 보장하지 않는다.

## 오류 코드

| 코드 | 의미 | 재시도 |
|---|---|---|
| `INVALID_REQUEST` | 필수 필드, 타입, 경로 또는 값 검증 실패 | 아니오 |
| `UNSUPPORTED_PROTOCOL_VERSION` | 지원하지 않는 프로토콜 버전 | 아니오 |
| `FRAME_TOO_LARGE` | 프레임 상한 초과 | 아니오 |
| `ENVIRONMENT_UNAVAILABLE` | 안전한 cgroup 실행 또는 정리 조건을 사용할 수 없음 | 아니오 |
| `CAPACITY_EXHAUSTED` | 실행 슬롯 또는 Registry 수용량 부족 | 예 |
| `TASK_NOT_FOUND` | 작업이 없거나 보관 기간이 지남 | 아니오 |
| `TASK_ALREADY_FINISHED` | 완료된 작업 취소 요청 | 아니오 |
| `IDEMPOTENCY_CONFLICT` | 같은 멱등 키에 다른 요청을 사용함 | 아니오 |
| `LIMIT_EXCEEDS_POLICY` | 요청 제한이 명시적인 배포 정책을 벗어남 | 아니오 |
| `INTERNAL_ERROR` | 예상하지 못한 데몬 오류 또는 제한 read-back 불일치 | 응답 값에 따름 |

`DAEMON_UNAVAILABLE`은 wire 오류 코드가 아니라 UDS 연결·읽기·쓰기 실패를 SDK가 표현하는 로컬 오류다.

## 호환성 fixture

공유 JSON은 [`protocol-fixtures/v1/`](../protocol-fixtures/v1/README.md)에 둔다. 필드, enum, 오류 코드 또는 상태 의미를 바꾸면 Rust 구현, Java SDK와 fixture를 같은 변경에서 갱신한다.
