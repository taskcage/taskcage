# TaskCage Local Profile Core API v2

> 상태: Local Profile의 **현재 Core 계약**이다. installed Profile은 signed Bundle import로 catalog에
> 등록될 수 있으며, Bundle archive 형식과 설치 절차는 [Bundle 형식](bundle-format.md)에 정의한다. 이
> 문서는 archive 자체가 아니라 실행 request·result 의미를 정한다.

## 목적과 범위

Local Profile Core는 신뢰된 Linux host의 daemon에 이미 설치된 Execution Profile을 이름과 버전으로
실행하는 최소 공통 계약이다. Java Core SDK와 Rust daemon은 이 계약으로 같은 요청, 결과, 오류와
멱등성 의미를 공유한다.

이 문서는 다음만 정한다.

- Local UDS 위 additive Protocol v2 request/result
- versioned Profile identity, typed input, Local Artifact, resource override
- 하나의 required Local Artifact input과 하나의 declared output Artifact
- Profile Task의 submit, observe, await, cancel, 실패와 cleanup 의미
- Java Core SDK의 Profile API와 transport/error 경계

다음은 이 문서의 범위가 아니다.

- FFmpeg·Chromium 등의 Profile Binding과 domain type
- Runtime Package cache, Bundle archive 형식, Profile import/install command
- Remote transport, TLS, object storage, Artifact upload/download
- Spring Boot integration, 임의 JSON value, multi-output, output overwrite
- Raw Command Protocol v1의 request, response, field 또는 오류 의미 변경

## v1과 v2의 경계

Protocol v2는 v1을 교체하지 않는다. v1은 계속 Raw Command 전용 권위 계약이며
[`api-mvp.md`](api-mvp.md)를 따른다. Local transport, UDS 권한, 4-byte big-endian length prefix,
1 MiB frame 상한, UTF-8 JSON, duplicate key 거부와 요청-응답 순서는 v1과 같다.

클라이언트는 먼저 Protocol v1 `getCapabilities`를 호출한다. 기존 `protocolVersions` 배열에 `2`가
포함될 때만 v2 request를 보낸다. daemon은 cgroup preflight, Artifact root, installed Profile catalog와
Profile 실행 경로를 모두 안전하게 준비한 경우에만 `[1, 2]`를 광고한다. 어느 하나라도 준비되지 않으면
`[1]`만 광고하며 Raw Command로 fallback하지 않는다.

| `protocolVersion` | request `type` | 계약 |
|---|---|---|
| `1` | `getCapabilities`, `submitTask`, `getTask`, `cancelTask` | 기존 v1 byte, field, state, error 의미를 유지한다. |
| `1` | `submitProfile`, `getProfileResult` | `INVALID_REQUEST` |
| `2` | `submitProfile`, `getProfileResult` | 이 문서를 적용한다. |
| `2` | v1 operation 또는 그 밖의 operation | `INVALID_REQUEST` |
| 그 밖의 정수 | 모든 operation | `UNSUPPORTED_PROTOCOL_VERSION` |

Profile Task도 v1 `getTask`와 `cancelTask`로 조회·취소할 수 있다. 두 v1 응답에는 Profile identity나
Artifact field를 추가하지 않는다. Raw Task를 v2 `getProfileResult`로 조회하면
`TASK_KIND_MISMATCH`다.

모든 v2 frame은 다음 envelope를 사용한다.

```json
{
  "protocolVersion": 2,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "submitProfile",
  "payload": {}
}
```

`requestId`는 UUID request/response correlation ID다. request의 unknown field, 잘못된 JSON type,
duplicate key와 잘못된 UUID는 `INVALID_REQUEST`다. SDK는 성공 response의 unknown field는 무시하지만
필수 field와 type이 다르면 `TaskCageProtocolException`으로 처리한다.

## 설치된 Profile

Profile은 daemon deployment가 명시적으로 설치한 immutable execution contract다. caller는 다음 identity만
선택한다.

```json
{
  "name": "file-copy",
  "version": "1.0.0"
}
```

