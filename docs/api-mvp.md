# TaskCage MVP API 명세 (v1)

## 목적과 MVP 범위

이 문서는 Java SDK와 Rust 데몬이 독립적으로 개발할 수 있도록 Unix domain socket(UDS) 위의 프로토콜 계약을 정의한다.

> 데몬은 cgroup v2 제한이 적용된 상태에서만 신뢰된 외부 명령을 실행한다. 종료·취소·시간 초과·자원 제한 초과 시 작업 cgroup 전체를 정리하고, SDK에 일관된 종료 원인과 사용량을 반환한다.

MVP는 단일 Linux 호스트에서 Java 17+ SDK와 `taskcaged`가 통신하는 경우만 다룬다. 데몬은 systemd나 DBus를 사용하지 않고 cgroup v2 파일 인터페이스와 Linux 프로세스 API를 직접 제어한다. 원격 실행, gRPC, 로그 스트리밍, 작업 재개, FIFO 대기열, 우선순위, CLI와 다른 언어 SDK는 범위 밖이다.

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
- daemon은 0보다 큰 UDS 동시 연결 상한을 명시적인 서비스 설정으로 받으며 기본값을 가정하지 않는다.
  이 상한은 실행 작업 수인 `maxConcurrentTasks`와 별개이며 Protocol v1 응답 field가 아니다.
- 연결 상한에는 실행 중인 handler와 완료됐지만 아직 회수되지 않은 handler가 모두 포함된다. 내부 연결
  대기열은 만들지 않는다.
- 상한을 넘은 연결은 요청 prefix, JSON 또는 본문을 읽거나 dispatcher를 호출하지 않고 즉시 닫는다.
  Protocol v1 오류 응답은 만들지 않으므로 SDK는 이를 다른 UDS 연결·응답 실패와 같이
  `DAEMON_UNAVAILABLE` transport 오류로 다룰 수 있다.
- 데몬은 socket 절대 경로를 서비스 설정으로 명시적으로 받으며 UID/GID를 임의로 바꾸지 않는다.
- MVP socket mode는 owner-only `0600`이다.
- 일반 bind는 기존 일반 파일, 디렉터리, symlink와 socket을 삭제하지 않고 시작을 거절한다.
- crash 뒤 startup recovery는 보호된 runtime 디렉터리에서 단일 daemon lock을 먼저 획득하고, 기존
  socket의 종류, owner UID, mode, link count, device·inode와 연결 결과를 모두 검증한 경우에만 stale
  socket을 제거할 수 있다. 연결 성공, 권한 오류, timeout 또는 불확실한 결과에서는 삭제하지 않는다.
- 정상 종료에서는 자신이 성공적으로 bind한 동일 device·inode의 socket만 제거한다. lock 파일은
  삭제하지 않고 file descriptor를 닫아 lock만 해제한다.
- 자세한 연결 admission과 슬롯 회수 규칙은
  [ADR 0007](decisions/0007-bound-concurrent-uds-connections.md)을 따른다.

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

- `RUNNING`: 데몬이 작업 cgroup을 만들고 모든 제한을 설정한 뒤, 그 cgroup 안에서 외부 명령의 시작을 시도했거나 실행 중이다.
- `FINISHED`: 작업 cgroup 전체와 출력 reader 정리를 확인한 뒤 종료 결과를 확정했으며, 더 이상 상태가 바뀌지 않는다.

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
| `DAEMON_ERROR` | 작업 생성 뒤 데몬 내부 오류가 발생했지만 안전한 정리와 필수 결과 근거를 모두 확인했다. |

데몬은 단일 exit code만으로 종료 원인을 추측하지 않는다. 메모리·PID 한도 초과는 cgroup 이벤트를, timeout과 cancel은 데몬 제어 상태를 함께 확인해 판정한다.

종료 원인이 겹칠 때는 다음 규칙을 사용한다.

