# Capsule 실행 계약 v1

## 목적과 상태

이 문서는 Capsule을 실행하는 모든 backend가 공유하는 언어·transport 독립 실행 의미를 고정한다.
첫 구현 기준선은 daemon-backed **ExternalRunner**다. Java SDK는 UDS 또는 TLS transport로 daemon에
연결하지만, 호출자가 받는 Capsule request와 terminal result의 의미는 transport에 따라 달라지지 않는다.

현재 Local Profile Protocol v2와 Remote Protocol v1의 wire envelope를 이 문서가 즉시 교체하지는 않는다.
기존 daemon과 SDK는 이 계약을 향해 adapter를 연결한다. EmbeddedRunner는 후속 선택적 backend이며,
`taskcaged` child daemon을 시작하는 방식으로 구현하지 않는다.

이 문서는 다음을 정의한다.

- Capsule identity, Profile identity와 typed request의 의미
- Runtime Package·Artifact·resource policy 검증 순서
- 성공·실패·취소·timeout의 cleanup-confirmed terminal result
- ExternalRunner와 이후 EmbeddedRunner가 지켜야 할 동등성 기준

archive format, Package download, Hub, TLS handshake와 credential 형식은 각각의 별도 계약이 담당한다.

## 핵심 객체

### Capsule identity와 Profile identity

Capsule identity는 immutable `(name, version)` pair다.

```json
{"name":"ffmpeg-audio-to-wav","version":"1.0.0"}
```

- `name`은 `[a-z][a-z0-9-]{0,62}`다.
- `version`은 prerelease/build metadata가 없는 strict `MAJOR.MINOR.PATCH`다.
- 공개된 같은 identity는 변경하지 않는다. 계약·Package·정책이 바뀌면 새 version을 발행한다.

Profile identity는 Capsule 내부 실행 계약을 가리키는 별도 field다. v1 Capsule은 정확히 하나의 Profile만
가진다. 따라서 v1 request의 Profile identity는 Capsule identity와 name·version이 모두 일치해야 한다.
불일치는 `CAPSULE_PROFILE_MISMATCH`로 실행 전 거절되며, Task·Artifact staging·cgroup·target process를
만들지 않는다. 여러 Profile을 가진 Capsule은 v1 범위 밖이다.

### Capsule request

Capsule request는 호출 의도와 선언된 입력만 표현한다.

```json
{
  "capsule": {"name": "ffmpeg-audio-to-wav", "version": "1.0.0"},
  "profile": {"name": "ffmpeg-audio-to-wav", "version": "1.0.0"},
  "inputs": {
    "source": {"kind": "ARTIFACT", "digest": "sha256:…", "sizeBytes": 1024},
    "sample_rate_hz": {"kind": "INT64", "value": 16000},
    "channels": {"kind": "INT64", "value": 1}
  },
  "resourceOverrides": {
    "limits": {"wallTimeLimitMs": 120000}
  }
}
```

`inputs`는 Profile schema가 선언한 slot을 정확히 한 번씩 가져야 한다. caller는 executable path, shell
문자열, argv, environment, working directory, output file name 또는 Artifact staging path를 지정할 수 없다.
Runtime Package digest, Profile schema, allowed override와 host policy는 daemon/backend가 최종 검증한다.

`clientRequestId`는 Capsule 자체의 field가 아니라 runner/transport가 받는 caller-owned idempotency key다.
응답이 유실되면 같은 key와 byte-equivalent request를 재제출한다. 같은 key에 다른 request를 사용하면
`IDEMPOTENCY_CONFLICT`이며 재시도할 수 없다.

### Artifact input과 output

`ARTIFACT`는 content digest, size와 declared media type으로 식별되는 불변 input snapshot을 뜻한다. Local UDS는
`LOCAL_INPUT` descriptor, TLS transport는 managed upload/reference로 이를 전달할 수 있다. transport별 경로·URI·upload
identifier는 adapter가 해석하며, execution backend는 target을 시작하기 전에 같은 digest와 size의 private snapshot을
확보해야 한다.

성공 output은 Profile이 선언한 slot만 publish한다. v1 Capsule은 output Artifact 하나만 지원한다. output의
path/reference, digest, size와 media type은 terminal result에 반환한다. 실패·취소·timeout에서는 Artifact map이
비어 있어야 하며 partial output과 staging residue를 공개하지 않는다.

Remote file adapter는 응답 유실 뒤 전체 작업을 다시 시작하지 않도록 upload, submit/terminal observation,
download를 복구 가능한 단계로 노출한다. upload는 caller-owned artifact id를 받아 완료 receipt를 반환하며, caller는
receipt를 보관한 뒤 같은 Artifact reference와 caller-owned `clientRequestId`로 submit을 재시도한다. submit 응답이
유실된 뒤 upload를 다시 시작해서는 안 된다. download는 cleanup-confirmed 성공 결과의 output Artifact만 사용하며
upload나 submit을 수행하지 않는다. 따라서 download 실패는 같은 terminal result로 download만 재시도하고 Capsule
process를 다시 실행하지 않는다.

## 실행 순서와 안전 경계

모든 Linux execution backend는 다음 순서를 지킨다.

```text
Capsule resolve
→ Capsule/Profile/schema/policy validation
→ Runtime Package digest and platform verification
→ input Artifact snapshot
→ cgroup limit apply and read-back
→ process execution
→ output validation and atomic publish
→ whole-task cleanup confirmation
→ terminal result
```