- `name`은 1~63 ASCII bytes이며 `[a-z][a-z0-9-]{0,62}`만 허용한다.
- `version`은 strict `MAJOR.MINOR.PATCH`이며 각 component는 0 또는 0이 아닌 숫자로 시작하는 decimal
  integer다. prerelease와 build metadata는 v0.2 Core에서 허용하지 않는다.
- 같은 `(name, version)`의 installed Profile은 교체할 수 없다. 변경은 새 version으로 설치한다.
- installed Profile이 없으면 `PROFILE_NOT_FOUND`, retryable `false`다.

Profile definition의 file format은 이 문서가 정하지 않는다. 다만 #150의 daemon 구현은 각 Profile마다
아래의 resolved contract를 가져야 한다.

- scalar input slot의 이름, kind, required 여부와 value validation rule
- 정확히 하나의 required `LOCAL_INPUT` slot
- 정확히 하나의 required output slot, fixed output file name, media type, maximum bytes
- program, argv template, environment, working directory와 ResourcePolicy defaults/allowed overrides

caller가 profile definition의 program, argv, environment, working directory, output file name 또는 Artifact root를
바꾸는 경로는 없다. daemon만 이 definition을 shell-free argv와 기존 Task lifecycle으로 resolve한다.

## `ProfileRequest`

`submitProfile.payload`가 하나의 generic `ProfileRequest`다.

```json
{
  "clientRequestId": "22222222-2222-4222-8222-222222222222",
  "profile": {
    "name": "file-copy",
    "version": "1.0.0"
  },
  "inputs": {
    "source": {
      "kind": "LOCAL_INPUT",
      "path": "jobs/42/source.txt",
      "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "sizeBytes": 12
    },
    "label": {
      "kind": "STRING",
      "value": "archive"
    },
    "retain_metadata": {
      "kind": "BOOLEAN",
      "value": true
    },
    "priority": {
      "kind": "INT64",
      "value": 3
    }
  },
  "resourceOverrides": {
    "limits": {
      "wallTimeLimitMs": 300000
    }
  }
}
```

`clientRequestId`, `profile`과 `inputs`는 required다. `resourceOverrides`는 optional이다. `inputs`는
installed Profile이 선언한 slot을 정확히 한 번씩 가져야 하며, 누락·추가·case 차이·kind 불일치는
`INVALID_PROFILE_INPUT`이다. key는 1~64 ASCII bytes의 `[a-z][a-z0-9_-]{0,63}`이다.

### Typed input value

v0.2 Core는 아래 네 object form만 지원한다. bare JSON scalar, `null`, float, array, map, base64 binary와
알 수 없는 `kind`는 `INVALID_PROFILE_INPUT`이다.

| `kind` | required field | JSON rule |
|---|---|---|
| `STRING` | `value` | UTF-8 string. 길이와 허용 값은 Profile slot schema가 검증한다. |
| `INT64` | `value` | exponent나 fraction 없는 integral JSON number이며 signed 64-bit 범위다. |
| `BOOLEAN` | `value` | JSON `true` 또는 `false`다. |
| `LOCAL_INPUT` | `path`, `digest`, `sizeBytes` | #149의 Local input Artifact descriptor와 동일하다. |

각 Profile은 정확히 하나의 required `LOCAL_INPUT` slot을 선언한다. v0.2 Core는 여러 input Artifact,
optional Artifact 또는 Artifact collection을 지원하지 않는다. `LOCAL_INPUT`의 `path`, SHA-256 digest,
size, root resolution, snapshot, symlink/mount rejection과 caller input ownership은
[Local Artifact 계약](local-artifact-contract.md)을 그대로 따른다.

`LOCAL_INPUT` validation 또는 snapshot digest mismatch가 나면 각각 `INVALID_ARTIFACT_PATH` 또는
`ARTIFACT_DIGEST_MISMATCH`, retryable `false`다. preflight snapshot은 생성될 수 있지만, Task record,
Registry reservation, task cgroup 또는 executable process는 만들지 않으며 실패 전에 preflight file을
제거한다.