- timeout과 cancel 같은 데몬 제어 원인은 먼저 관찰한 하나만 기록하며, 늦게 도착한 원인이 이미 기록한 값을 덮어쓰지 않는다.
- 같은 작업에서 메모리와 PID 사건이 함께 늘었으면 `memory.events.local`의 `oom_kill` 증가, `pids.events`의 `max` 증가, `memory.events.local`의 `oom` 증가 순서로 더 직접적인 근거를 선택한다.
- 안전한 정리를 완료한 데몬 내부 오류보다 위의 명시적 제어 원인과 커널 사건을 우선한다.

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

`taskAccepted`는 데몬이 작업 cgroup을 만들고 모든 제한을 설정한 뒤, 외부 명령을 해당 cgroup 안에서 시작한 후에만 반환한다. 실행 파일 또는 작업 디렉터리 문제로 `execve`가 시작되지 못하면 `taskAccepted`를 반환하지 않고, 정리를 마친 `task`/`FINISHED` 응답을 같은 요청의 직접 응답으로 반환한다. 이때 `terminationReason`은 `EXECUTION_FAILED`이고 `process.exitCode`와 `process.signal`은 모두 `null`이다. `execve`가 성공한 뒤 명령이 매우 빨리 종료해도 먼저 `taskAccepted`를 반환한다.

`submittedAt`은 유효한 `submitTask` 요청을 데몬이 받은 시각이다. `startedAt`은 cgroup 준비, 제한
적용과 read-back, pending `clone3` child의 cgroup 소속 확인을 끝낸 뒤 부모가 exec 시작 gate 신호를
성공적으로 기록한 시각이다. 이 gate commit과 같은 단조시간에서 `wallTimeLimitMs`의 절대 deadline을
만든다. `RUNNING` 공개는 별도 사건이며 child의 `execve` 성공을 확인한 뒤에만 수행한다. gate를 열기
전에 실패하거나 fail-stop이 먼저 확정되면 시작 시각과 `RUNNING`을 만들지 않는다. gate commit 뒤
`execve`가 실패하면 그 commit 시각을 `startedAt`으로 사용한 `EXECUTION_FAILED` 결과를 정리 뒤
반환한다.

`effectiveLimits`는 요청을 검증한 뒤 cgroup 제어 파일에 쓰고 다시 읽어 요청값과 정확히 같음을
확인한 적용값이다. MVP는 커널이 다른 값으로 정규화한 결과를 조용히 허용하지 않는다. 확인한 값이
요청값과 다르면 target을 시작하거나 `taskAccepted`와 공개 `taskId`를 반환하지 않고, 해당
`submitTask`를 `INTERNAL_ERROR`와 `retryable: false`로 실패시킨다.

read-back 불일치 전에 만든 임시 cgroup과 Registry·idempotency·capacity 예약은 전체 정리를 확인한
뒤 원상 복구한다. 정리를 확인할 수 없으면 일반 `INTERNAL_ERROR` 응답으로 끝내지 않고 기존
process-wide fail-stop 계약으로 전환한다. `LIMIT_EXCEEDS_POLICY`는 cgroup을 만들기 전에 요청 제한이
명시적인 daemon 배포 정책을 벗어났다고 판정한 경우에만 사용한다. 지원하지 않는 cgroup 환경을 시작
전에 발견하는 `ENVIRONMENT_UNAVAILABLE` 계약은 바뀌지 않는다. 자세한 선택 근거는
[ADR 0006](decisions/0006-use-internal-error-for-cgroup-read-back-mismatch.md)을 따른다.

```json
{
  "protocolVersion": 1,
  "requestId": "a1f6d5f2-2bf7-4a8d-b6d9-2e4d0a8860e2",
  "type": "error",
  "payload": {
    "code": "INTERNAL_ERROR",
    "message": "cgroup limit read-back verification failed",
    "retryable": false
  }
}
```

슬롯이 없으면 데몬은 작업을 만들지 않고 `CAPACITY_EXHAUSTED` 오류를 반환한다. 같은
`clientRequestId` 재전송에는 새 작업 대신 기존 `taskId`와 현재 상태를 반환한다.

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