validation, Package verification, input snapshot 또는 cgroup read-back 중 하나라도 실패하면 target을 시작하지
않는다. timeout, cancel, execution error, output validation error에서는 root PID 하나가 아니라 해당 Task의
모든 descendant와 cgroup, staging data를 정리한 뒤에만 terminal result를 공개한다. 이 정리를 증명할 수
없으면 성공이나 cleanup-confirmed 실패 결과로 바꾸지 않고 backend failure로 처리한다.

## Terminal result

terminal result는 process, cgroup, output reader와 staging cleanup이 확인된 뒤의 immutable snapshot이다.

```json
{
  "taskId": "44444444-4444-4444-8444-444444444444",
  "capsule": {"name": "ffmpeg-audio-to-wav", "version": "1.0.0"},
  "profile": {"name": "ffmpeg-audio-to-wav", "version": "1.0.0"},
  "state": "FINISHED",
  "outcome": "SUCCEEDED",
  "terminationReason": "EXITED",
  "cleanupConfirmed": true,
  "process": {"exitCode": 0, "signal": null},
  "timing": {
    "submittedAt": "2026-08-20T00:00:00Z",
    "startedAt": "2026-08-20T00:00:01Z",
    "finishedAt": "2026-08-20T00:00:02Z",
    "wallTimeMs": 1000
  },
  "usage": {"cpuTimeMicros": 7000, "memoryPeakBytes": 1048576},
  "output": {
    "stdoutTail": "",
    "stderrTail": "",
    "stdoutTruncated": false,
    "stderrTruncated": false
  },
  "artifacts": {},
  "failure": null
}
```

| Field | 규칙 |
|---|---|
| `state` | terminal result에서는 항상 `FINISHED`다. |
| `outcome` | `SUCCEEDED` 또는 `FAILED`다. |
| `terminationReason` | `EXITED`, `EXECUTION_FAILED`, `CANCELLED`, `TIMED_OUT`, `MEMORY_LIMIT_EXCEEDED`, `PROCESS_LIMIT_EXCEEDED` 또는 cleanup-confirmed `DAEMON_ERROR`다. |
| `process` | exec가 시작되지 않은 경우 두 field가 `null`일 수 있다. 그렇지 않으면 exit code 또는 signal을 기록한다. |
| `timing`, `usage`, `output` | Java `ExecutionResult`와 같은 의미다. output은 bounded tail과 truncation 여부를 항상 포함한다. |
| `artifacts` | 성공이면 Profile의 선언된 유일 output 하나, 실패면 빈 object다. |
| `failure` | 실패면 stable `code`와 diagnostic `message`, 성공이면 `null` 또는 wire transport에서 생략이다. message는 안정된 비교 대상이 아니다. |

`SUCCEEDED`는 다음을 모두 만족할 때만 가능하다.

1. target이 `exitCode == 0`으로 종료했다.
2. 선언된 output의 존재, size, media type과 digest 검증을 통과했다.
3. output publish가 atomic하게 완료됐다.
4. process tree, cgroup, staging과 output reader cleanup이 확인됐다.

그 외 모든 terminal execution은 `FAILED`다. 예를 들어 exit code 0 뒤 output validation이 실패해도
`FAILED`이며 Artifact를 노출하지 않는다. transport disconnect, malformed protocol, authentication failure 또는
cleanup uncertainty는 terminal result가 아니라 runner/backend error다.

## 취소, timeout과 재시도

- cancel은 terminal result가 아니며 whole-task cleanup이 끝난 뒤 `CANCELLED` terminal result로 관찰된다.
- wall-time expiry는 `TIMED_OUT` terminal result다.
- cancel과 process exit이 경쟁하면 daemon이 먼저 확정한 lifecycle control reason을 사용한다.
- cleanup-confirmed terminal result는 같은 `clientRequestId` 재제출과 result 조회에서 같은 Task/result를 반환한다.
- pre-execution validation rejection은 Task identity를 만들지 않으며 caller는 수정된 새 request와 새 idempotency
  key로 제출해야 한다.

## Backend 동등성

ExternalRunner는 v1의 권위 있는 실행 기준선이다. 이후 EmbeddedRunner는 동일 Capsule, input snapshot,
resource override와 idempotency key에 대해 다음을 유지해야 한다.

- validation acceptance/rejection과 stable failure code
- Runtime Package·Profile resolve와 argv materialization
- timeout, cancel, execution failure의 termination reason
- output publish, failure 시 Artifact 비공개와 cleanup-confirmed result
- idempotency 재제출 의미

Embedded backend가 cgroup 제한이나 whole-task cleanup을 증명할 수 없으면 제한 없는 fallback으로 실행해서는
안 된다. ExternalRunner와 EmbeddedRunner의 내부 transport나 helper lifecycle은 달라도 public Capsule result
의미는 달라질 수 없다.

## v1 제외 범위

- 하나의 Capsule에 여러 Profile을 조합하는 orchestration
- 여러 input/output Artifact collection
- caller-provided Raw Command Capsule
- Hub 검색, 자동 Package download와 remote package fetch
- 보안 sandbox(namespace, seccomp, filesystem/network isolation)

## Conformance fixture

[`protocol-fixtures/capsule-v1`](../protocol-fixtures/capsule-v1/)은 transport framing이 아닌 위 semantic
contract를 고정한다. Java와 Rust 구현은 success, process failure, output contract failure, timeout과 cancel
fixture의 identity, outcome, Artifact visibility, timing/output/usage shape를 임의로 바꾸지 않아야 한다.
Rust `capsule_contract_fixtures`와 Java `CapsuleContractFixtureCompatibilityTest`는 corpus의 정확한 파일 집합과
request, pre-execution identity mismatch, cleanup-confirmed terminal result를 직접 소비한다. fixture를 바꾸면
두 구현의 conformance test도 함께 통과해야 한다.