### Resource override

`resourceOverrides`가 있으면 `limits` 또는 `output` 중 하나 이상을 가져야 하며, 빈 object는
`INVALID_PROFILE_INPUT`이다.

```json
{
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
```

- 모든 nested field는 optional이지만, 지정한 값은 positive integral JSON number여야 한다.
- `cpuMax`는 quota와 period를 함께 가져야 하며, 비율은 integer cross-multiplication으로 비교한다.
- output tail은 v1과 같이 stream마다 1~65,536 bytes, 합계 131,072 bytes 이하다.
- unspecified value는 Profile ResourcePolicy default를 사용한다.
- 지정된 field는 Profile의 allowed override이면서 그 maximum과 daemon deployment maximum을 모두 넘지
  않아야 한다. default보다 크거나 작은 override를 허용할지는 Profile이 정한 maximum으로만 결정한다.
- 모든 완성 effective value는 target 시작 전에 검증하며, v1과 같은 cgroup read-back 불일치는 target을
  시작하지 않고 `INTERNAL_ERROR`로 끝낸다.

허용되지 않았거나 policy를 넘은 override는 `LIMIT_EXCEEDS_POLICY`, retryable `false`다.

## v2 operation과 결과

### `submitProfile`

`submitProfile`은 profile identity, input schema, Artifact preflight, resource override, idempotency와 capacity를
검증한다. Profile execution은 v1 Runner와 같은 cgroup·output drain·cleanup lifecycle을 사용한다.

target이 cgroup 안에서 시작되면 daemon은 `profileAccepted`를 반환한다.

```json
{
  "protocolVersion": 2,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "profileAccepted",
  "payload": {
    "taskId": "44444444-4444-4444-8444-444444444444",
    "state": "RUNNING",
    "profile": { "name": "file-copy", "version": "1.0.0" },
    "effectiveResources": {
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
}
```

target이 시작되기 전 거절은 top-level `error` response다. target이 lifecycle에 들어간 뒤 exec failure를
포함해 terminal state가 되면 cleanup-confirmed `profileResult`를 반환하거나 조회에서 제공한다.

### `getProfileResult`

```json
{
  "protocolVersion": 2,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "getProfileResult",
  "payload": { "taskId": "44444444-4444-4444-8444-444444444444" }
}
```

실행 중 응답은 `type: "profileResult"`이며 `taskId`, `state: "RUNNING"`, `profile`, `submittedAt`,
`startedAt`을 가진다. finished 응답은 다음 field를 추가한다.

| field | 규칙 |
|---|---|
| `profileOutcome` | `SUCCEEDED` 또는 `FAILED` |
| `terminationReason`, `process`, `timing`, `usage`, `output` | v1 `task` finished payload와 같은 의미와 enum이다. |
| `artifacts` | `SUCCEEDED`면 Profile의 유일한 output slot 하나, `FAILED`면 빈 object다. |
| `failure` | `FAILED`에서 required인 `{ code, message }`, `SUCCEEDED`에서는 없다. `message`는 안정된 API가 아니다. |

성공 output Artifact는 #149의 `LOCAL_FILE` shape다. `path`는 configured Artifact root 기준 상대 path이고
caller-specified path가 아니다.

```json
{
  "kind": "LOCAL_FILE",
  "path": "tasks/44444444-4444-4444-8444-444444444444/result.txt",
  "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "sizeBytes": 12,
  "mediaType": "text/plain"
}
```

`SUCCEEDED`는 다음을 모두 만족할 때만 가능하다: `terminationReason=EXITED`, exit code `0`, declared output
검증, no-overwrite publish, cgroup/process/output reader cleanup, Artifact staging cleanup과 finished snapshot
저장. output contract/publish/cleanup 실패에서는 `SUCCEEDED`나 published Artifact를 만들지 않는다.