- `wallTimeMs`는 exec 시작 gate commit의 단조시간부터 종료와 cleanup 확정 시각까지의 차이다.
  cgroup 준비, 제한 적용과 pending `clone3` 대기 시간은 포함하지 않는다. timeout 뒤 whole-cgroup
  cleanup과 결과 확정 시간은 포함하므로 `wallTimeMs`가 `wallTimeLimitMs`보다 클 수 있다.
- `cpuTimeMicros`와 `memoryPeakBytes`는 cgroup 통계에서 수집한다.
- `exitCode`와 `signal`은 해당하지 않으면 `null`이다.
- `signal`은 Linux 표준 signal의 정식 이름을 사용한다. 예를 들어 9는 `SIGKILL`, 11은 `SIGSEGV`, 15는 `SIGTERM`이다. realtime signal은 `SIGRTMIN` 또는 `SIGRTMIN+N` 형태로 반환한다.
- `EXECUTION_FAILED`로 실행 파일을 시작하지 못한 경우 `exitCode`와 `signal`은 모두 `null`이다.
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
- 작업 cgroup 제거와 `populated 0`, direct child 회수, 출력 reader 종료를 모두 확인하기 전에는 `FINISHED`를 공개하지 않는다. 격리 정리를 확인하지 못한 실행기는 새 작업을 시작하지 않으며 내부 방어 경로에서 안전한 정리를 다시 시도한다. wire에 별도 cleanup state나 cleanup 객체를 추가하지 않는다.

### `RUNNING` 이후 정리 불확실 상태

`RUNNING`을 공개한 뒤 direct child 회수, 작업 cgroup 전체 종료, `populated 0`, cgroup 제거 또는
출력 reader 회수 중 하나를 확인하지 못하면 데몬은 process-wide 정리 불확실 상태로 전환한다. 이
상태는 현재 프로세스가 살아 있는 동안 되돌리지 않는다.

- 해당 작업은 정리 완료와 필수 결과 근거를 모두 확인하기 전까지 `FINISHED`로 전환하지 않는다.
  exit code, signal 또는 timeout만으로 `EXITED`나 `DAEMON_ERROR`를 만들지 않는다.
- `getCapabilities.cgroupV2Ready`는 `false`를 반환한다. 새로운 `clientRequestId`의 `submitTask`는
  task ID, Registry 항목, cgroup과 process를 만들기 전에 `ENVIRONMENT_UNAVAILABLE`로 거절한다.
- `getTask`는 저장된 snapshot을 반환하므로 안전한 `FINISHED`를 저장하기 전까지 정리가 불확실한
  작업은 `RUNNING`으로 보인다.
- 같은 `clientRequestId`와 같은 본문의 재전송은 새 실행 없이 기존 `taskId`와 현재 저장된 snapshot을
  반환한다. 본문이 다르면 `IDEMPOTENCY_CONFLICT`를 반환한다.
- 일반 cleanup은 최초 cleanup 단계가 시작된 단조시간에 작업별 cleanup timeout을 한 번만 더한 절대
  deadline을 사용한다. child 회수, cgroup 정리와 output reader 회수 단계마다 timeout을 새로 시작하지
  않는다.
- 정리 불확실성을 처음 관찰하면 별도의 양수·유한 내부 fail-stop timeout으로 process-wide fail-stop
  deadline을 정확히 한 번 만든다. 작업별 cleanup deadline이 이미 소진됐어도 fail-stop 예산은 온전히
  남아 있다. 이후 다른 오류는 이 deadline을 연장하지 않는다.
- 같은 작업 재시도, 다른 활성 작업 정리, 결과 저장, 조회 유예와 마지막 종료 방어가 fail-stop
  deadline을 공유한다. 전체 최악 시간은 작업별 cleanup timeout과 fail-stop timeout의 합으로 제한된다.
  fail-stop timeout은 daemon 내부 설정 또는 생성자 입력이며 새 wire field가 아니다.
