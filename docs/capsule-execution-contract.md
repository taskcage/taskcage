# Capsule 실행 계약 v1

## 상태와 범위

이 문서는 Capsule을 실행하는 모든 backend가 공유해야 하는 언어·transport 독립 계약이다. 현재
Local Profile Protocol v2의 wire envelope와 field를 교체하지 않는다. Local UDS daemon은 v2의
`ProfileRequest`와 `FinishedProfileTaskSnapshot`을 사용하고, Java `CapsuleRunner`와 향후
`EmbeddedRunner`는 이 문서의 의미를 보존해야 한다.

이 계약은 실행 방법을 정의하지만 archive 배포 형식, Runtime Package 다운로드, Hub, TLS transport는
정의하지 않는다. 구현은 하나의 Rust `taskcage-core`를 공유해야 하며, `taskcaged`와 Embedded용
`taskcage-exec`는 각각 daemon·private helper adapter로 동작한다. EmbeddedRunner는 `taskcaged` child
daemon을 시작하는 방식으로 구현하지 않는다. 현재 분리 단계에서는 cgroup 경로·preflight·deadline·출력
캡처·프로세스 실행기를 `taskcage-core`가 소유하고, `taskcaged`는 UDS/TLS·host policy·protocol/task
lifecycle 조정만 담당한다. daemon adapter는 core가 제공한 검증된 실행 primitives를 host policy와
wire protocol에 연결한다.

## 핵심 객체

### Capsule identity

Capsule identity는 다음 두 필드의 immutable pair다.

```json
{"name":"ffmpeg-audio-to-wav","version":"1.0.0"}
```

- `name`: `[a-z][a-z0-9-]{0,62}`
- `version`: prerelease/build metadata가 없는 strict `MAJOR.MINOR.PATCH`
- 공개된 동일 identity는 변경하지 않는다. 계약·Runtime Package·정책이 바뀌면 새 version을 발행한다.

Capsule identity와 그 안의 Execution Profile identity는 별개다. 하나의 Capsule은 MVP에서 하나의
Profile을 제공하며, 요청은 두 identity를 모두 보존한다.

### Capsule request

요청은 Capsule identity와 기존 ProfileRequest를 함께 가진다.

```json
{
  "capsule": {"name":"ffmpeg-audio-to-wav","version":"1.0.0"},
  "profile": {"name":"ffmpeg-audio-to-wav","version":"1.0.0"},
  "inputs": {},
  "resourceOverrides": {}
}
```

입력은 Profile schema가 선언한 typed value만 허용한다. 호출자는 executable path, shell 문자열,
argv 배열, environment, working directory 또는 output path를 지정할 수 없다. daemon/backend는
Capsule의 서명·allowlist·Runtime Package digest·Profile schema·override를 최종 검증한다.

`clientRequestId`는 transport가 제공하는 idempotency key이며 실행 계약의 일부가 아니다. 응답이
유실되면 같은 key와 동일 request를 재제출해 같은 Task/result를 조회한다.

## 실행 의미

모든 backend는 다음 순서를 보장해야 한다.

```text
Capsule resolve
→ contract and policy validation
→ Runtime Package verification
→ input staging/snapshot
→ resource limit application and read-back
→ process execution
→ output validation and atomic publish
→ whole-task cleanup confirmation
→ terminal result
```

검증·제한·staging 중 하나라도 실패하면 외부 process를 시작하지 않는다. timeout, cancel, execution
error, output validation failure는 루트 PID만 종료하는 것으로 끝내지 않고 모든 descendant와 staging
임시물을 정리한 뒤 terminal result를 반환한다.

## 결과 의미

모든 backend는 cleanup-confirmed terminal result를 반환한다. 결과에는 최소한 다음이 포함된다.

- Capsule identity와 Profile identity
- Task identity와 terminal state
- `SUCCEEDED` 또는 `FAILED` outcome
- termination reason, exit code/signal
- submitted/started/finished time과 wall time
- CPU time과 memory peak
- bounded stdout/stderr tail과 truncation 여부
- 성공 시 선언된 output Artifact만, 실패 시 failure code/message만

`SUCCEEDED`는 다음을 모두 만족해야 한다.

1. 프로세스가 정상 종료했다(`exitCode == 0`).
2. 선언된 output schema와 media type·size·digest 검증을 통과했다.
3. output publish가 atomic하게 완료됐다.
4. 프로세스·descendant·cgroup·staging cleanup이 확인됐다.

그 외에는 `FAILED`다. 프로세스가 exit code 0으로 끝났더라도 output 검증이나 cleanup이 실패하면
성공으로 바꾸지 않는다. 실패 결과에는 partial output을 노출하지 않는다.

## Backend 동등성

`EmbeddedRunner`와 daemon-backed `ExternalRunner`는 내부 구현과 transport만 다르다. 동일한 Capsule,
ProfileRequest, input Artifact와 resource override에 대해 다음을 동일하게 유지해야 한다.

- validation acceptance/rejection
- timeout·cancel·execution failure의 termination reason
- output publish와 failure 시 artifact 비공개
- cleanup 완료 후 terminal result 공개
- idempotency key 재제출 의미

backend가 제공하지 않는 기능을 제한 없는 fallback으로 실행해서는 안 된다. 예를 들어 Embedded backend가
cgroup 제한이나 whole-task cleanup을 증명할 수 없으면 실행을 거부해야 한다.

## MVP 제외

- 하나의 Capsule에 여러 Profile을 조합하는 orchestration
- 여러 input/output Artifact collection
- 임의 CLI를 허용하는 Raw Command Capsule
- Hub 검색·자동 설치·원격 package fetch
- 보안 sandbox(namespace, seccomp, filesystem/network isolation)

## Conformance fixture

동일 의미를 Rust와 Java에서 검증하기 위한 fixture는
[`protocol-fixtures/capsule-v1`](../protocol-fixtures/capsule-v1/)에 둔다. fixture는 wire transport가
아니라 공통 request/result shape와 성공·실패 invariant를 고정한다.