`FAILED.failure.code`는 process failure에는 v1 termination reason 또는 `PROCESS_EXITED_NONZERO`, Artifact
staging에는 `OUTPUT_CONTRACT_VIOLATION` 또는 `OUTPUT_PUBLISH_FAILED`를 사용한다. `terminationReason`은
exit code 하나로 추측하지 않고 기존 cgroup evidence 분류를 유지한다.

`getProfileResult`에 없는 task는 `TASK_NOT_FOUND`, Raw Task ID는 `TASK_KIND_MISMATCH`다. cancellation은
새 v2 operation을 만들지 않고 v1 `cancelTask`로 요청한다. Profile Task cancel도 v1의 cleanup-confirmed
`taskCancelled` 의미를 유지하며, 그 뒤 `getProfileResult`는 `profileOutcome: "FAILED"`와
`failure.code: "CANCELLED"`를 반환한다.

### Error response

수락 전 오류는 아래 form이다.

```json
{
  "protocolVersion": 2,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "error",
  "payload": {
    "code": "INVALID_PROFILE_INPUT",
    "message": "inputs.source must be LOCAL_INPUT",
    "retryable": false
  }
}
```

v2는 v1 오류 코드에 더해 아래 코드를 사용한다.

| code | meaning | retryable |
|---|---|---|
| `PROFILE_NOT_FOUND` | identity가 설치된 Profile과 일치하지 않음 | false |
| `INVALID_PROFILE_INPUT` | slot, typed value, 빈 override 또는 Profile input schema 위반 | false |
| `INVALID_ARTIFACT_PATH` | Artifact root escape, symlink, mount crossing 또는 non-regular input | false |
| `ARTIFACT_DIGEST_MISMATCH` | source snapshot의 SHA-256 또는 size가 descriptor와 다름 | false |
| `TASK_KIND_MISMATCH` | Raw/Profile task를 잘못된 result operation으로 조회 | false |

`CAPACITY_EXHAUSTED`, `LIMIT_EXCEEDS_POLICY`, `IDEMPOTENCY_CONFLICT`, `ENVIRONMENT_UNAVAILABLE`,
`UNSUPPORTED_PROTOCOL_VERSION`과 `INTERNAL_ERROR`는 v1 의미와 retryability를 유지한다. Profile validation과
Artifact preflight는 capacity admission보다 먼저 끝나야 한다.

## 멱등성, 보관과 cleanup

`clientRequestId` namespace는 v1 `submitTask`와 v2 `submitProfile`이 공유한다. 같은 id는 다음이 모두
동일한 canonical request일 때만 기존 Task의 현재 response를 반환한다.

- operation kind, Profile name/version
- 각 input slot name, `kind`, scalar value 또는 Artifact path/digest/size
- resource override의 모든 지정 field

JSON object key order는 identity가 아니며 string bytes, slot name case, integer value와 omitted field는
identity에 포함된다. 빈 override object는 금지하므로 omitted와 empty object를 같은 요청으로 해석하지 않는다.
같은 id를 Raw/Profile에 교차 사용하거나 위 값 중 하나라도 다르면 `IDEMPOTENCY_CONFLICT`다.

기존 mapping을 조회한 뒤 같은 request이면 source file을 다시 snapshot하거나 새 cgroup·process·published
Artifact를 만들지 않는다. 이 보장은 v1과 같이 daemon process와 completed snapshot retention 안에서만
유효하며, 완료 뒤 최소 10분을 보관한다. daemon restart 뒤 exactly-once와 Task resume은 보장하지 않는다.

## Java Core SDK mapping

Java Core SDK는 daemon implementation detail을 노출하지 않는 아래 public model을 제공한다.

```java
public record ProfileIdentity(String name, String version) {}

public record ProfileRequest(
    ProfileIdentity profile,
    Map<String, ProfileInputValue> inputs,
    ProfileResourceOverrides resourceOverrides) {}

public sealed interface ProfileInputValue
    permits StringProfileInput, Int64ProfileInput, BooleanProfileInput, LocalInputArtifact {}

public record StringProfileInput(String value) implements ProfileInputValue {}
public record Int64ProfileInput(long value) implements ProfileInputValue {}
public record BooleanProfileInput(boolean value) implements ProfileInputValue {}

public record LocalInputArtifact(ArtifactPath path, Sha256Digest digest, long sizeBytes)
    implements ProfileInputValue {}

public record PublishedArtifact(
    ArtifactPath path, Sha256Digest digest, long sizeBytes, String mediaType) {}
```