- 신규 UDS 연결과 신규 작업 수락을 중단하고, 모든 활성 작업에 whole-cgroup 종료와 정리를 시작한다.
  안전한 정리와 필수 결과 근거를 모두 확인한 작업만 기존 lifecycle로 `FINISHED`를 정확히 한 번
  저장한다. fail-stop 때문에 종료한 작업은 다른 선행 종료 근거가 없을 때 기존 `DAEMON_ERROR`를
  사용한다.
- 복구 성공 여부와 관계없이 정상 상태로 돌아가지 않는다. fail-stop deadline이 남아 있는 동안 이미
  연결된 클라이언트의 capabilities, 작업 조회와 idempotency 재전송을 처리한다. 새로운
  `clientRequestId`의 제출은 `ENVIRONMENT_UNAVAILABLE`로 거절하며 side effect를 만들지 않는다.
  fail-stop deadline에 도달하거나 모든 활성 작업 정리가 끝나고 기존 연결이 모두 닫히면 0이 아닌
  종료 코드로 종료한다. 연결 종료로 응답을 받지 못한 SDK는 기존 `DAEMON_UNAVAILABLE`을 사용한다.
- 치명적 로그에는 `taskId`, 실패 단계, 재시도 결과와 정리하지 못한 항목만 기록한다. 명령 인자
  전체, 환경 변수 값과 stdout·stderr 원문은 기록하지 않는다.

### 재시작 복구

- 하나의 서비스 인스턴스는 인스턴스 전용 socket 부모 runtime 디렉터리와 supervisor가 독점 위임한
  cgroup root를 사용한다. runtime 디렉터리는 daemon effective UID가 소유하고 group·other가 쓸 수
  없어야 한다. lock 경로는 그 디렉터리의 `.taskcaged.lock`이며 mode는 `0600`이다.
- 시작 순서는 단일 daemon lock 획득, stale socket 확인·복구, 남은 TaskCage cgroup 정리, 전체 cgroup
  사전 검사, socket bind, UDS 요청 수락이다. 앞 단계가 끝나기 전에는 뒤 단계의 side effect를 만들지
  않는다.
- socket은 명시된 경로이거나 `connect`가 한 번 실패했다는 이유만으로 삭제하지 않는다. symlink를
  따라가지 않고 신원과 권한을 확인하며, `ECONNREFUSED`는 다른 검증을 모두 통과한 뒤에만 stale
  근거로 사용할 수 있다. 삭제 직전에 device·inode를 포함한 신원을 다시 확인한다.
- 잔여 cgroup의 `populated 0`과 제거를 확인한 뒤 전체 cgroup 사전 검사를 새로 통과하기 전에는 요청을
  수락하거나 준비됐다고 보고하지 않는다.
- crash 뒤 새 daemon이 설정으로 명시된 위임 root의 정확한 `manager`에서 시작한 경우에는 현재 daemon
  외의 직접 프로세스와 예상 밖 하위 구조가 없음을 확인한 뒤 그 manager를 보존하고 형제 `jobs`의 잔여
  작업을 정리한다. 부모 root가 명시되지 않았거나 다른 하위 cgroup이면 안전하게 추론하지 않고 시작을
  거절한다.
- 소유권 확인, stale 판정, socket 복구, cgroup 복구 또는 사전 검사에 실패하면 socket을 bind하거나
  신규 작업을 시작하지 않고 startup 오류로 종료한다. 이는 새 protocol 오류 코드가 아니다.
- MVP Registry는 메모리 기반이므로 재시작 전 작업 snapshot과 idempotency mapping을 복구하지
  않는다. 이전 `taskId` 조회는 `TASK_NOT_FOUND`가 될 수 있다.
- 같은 `clientRequestId`를 재제출하면 새 작업이 만들어질 수 있지만, 이전 잔여 cgroup 정리가 끝난
  뒤에만 허용한다. 따라서 이전 작업과 새 작업의 동시 실행은 막지만 장애와 재시작을 가로지르는
  정확히 한 번 실행은 보장하지 않는다.

이 동작은 기존 `RUNNING`, `FINISHED`, `ENVIRONMENT_UNAVAILABLE`, `DAEMON_UNAVAILABLE`,
`TASK_NOT_FOUND`와 `IDEMPOTENCY_CONFLICT`만 사용한다. protocol v1에 cleanup field, 상태, 응답
타입 또는 오류 코드를 추가하지 않는다. 파일 판정과 lock의 상세 절차는
[ADR 0005](decisions/0005-own-startup-recovery-before-removing-stale-socket.md)를 따른다.

## 오류 코드

| 코드 | 의미 | 재시도 |
|---|---|---|
| `INVALID_REQUEST` | 필수 필드, 타입, 경로 또는 상한 검증 실패 | 아니오 |
| `UNSUPPORTED_PROTOCOL_VERSION` | 지원하지 않는 프로토콜 버전 | 아니오 |
| `FRAME_TOO_LARGE` | 프레임 크기가 최대치를 넘음 | 아니오 |
| `ENVIRONMENT_UNAVAILABLE` | cgroup v2, 필수 controller·권한 또는 안전한 작업 격리 상태를 사용할 수 없음 | 아니오 |
| `CAPACITY_EXHAUSTED` | 전역 실행 슬롯이 모두 사용 중임 | 예 |
| `TASK_NOT_FOUND` | 작업이 없거나 보관 기간이 지남 | 아니오 |
| `TASK_ALREADY_FINISHED` | 완료된 작업 취소 요청 | 아니오 |
| `IDEMPOTENCY_CONFLICT` | 같은 clientRequestId에 다른 요청 본문을 사용함 | 아니오 |
| `LIMIT_EXCEEDS_POLICY` | cgroup 생성 전에 요청 제한이 데몬의 명시적인 배포 정책을 벗어남 | 아니오 |
| `DAEMON_UNAVAILABLE` | SDK가 UDS 연결 또는 응답을 얻지 못함 | 예 |
| `INTERNAL_ERROR` | 예상하지 못한 데몬 오류 또는 cgroup 제한값 read-back 불일치 | 상황에 따라. read-back 불일치는 아니오 |

`DAEMON_UNAVAILABLE`은 소켓 연결·읽기·쓰기 실패를 Java SDK가 표현하는 로컬 오류이며, 데몬이 전송하는 JSON 오류가 아니다.

cgroup 제한값 read-back 불일치의 `INTERNAL_ERROR`는 `retryable: false`다. 같은 daemon에서 같은 요청을
즉시 다시 보내도 안전한 제한 적용을 보장할 수 없기 때문이다. 이 응답에는 cgroup 경로, 제어 파일,
예상값·실제값 같은 내부 진단 정보를 새 field로 넣지 않으며 SDK는 `message`를 분기 기준으로 사용하지
않는다.

정리 불확실 상태에서 받은 `ENVIRONMENT_UNAVAILABLE`은 같은 데몬 프로세스에 `submitTask`를 다시
시도하라는 뜻이 아니다. SDK는 해당 프로세스의 연결이 종료될 때까지 새 실행을 재시도하지 않는다.
supervisor가 데몬을 재시작한 뒤 SDK가 다시 연결하고 `getCapabilities.cgroupV2Ready: true`를 확인한
경우에만 새 호출을 시도할 수 있다. 메모리 Registry는 재시작 뒤 복구되지 않으므로 같은
`clientRequestId`도 새 작업을 만들 수 있으며, 장애와 재시작을 가로지르는 정확히 한 번 실행은
보장하지 않는다.

## 병렬 개발과 fixture

Java SDK와 Rust 데몬은 아래 fixture를 공유 계약으로 사용한다. 실제 파일은 `protocol-fixtures/v1/`에 둔다.

```text
submit-task-valid.json
task-accepted.json
task-running.json
task-result-execution-failed.json
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