`ArtifactPath`은 root-relative wire string을 검증하는 value type이며 `java.nio.file.Path`가 아니다. absolute
path normalization, Artifact root selection과 output file name 지정은 Core SDK public API에 없다.

`ProfileResourceOverrides`는 `limits`와 `output`을 각각 optional으로 담는 immutable builder type이다. builder는
field가 생략된 상태를 보존하며, wire `resourceOverrides`와 똑같이 `cpuMax`, `memoryMaxBytes`, `pidsMax`,
`wallTimeLimitMs`, `stdoutTailMaxBytes`, `stderrTailMaxBytes`만 노출한다. SDK가 Profile default나 deployment
policy를 추측해 `safeDefaults()`를 넣지 않는다.

`ProfileTaskSubmission`은 running `ProfileTask` 또는 cleanup-confirmed
`FinishedProfileTaskSnapshot`의 sealed union이다. `ProfileTaskSnapshot`은 running/finished profile result의
공통 view이고, `FinishedProfileTaskSnapshot`은 `profileOutcome`, v1-compatible process/timing/usage/output,
`PublishedArtifact` map과 failure를 immutable으로 노출한다. `SUCCEEDED` snapshot의 Artifact map은 output slot
하나이고 `FAILED` snapshot의 map은 비어 있어야 한다.

`TaskCageClient`는 다음 default extension을 갖는다. default method는 기존 custom `TaskCageClient`
implementation의 source/binary compatibility를 보존하고, 기본 구현은 `UnsupportedOperationException`을
던진다. bundled `DefaultTaskCageClient`만 v2 capability가 있을 때 이를 override한다.

```java
ProfileTaskSubmission submitProfile(ProfileRequest request);
ProfileTaskSubmission submitProfile(UUID clientRequestId, ProfileRequest request);
ProfileTaskHandle submitProfileHandle(ProfileRequest request);
ProfileTaskHandle submitProfileHandle(UUID clientRequestId, ProfileRequest request);
FinishedProfileTaskSnapshot run(ProfileRequest request, Duration waitTimeout);
FinishedProfileTaskSnapshot run(UUID clientRequestId, ProfileRequest request, Duration waitTimeout);
ProfileTaskSnapshot getProfileResult(UUID taskId);
```

`ProfileTaskHandle.await(Duration)`은 `getProfileResult`를 polling하고 cleanup-confirmed
`FinishedProfileTaskSnapshot`을 반환한다. `ProfileTaskHandle.cancel()`은 기존 `cancelTask(UUID)`를 호출한다.
wait timeout은 Task를 자동 취소하지 않는다.

id 없는 convenience overload는 SDK가 새 UUID를 만들 수 있지만, connection failure·lost response·wait timeout
복구에는 caller가 `clientRequestId` overload를 사용해야 한다. v2 frame/validation 오류는
`TaskCageProtocolException` 계열이고, UDS connect/read/write 오류는 기존 `TaskCageConnectionException`이다.
capability에 `2`가 없을 때는 `TaskCageProtocolException`으로 보고하며 Raw Command API를 fallback으로
호출하지 않는다.

## Fixture corpus와 구현 gate

[`protocol-fixtures/v2/README.md`](../protocol-fixtures/v2/README.md)의 fixture는 이 문서의 결정적 예시다.
fixture digest는 JSON bytes의 실제 hash가 아니라 wire format을 고정하기 위한 가짜 값이다.

#150은 Rust decoder/handler와 Linux cgroup E2E에서, #153은 Java encoder/decoder와 real-daemon E2E에서
동일 fixture를 검증해야 한다. 문서와 fixture만 있는 현재 단계는 Protocol v2 daemon 또는 Java SDK가
구현됐다는 증거가 아니다.
