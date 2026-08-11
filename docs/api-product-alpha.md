# TaskCage v0.2 Local Product Alpha 계약

## 문서 상태와 범위

이 문서는 TaskCage v0.2 Local Product Alpha에서 새로 제공하는 Execution Profile, Local Artifact,
Runtime Package cache와 TaskCage Bundle 초안의 구현 계약이다. 기존 Raw Command 계약은
[Local Protocol v1 API 명세](api-mvp.md)가 계속 권위 문서다.

v0.2의 제품 범위는 다음 한 흐름을 실제로 제공하는 것이다.

```text
Profile Binding
  -> ProfileRequest
  -> 설치된 Bundle과 Runtime Package 검증
  -> 결정적인 실행 plan
  -> task cgroup 안에서 실행
  -> Artifact 원자적 공개
  -> ProfileResult
```

첫 번째이자 v0.2에서 유일하게 표준 제공하는 Execution Profile은
`org.taskcage.ffmpeg.transcode@1.0.0`이다. Java Binding은 이 Profile을 다음과 같은 domain API로
노출한다.

```java
FfmpegResult result = ffmpeg.transcode(
    TranscodeRequest.of(input, MP4)
);
```

이 편의 API도 최종적으로 이 문서의 범용 `ProfileRequest` 하나를 만든다. FFmpeg 전용 wire operation이나
FFmpeg 전용 daemon handler를 만들지 않는다.

## 버전과 호환성

### 전송과 framing

v0.2도 Local UDS만 사용한다. UDS 권한, 4-byte big-endian 길이 prefix, 1 MiB frame 상한, UTF-8 JSON,
중복 key 거부와 요청 순서 보장은 Protocol v1과 같다. 연결 단위 version negotiation은 없다. 각 frame의
`protocolVersion`과 `type` 쌍으로 operation을 판정하므로 같은 연결에서 v1과 v2 요청을 순서대로 보낼 수
있다.

v0.2 daemon의 v1 `getCapabilities` 응답은 다음을 포함한다.

```json
{
  "protocolVersion": 1,
  "requestId": "c2a091d5-2dd7-44aa-b48f-fd3dd82aa684",
  "type": "capabilities",
  "payload": {
    "daemonVersion": "0.2.0",
    "protocolVersions": [1, 2],
    "maxFrameBytes": 1048576,
    "maxConcurrentTasks": 4,
    "cgroupV2Ready": true
  }
}
```

`protocolVersions`의 순서는 오름차순으로 고정한다. Profile Binding은 `2`가 없으면 Profile 요청을 보내지
않고 SDK의 unsupported-protocol 오류를 반환한다.

daemon은 cgroup 실행 경계뿐 아니라 configured Artifact root, Bundle catalog와 참조된 Runtime Package를
모두 시작 시 검증한 뒤에만 `2`를 광고한다. Raw Command 환경만 준비됐거나 Product Alpha store가 설정되지
않았으면 `[1]`만 광고하며, v2 요청을 제한 없는 fallback으로 실행하지 않는다. 실행 중 검증된 Product
Alpha 환경이 손상되면 v2 신규 제출을 `ENVIRONMENT_UNAVAILABLE`로 닫고 fail-stop 정책을 적용한다.

### operation matrix

| `protocolVersion` | 요청 `type` | v0.2 동작 |
|---|---|---|
| `1` | `getCapabilities`, `submitTask`, `getTask`, `cancelTask` | Protocol v1과 byte·field·상태·오류 의미를 유지한다. |
| `1` | `submitProfile`, `getProfileResult` | v1의 기존 unknown operation 규칙대로 `INVALID_REQUEST`다. |
| `2` | `submitProfile`, `getProfileResult` | 이 문서의 Product Alpha 계약을 적용한다. |
| `2` | v1 Raw operation 또는 그 밖의 operation | `INVALID_REQUEST`다. |
| 그 밖의 정수 | 모든 operation | `UNSUPPORTED_PROTOCOL_VERSION`이다. |

Protocol v2는 v1을 교체한 전체 protocol이 아니라 Profile 기능만 담은 additive protocol이다. Profile로
생성한 `taskId`는 v1 `getTask`와 `cancelTask`에서도 사용할 수 있다. 반대로 Raw Command로 생성한
`taskId`를 `getProfileResult`로 조회하면 `TASK_KIND_MISMATCH`를 반환한다.

| client | v0.1 daemon `[1]` | v0.2 daemon `[1,2]` |
|---|---|---|
| v0.1 Raw SDK | 지원 | 기존 의미 그대로 지원 |
| v0.2 Raw SDK | 지원 | 지원 |
| v0.2 Profile SDK | capabilities 확인 뒤 미지원으로 중단 | 지원 |

기존 `protocol-fixtures/v1/` JSON과 Protocol v1 동작은 수정하지 않는다. Profile 계약 fixture는 별도
`protocol-fixtures/v2/`에 둔다.

## 공통 실행 불변 조건

Profile Task도 Raw Task와 같은 안전 불변 조건을 지킨다.

| 조건 | 계약 |
|---|---|
| 제한 우선 | task cgroup과 모든 제한을 적용하고 read-back한 뒤에만 target을 시작한다. |
| 원자적 진입 | target은 생성 시점부터 자신의 task cgroup에 들어간다. 이 조건을 만족하지 못하면 실행하지 않는다. |
| shell-free | executable과 argv를 분리하며 shell parsing, glob, command substitution과 PATH lookup을 사용하지 않는다. |
| whole-task cleanup | timeout·취소·오류 시 루트 PID가 아니라 Task의 프로세스 트리 전체를 정리한다. |
| 완료 의미 | 프로세스, task cgroup, output reader와 실패한 Artifact staging 정리를 확인한 뒤에만 `FINISHED`를 공개한다. |
| 실패 차단 | 안전한 실행이나 정리를 확인할 수 없으면 신규 Task를 받지 않고 필요하면 fail-stop한다. |

Profile은 이 제한을 우회하는 별도 실행 경로가 아니다. 최종 실행은 Protocol v1 Raw Task와 동일한 검증된
Runner와 lifecycle을 사용해야 한다.

## Protocol v2 공통 메시지

요청과 응답은 정수 `protocolVersion`, UUID `requestId`, 문자열 `type`, 객체 `payload`를 반드시 가진다.
요청의 알 수 없는 field는 `INVALID_REQUEST`로 거절하고 SDK는 응답의 알 수 없는 field를 무시한다.

오류 응답은 실행이 수락되기 전의 거절 또는 조회 오류에만 사용한다.

```json
{
  "protocolVersion": 2,
  "requestId": "66666666-6666-4666-8666-666666666666",
  "type": "error",
  "payload": {
    "code": "ARTIFACT_DIGEST_MISMATCH",
    "message": "artifacts.source does not match the declared digest and size",
    "retryable": false
  }
}
```

호출자는 `message`를 parsing하지 않고 `code`와 `retryable`로 분기한다. 수락된 Task의 process 또는 output
계약 실패는 top-level `error`로 바꾸지 않고 최종 `profileResult`로 보존한다.

## `ProfileRequest`

`submitProfile.payload`가 범용 `ProfileRequest`다.

```json
{
  "protocolVersion": 2,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "submitProfile",
  "payload": {
    "clientRequestId": "22222222-2222-4222-8222-222222222222",
    "profile": {
      "id": "org.taskcage.ffmpeg.transcode",
      "version": "1.0.0",
      "digest": "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199"
    },
    "bundleDigest": "sha256:e11581dc8be885c4fed87fb9705200d4b2390fe85be2ff8af4ac49e01346f477",
    "inputs": {
      "format": "MP4",
      "quality": 23,
      "stripMetadata": true
    },
    "artifacts": {
      "source": {
        "kind": "LOCAL_INPUT",
        "path": "jobs/42/source.mov",
        "digest": "sha256:4444444444444444444444444444444444444444444444444444444444444444",
        "sizeBytes": 1048576
      }
    },
    "resourceOverrides": {
      "limits": {
        "memoryMaxBytes": 1073741824,
        "wallTimeLimitMs": 300000
      },
      "output": {
        "stderrTailMaxBytes": 32768
      }
    }
  }
}
```

이 예시는 [`submit-profile-valid.json`](../protocol-fixtures/v2/submit-profile-valid.json)과 같다. 문서와
fixture의 Profile과 Bundle digest는 `product-fixtures/v1/` manifest의 canonical SHA-256과 일치한다.
실제 payload가 없는 input/output Artifact와 resolved plan digest만 wire 형식을 위한 결정적인 값이다.

### 공통 field

- `clientRequestId`는 UUID이며 daemon 생존 기간의 멱등 제출 key다.
- `profile.id`와 `profile.version`은 Bundle 안의 Profile과 정확히 같아야 한다.
- `profile.digest`는 Bundle 안의 Profile object를 canonicalize해 계산한 `profileDigest`와 정확히 같아야
  한다.
- payload top-level `bundleDigest`는 설치된 Bundle을 고정한다. `profile.digest`와 `bundleDigest`는 모두
  `sha256:` 뒤에 소문자 16진수 64자가 오는 형식이며 둘 중 하나라도 생략할 수 없다.
- alias나 `latest`는 허용하지 않는다.
- `inputs`와 `artifacts`의 key는 Profile이 선언한 slot 이름과 정확히 일치해야 한다. 누락, 추가 field,
  대소문자 차이는 `INVALID_PROFILE_INPUT`이다.
- 이름은 ASCII 소문자로 시작하고 이후 ASCII 영문자·숫자·`_`·`-`를 허용하는
  `[a-z][A-Za-z0-9_-]{0,63}` 형식이다.
- canonical Product Alpha manifest와 request에서 정수는 소수점·지수 표기 없이
  `-9007199254740991`~`9007199254740991`의 exact I-JSON 범위여야 한다. 이 범위는 Rust와 Java가
  RFC 8785 bytes와 digest를 반올림 없이 동일하게 계산하기 위한 v0.2 제한이다.

### scalar input

`inputs`는 wrapper 없는 JSON scalar map이다. type은 Profile의 같은 이름 input schema가 결정하며 v0.2는
다음 세 scalar만 지원한다.

| Profile `type` | 요청 JSON 값 | 규칙 |
|---|---|---|
| `string` | string | UTF-8, NUL 없음, 최대 4,096 bytes. Profile의 `enum`, `minLength`, `maxLength`를 적용한다. |
| `integer` | number | exact I-JSON 정수 범위이며 Profile의 `minimum`, `maximum`을 적용한다. |
| `boolean` | boolean | JSON `true` 또는 `false`만 허용한다. |

요청에 `{ "type": "string", "value": "MP4" }` 같은 typed wrapper를 넣지 않는다. `null`, floating
point, array, object와 암시적 type coercion은 허용하지 않는다. JSON object key 순서는 의미가 없다.
scalar 문자열은 shell fragment가 아니며 하나의 argv token으로만 해석할 수 있다.

### Artifact binding

- `artifacts`에는 Profile이 선언한 input slot만 오며 각 값은 `kind: "LOCAL_INPUT"`, `path`,
  `sizeBytes`, `digest`를 모두 가진다.
- `path`는 daemon에 하나만 설정된 Local Artifact root 기준 상대 경로다. wire에서 root 이름이나 절대
  경로를 선택할 수 없다.
- output root, path와 file name은 caller가 지정하지 않는다. Profile이 정확히 하나의 required output
  slot과 고정 file name을 선언하고 daemon이 task별 staging과 published relative path를 만든다.
- `sizeBytes`는 0 이상의 JSON 정수이며 Profile slot 상한 이하다.
- input `digest`는 실제 staged bytes를 검증한다.
- v0.2 Bundle은 하나 이상의 input slot과 **정확히 하나의 required output slot**을 가진다. 여러
  output을 하나의 transaction으로 공개하는 기능은 후속 계약이다.

### resource override

`resourceOverrides`는 선택 field다. 내부의 `limits`와 `output`도 각각 선택이며 다음 field의 부분 집합만
허용한다.

```json
{
  "limits": {
    "cpuMax": { "quotaMicros": 100000, "periodMicros": 100000 },
    "memoryMaxBytes": 1073741824,
    "pidsMax": 32,
    "wallTimeLimitMs": 300000
  },
  "output": {
    "stdoutTailMaxBytes": 32768,
    "stderrTailMaxBytes": 32768
  }
}
```

- 지정하지 않은 값은 Profile `resourcePolicy.defaults.limits`와 `defaults.output`을 사용한다.
- `cpuMax`를 지정하면 quota와 period를 함께 지정해야 한다.
- 모든 값은 양의 정수이며 Profile `maxOverrides`의 같은 field와 daemon 배포 정책을 모두 넘지 않아야
  한다.
- CPU는 `quotaMicros / periodMicros` 비율을 정수 교차 곱으로 비교한다.
- stdout/stderr tail은 각각 1~65,536 bytes이고 합계는 131,072 bytes 이하다.
- 최종 cgroup limit은 read-back 결과와 같아야 한다. 다르면 target을 시작하지 않고 `INTERNAL_ERROR`다.
- 최종 output tail 상한도 resolved plan과 `profileAccepted.effectiveResources`에 기록한다.

Profile 기본값 자체가 daemon 배포 정책을 넘으면 해당 제출은 `LIMIT_EXCEEDS_POLICY`다. 제한 없는 fallback은
없다.

## operation

### `submitProfile`

검증과 실행 준비가 끝나고 target이 제한된 task cgroup 안에서 시작되면 다음을 반환한다.

```json
{
  "protocolVersion": 2,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "profileAccepted",
  "payload": {
    "taskId": "44444444-4444-4444-8444-444444444444",
    "state": "RUNNING",
    "profile": {
      "id": "org.taskcage.ffmpeg.transcode",
      "version": "1.0.0",
      "digest": "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199"
    },
    "bundleDigest": "sha256:e11581dc8be885c4fed87fb9705200d4b2390fe85be2ff8af4ac49e01346f477",
    "runtimePackage": {
      "id": "org.taskcage.ffmpeg",
      "version": "7.1.1-taskcage.1",
      "digest": "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
    },
    "resolvedPlanDigest": "sha256:6666666666666666666666666666666666666666666666666666666666666666",
    "effectiveResources": {
      "limits": {
        "cpuMax": { "quotaMicros": 100000, "periodMicros": 100000 },
        "memoryMaxBytes": 1073741824,
        "pidsMax": 32,
        "wallTimeLimitMs": 300000
      },
      "output": {
        "stdoutTailMaxBytes": 65536,
        "stderrTailMaxBytes": 32768
      }
    }
  }
}
```

`profileAccepted`는 준비 접수가 아니라 target 시작 확인이다. `execve`가 시작되지 못하면 cleanup을 완료한
`profileResult`/`FINISHED`를 `submitProfile`의 직접 응답으로 반환한다.

### `getProfileResult`

```json
{
  "protocolVersion": 2,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "getProfileResult",
  "payload": {
    "taskId": "44444444-4444-4444-8444-444444444444"
  }
}
```

실행 중 응답:

```json
{
  "protocolVersion": 2,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "profileResult",
  "payload": {
    "taskId": "44444444-4444-4444-8444-444444444444",
    "state": "RUNNING",
    "profile": {
      "id": "org.taskcage.ffmpeg.transcode",
      "version": "1.0.0",
      "digest": "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199"
    },
    "bundleDigest": "sha256:e11581dc8be885c4fed87fb9705200d4b2390fe85be2ff8af4ac49e01346f477",
    "runtimePackage": {
      "id": "org.taskcage.ffmpeg",
      "version": "7.1.1-taskcage.1",
      "digest": "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
    },
    "resolvedPlanDigest": "sha256:6666666666666666666666666666666666666666666666666666666666666666",
    "submittedAt": "2026-08-11T12:00:00Z",
    "startedAt": "2026-08-11T12:00:01Z"
  }
}
```

성공 응답:

```json
{
  "protocolVersion": 2,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "profileResult",
  "payload": {
    "taskId": "44444444-4444-4444-8444-444444444444",
    "state": "FINISHED",
    "profileOutcome": "SUCCEEDED",
    "profile": {
      "id": "org.taskcage.ffmpeg.transcode",
      "version": "1.0.0",
      "digest": "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199"
    },
    "bundleDigest": "sha256:e11581dc8be885c4fed87fb9705200d4b2390fe85be2ff8af4ac49e01346f477",
    "runtimePackage": {
      "id": "org.taskcage.ffmpeg",
      "version": "7.1.1-taskcage.1",
      "digest": "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
    },
    "resolvedPlanDigest": "sha256:6666666666666666666666666666666666666666666666666666666666666666",
    "terminationReason": "EXITED",
    "process": { "exitCode": 0, "signal": null },
    "timing": {
      "submittedAt": "2026-08-11T12:00:00Z",
      "startedAt": "2026-08-11T12:00:01Z",
      "finishedAt": "2026-08-11T12:00:11Z",
      "wallTimeMs": 10000
    },
    "usage": {
      "cpuTimeMicros": 4800000,
      "memoryPeakBytes": 67108864
    },
    "output": {
      "stdoutTail": "",
      "stderrTail": "",
      "stdoutTruncated": false,
      "stderrTruncated": false
    },
    "artifacts": {
      "result": {
        "kind": "LOCAL_FILE",
        "path": "tasks/44444444-4444-4444-8444-444444444444/result.mp4",
        "digest": "sha256:5555555555555555555555555555555555555555555555555555555555555555",
        "sizeBytes": 7340032,
        "mediaType": "video/mp4"
      }
    }
  }
}
```

실패한 완료 응답은 process와 Task 근거를 그대로 보존하고 공개 Artifact를 반환하지 않는다.

```json
{
  "protocolVersion": 2,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "profileResult",
  "payload": {
    "taskId": "44444444-4444-4444-8444-444444444444",
    "state": "FINISHED",
    "profile": {
      "id": "org.taskcage.ffmpeg.transcode",
      "version": "1.0.0",
      "digest": "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199"
    },
    "bundleDigest": "sha256:e11581dc8be885c4fed87fb9705200d4b2390fe85be2ff8af4ac49e01346f477",
    "runtimePackage": {
      "id": "org.taskcage.ffmpeg",
      "version": "7.1.1-taskcage.1",
      "digest": "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
    },
    "resolvedPlanDigest": "sha256:6666666666666666666666666666666666666666666666666666666666666666",
    "profileOutcome": "FAILED",
    "terminationReason": "EXITED",
    "process": { "exitCode": 1, "signal": null },
    "timing": {
      "submittedAt": "2026-08-11T12:00:00Z",
      "startedAt": "2026-08-11T12:00:01Z",
      "finishedAt": "2026-08-11T12:00:02Z",
      "wallTimeMs": 1000
    },
    "usage": {
      "cpuTimeMicros": 120000,
      "memoryPeakBytes": 8388608
    },
    "output": {
      "stdoutTail": "",
      "stderrTail": "invalid input",
      "stdoutTruncated": false,
      "stderrTruncated": false
    },
    "artifacts": {},
    "failure": {
      "code": "PROCESS_EXITED_NONZERO",
      "message": "profile process exited with code 1"
    }
  }
}
```

`terminationReason`, `process`, `timing`, `usage`, `output`의 의미와 enum은 Protocol v1과 같다.

`profileOutcome`은 다음 규칙으로 확정한다.

| outcome | 조건 |
|---|---|
| `SUCCEEDED` | `terminationReason=EXITED`, `exitCode=0`, required output 검증과 원자적 공개가 모두 끝남 |
| `FAILED` | 그 밖의 모든 최종 상태 |

실패 `failure.code`는 `PROCESS_EXITED_NONZERO`, `TASK_TERMINATED`, `OUTPUT_CONTRACT_VIOLATION`,
`OUTPUT_PUBLISH_FAILED` 중 하나다. target이 종료된 원인은 항상 `terminationReason`을 함께 확인한다.
출력이 없거나 type·크기 계약을 어기면 `OUTPUT_CONTRACT_VIOLATION`이다. publish I/O가 실패하면
`OUTPUT_PUBLISH_FAILED`다.

Artifact staging cleanup을 확인할 수 없으면 근거 없는 `FINISHED`를 만들지 않고 기존 fail-stop 계약을
적용한다.

## 멱등성과 보관

- `clientRequestId` namespace는 `submitTask`와 `submitProfile`이 공유한다.
- 같은 `clientRequestId`와 같은 canonical ProfileRequest는 staging이나 새 실행 없이 기존 Task의 현재
  응답을 반환한다.
- 같은 ID를 Raw와 Profile에 교차 사용하거나 canonical request가 다르면 `IDEMPOTENCY_CONFLICT`다.
- canonical request에는 Profile id/version/digest와 Bundle digest, scalar input, input Artifact descriptor, resource
  override가 모두 포함된다. JSON key 순서는 포함하지 않는다.
- input 파일의 현재 존재 여부나 성공 output의 현재 존재 여부를 재검사하기 전에 기존 멱등 mapping을
  조회한다. 따라서 성공 응답을 잃은 호출자가 같은 요청을 재시도해도 새 실행이나
  새 published Artifact가 발생하지 않는다.
- 보장은 Protocol v1과 같이 같은 daemon process와 메모리 Registry 보관 기간 안에서만 유효하다.
- 완료 Profile snapshot과 mapping도 완료 시점부터 최소 10분 보관한다. daemon 재시작 뒤 exactly-once나
  Task 재개는 보장하지 않는다.

## Execution Profile과 결정적 plan

Execution Profile은 Bundle 안에 포함되며 요청자가 program, raw argv, working directory 또는 environment를
직접 지정하지 못하게 한다. v0.2 Profile schema의 핵심 field는 다음과 같다.

```json
{
  "schemaVersion": "taskcage.execution-profile/v0alpha1",
  "id": "org.taskcage.ffmpeg.transcode",
  "version": "1.0.0",
  "entrypoint": "bin/ffmpeg",
  "inputSchema": {
    "scalars": {
      "format": {
        "type": "string",
        "required": true,
        "enum": ["MP4"]
      },
      "quality": {
        "type": "integer",
        "required": true,
        "enum": [18, 23, 28]
      },
      "stripMetadata": {
        "type": "boolean",
        "required": true
      }
    },
    "artifacts": {
      "source": {
        "kind": "LOCAL_INPUT",
        "required": true,
        "mediaTypes": ["video/quicktime", "video/mp4", "video/x-matroska"],
        "maxSizeBytes": 2147483648
      }
    },
    "additionalProperties": false
  },
  "outputSchema": {
    "artifacts": {
      "result": {
        "kind": "LOCAL_FILE",
        "required": true,
        "mediaType": "video/mp4",
        "maxSizeBytes": 2147483648,
        "fileName": "result.mp4",
        "publication": "TASK_SCOPED"
      }
    },
    "additionalProperties": false
  },
  "argv": [
    { "kind": "literal", "value": "-hide_banner" },
    { "kind": "literal", "value": "-loglevel" },
    { "kind": "literal", "value": "error" },
    { "kind": "literal", "value": "-nostdin" },
    { "kind": "literal", "value": "-n" },
    { "kind": "literal", "value": "-i" },
    { "kind": "artifact", "slot": "source" },
    { "kind": "literal", "value": "-map" },
    { "kind": "literal", "value": "0:v:0?" },
    { "kind": "literal", "value": "-map" },
    { "kind": "literal", "value": "0:a:0?" },
    { "kind": "literal", "value": "-c:v" },
    { "kind": "literal", "value": "libx264" },
    { "kind": "literal", "value": "-crf" },
    {
      "kind": "choice",
      "input": "quality",
      "cases": [
        { "equals": 18, "value": "18" },
        { "equals": 23, "value": "23" },
        { "equals": 28, "value": "28" }
      ]
    },
    { "kind": "literal", "value": "-c:a" },
    { "kind": "literal", "value": "aac" },
    { "kind": "literal", "value": "-map_metadata" },
    {
      "kind": "choice",
      "input": "stripMetadata",
      "cases": [
        { "equals": true, "value": "-1" },
        { "equals": false, "value": "0" }
      ]
    },
    { "kind": "literal", "value": "-movflags" },
    { "kind": "literal", "value": "+faststart" },
    { "kind": "literal", "value": "-f" },
    {
      "kind": "choice",
      "input": "format",
      "cases": [
        { "equals": "MP4", "value": "mp4" }
      ]
    },
    { "kind": "artifact", "slot": "result" }
  ],
  "environment": {
    "LANG": "C",
    "LC_ALL": "C"
  },
  "resourcePolicy": {
    "defaults": {
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
    },
    "maxOverrides": {
      "limits": {
        "cpuMax": { "quotaMicros": 200000, "periodMicros": 100000 },
        "memoryMaxBytes": 2147483648,
        "pidsMax": 128,
        "wallTimeLimitMs": 900000
      },
      "output": {
        "stdoutTailMaxBytes": 65536,
        "stderrTailMaxBytes": 65536
      }
    }
  }
}
```

v0.2 manifest JSON은 최대 1 MiB다. identity는 최대 255 ASCII bytes, 선언한 input slot 합계는 최대 64개,
scalar enum과 `choice.cases`는 각각 최대 256개, Artifact `mediaTypes`는 최대 64개다. Profile environment는
최대 64개이며 key/value UTF-8 bytes 합계가 65,536 이하여야 한다. Runtime Package는 file 4,096개,
`libraryPaths` 64개, license metadata 256개를 상한으로 한다.

Profile schema는 `additionalProperties: false` 의미로 검증한다. Profile JSON 자기 안에는 자신의 digest를
넣지 않는다. importer는 Profile object 전체를 RFC 8785로 canonicalize한 bytes에서 `profileDigest`를
계산하고 Bundle catalog와 request/result identity에서 보유한다. `entrypoint`는 Runtime Package root 기준
canonical relative path이며 executable regular file이어야 한다.

Profile `environment`는 daemon이나 caller의 환경을 상속하지 않는 고정 UTF-8 map이다. `PATH`와 `LD_`로
시작하는 key는 Profile에서 금지한다. package executable과 loader search path는 daemon이 검증된 Runtime
Package handle로 구성하며 host PATH, `LD_PRELOAD`, `LD_AUDIT`, `LD_LIBRARY_PATH` fallback을 허용하지 않는다.

output slot은 정확히 하나이고 `required: true`여야 한다. `fileName`은 slash가 없는 canonical path segment
하나이며 caller input으로 대체할 수 없다. daemon은 이 값과 `taskId`로 published relative path를 만든다.
v0.2에서 지원하는 `publication` 값은 `TASK_SCOPED` 하나이며 결과 path는
`tasks/<taskId>/<fileName>`이다.
input `mediaTypes`는 Binding이 지원 입력을 설명하는 Profile metadata다. v0.2 wire에는 caller가 주장하는
media type field가 없으므로 daemon은 file extension만으로 type을 신뢰하지 않는다. daemon의 pre-execution
Artifact 검증은 path·regular-file·size·digest에 한정하며 실제 container/codec 부적합은 FFmpeg process
결과로 나타난다.

v0.2 argv template token은 다음 세 종류만 지원하며 각 token은 정확히 하나의 argv element를 만든다.

| `kind` | 규칙 |
|---|---|
| `literal` | Bundle에 고정한 NUL 없는 UTF-8 문자열 |
| `choice` | 선언한 input 값과 `cases[].equals`를 JSON type까지 exact match해 Bundle의 고정 `value` 하나를 선택 |
| `artifact` | input은 `artifacts/in/<slot>`, output은 daemon staging의 `artifacts/out/<slot>.part` 상대 경로 |

문자열 결합, 조건부 token 생략, 반복, shell escape와 nested template은 v0.2에 없다. argv는 최대 256개,
token 하나는 최대 4,096 bytes, 전체는 최대 65,536 bytes다. Profile import 시 모든 선언과 참조가 닫혀
있는지 검증한다.

daemon은 검증된 요청으로 다음 logical resolved plan을 만든다.

- Bundle digest, Profile digest와 Runtime Package digest
- `package:<Runtime Package digest>/<entrypoint>` 형식의 logical executable identity
- 위 규칙으로 순서가 확정된 argv
- logical working directory `TASK_ROOT`
- key의 byte 순으로 정렬한 고정 environment
- staged Artifact의 상대 경로, input digest와 size
- 완전히 채워진 effective limits와 output tail 상한

실행 직전 daemon은 이 identity를 이미 고정한 cache directory handle 아래의 실제 absolute executable로
resolve한다. host별 cache base path는 logical plan digest에 넣지 않는다. target의 실제 working directory는
단일 Artifact root의 daemon 전용 staging subtree에 있는 Task directory지만 argv에는 위의 안정적인 상대
Artifact 경로만 사용한다. `resolvedPlanDigest`는 logical plan을 RFC 8785 JSON Canonicalization Scheme으로
canonicalize한 UTF-8 bytes의 SHA-256이다. 같은 Bundle digest와 canonical ProfileRequest는 같은
`resolvedPlanDigest`를 만들어야 한다.

## Local Artifact 계약

### 단일 허용 root

daemon 배포 설정은 Local Artifact root를 정확히 하나의 canonical absolute directory와 maximum bytes로
명시한다. 설정 파일 형식은 배포 계층의 책임이지만 다음 의미는 고정한다.

- daemon은 시작할 때 root를 canonicalize하고 directory file descriptor를 보관한다.
- root는 symlink가 아닌 기존 절대 directory이며 daemon이 읽기, staging, publish와 cleanup을 수행할 수
  있어야 한다.
- daemon은 root 안의 `.taskcage/`를 staging 전용으로, `tasks/`를 published output 전용으로 소유한다.
  caller가 제출하는 input `path`는 `.taskcage/` staging subtree를 가리킬 수 없다. 성공 결과로 받은
  `tasks/<taskId>/...` path는 후속 Task의 input으로 다시 사용할 수 있다.
- slot 상한, Artifact root 상한과 daemon 전역 상한 중 가장 작은 값을 적용한다.
- wire에 root selector는 없다. 허용 root 밖의 임의 절대 경로, `file:` URI와 network URI는 받지 않는다.

### descriptor-relative path

`path`는 `/` separator를 쓰는 UTF-8 relative path다. 다음 조건을 모두 만족해야 한다.

- 1~4,096 UTF-8 bytes이며 leading/trailing `/`가 없다.
- segment는 비어 있지 않고 `.` 또는 `..`가 아니며 NUL, `\\`, ASCII control character를 포함하지 않는다.
- percent decoding, Unicode normalization과 case folding을 하지 않는다. wire bytes 그대로 하나의 이름이다.
- caller input의 첫 segment는 reserved `.taskcage`가 아니어야 한다.
- input의 모든 component는 symlink가 아니어야 한다.
- Linux에서는 root fd 기준 `openat2`의 `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS |
  RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`와 `O_NOFOLLOW`에 해당하는 fail-closed resolution을 사용한다.
  같은 보장을 만들 수 없으면 Product Alpha Artifact 기능을 ready로 광고하지 않는다.
- input은 regular file이어야 한다. Artifact root 아래 mount crossing은 허용하지 않는다.

### staging과 input snapshot

input Artifact는 caller가 소유하며 daemon은 수정하거나 제거하지 않는다. 새 제출은 공개 Task record,
Registry reservation, task cgroup 또는 target을 만들기 전에 input을
`.taskcage/preflight/<requestId>.<nonce>/`로 복사하면서 size와 SHA-256을 검증한다. staged snapshot은
`0400` regular file이고 실행 plan에서는 `artifacts/in/<slot>`으로 보인다.

복사 중 source가 바뀌어 선언한 size나 digest와 달라지면 `ARTIFACT_DIGEST_MISMATCH`로 거절하고 preflight
copy를 제거한다. 성공한 snapshot만 Task 준비 단계의
`.taskcage/staging/<taskId>/task/artifacts/in/`으로 원자적으로 넘긴다.

output은 같은 Task staging directory의 `artifacts/out/<slot>.part`로 작성한다. caller는 output path를
지정하지 않으며 target은 caller input 원본이나 published path를 직접 받지 않는다. daemon이 생성하는
최종 path는 다음과 같다.

```text
tasks/<taskId>/<Profile output fileName>
```

v0.2에서 한 Task의 output은 하나뿐이므로 이 경로가 Task의 단일 publish identity다. UUID `taskId`와
Profile에 고정된 file name을 사용하며 alias나 caller 문자열을 섞지 않는다.

### 성공 공개와 실패 정리

`terminationReason=EXITED`와 `exitCode=0`일 때만 required output을 검증한다.

1. output이 symlink가 아닌 regular file인지 확인한다.
2. Profile, Artifact root와 daemon의 size 상한을 확인한다.
3. SHA-256과 정확한 `sizeBytes`를 계산한다.
4. file 내용을 `fsync`한다.
5. Linux `renameat2(RENAME_NOREPLACE)`와 동등한 no-overwrite rename으로 최종 path에 공개한다.
6. final parent directory를 `fsync`한 뒤에만 성공 `profileResult`를 저장한다.

daemon은 `tasks/<taskId>/`를 충돌 없이 만들고 staging file을
`renameat2(RENAME_NOREPLACE)`로 옮긴다. generated destination이 이미 있으면 덮어쓰거나 다른 이름을
성공으로 반환하지 않고 `OUTPUT_PUBLISH_FAILED`로 끝낸다. no-overwrite atomic rename을 지원하지 않는
filesystem에서는 Artifact root를 사용할 수 없다.

성공 응답의 `PublishedArtifact`는 다음 shape다.

```json
{
  "kind": "LOCAL_FILE",
  "path": "tasks/44444444-4444-4444-8444-444444444444/result.mp4",
  "digest": "sha256:5555555555555555555555555555555555555555555555555555555555555555",
  "sizeBytes": 7340032,
  "mediaType": "video/mp4"
}
```

`path`는 동일한 configured Artifact root 기준 상대 경로다. Profile의 output slot 이름이
`profileResult.artifacts` key가 되고 `mediaType`은 Profile 고정값이다.

성공한 output은 호출자 소유의 persistent Artifact다. 여기서 소유는 lifecycle 책임을 뜻하며 Unix
UID/GID는 daemon 배포 credential을 따른다. daemon은 Registry snapshot 만료, 재시작 또는 cache 정리 때
성공 Artifact를 삭제하지 않는다. 호출자가 명시적으로 삭제할 때까지 남는다.

process non-zero exit, timeout, 취소, exec 실패, output 계약 위반과 publish 실패에서는 최종 path를 만들지
않고 모든 staging input/output과 Task directory를 제거한다. 정리 완료를 확인하기 전에는 `FINISHED`를
공개하지 않는다. 기존 destination과 caller input은 어떤 실패 경로에서도 변경하지 않는다.

## Runtime Package local import와 cache

Runtime Package는 Bundle과 별도로 import하고 digest 기준으로 공유하는 실제 실행물이다. v0.2에는 Package
wire operation이 없다. 배포 관리자만 daemon host의 local path에서 administrative import를 수행한다.
URL, Hub 조회와 실행 중 download는 허용하지 않는다.

import directory 형식:

```text
runtime-package.json
rootfs/
  bin/ffmpeg
  lib/...
  share/...
```

manifest 초안:

```json
{
  "schemaVersion": "taskcage.runtime-package/v0alpha1",
  "id": "org.taskcage.ffmpeg",
  "version": "7.1.1-taskcage.1",
  "platform": {
    "os": "linux",
    "architecture": "x86_64",
    "abi": "gnu",
    "libc": {
      "family": "glibc",
      "minimumVersion": "2.39"
    }
  },
  "entrypoint": "bin/ffmpeg",
  "libraryPaths": ["lib"],
  "files": [
    {
      "path": "bin/ffmpeg",
      "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "sizeBytes": 82739200,
      "mode": "0555"
    },
    {
      "path": "lib/libavcodec.so.61",
      "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "sizeBytes": 15123456,
      "mode": "0444"
    },
    {
      "path": "lib/libavformat.so.61",
      "digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "sizeBytes": 3817472,
      "mode": "0444"
    },
    {
      "path": "lib/libavutil.so.59",
      "digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "sizeBytes": 1048576,
      "mode": "0444"
    },
    {
      "path": "lib/libswresample.so.5",
      "digest": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
      "sizeBytes": 442368,
      "mode": "0444"
    },
    {
      "path": "lib/libswscale.so.8",
      "digest": "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
      "sizeBytes": 737280,
      "mode": "0444"
    },
    {
      "path": "share/licenses/ffmpeg/COPYING.GPLv2",
      "digest": "sha256:7777777777777777777777777777777777777777777777777777777777777777",
      "sizeBytes": 18092,
      "mode": "0444"
    },
    {
      "path": "share/sbom.spdx.json",
      "digest": "sha256:8888888888888888888888888888888888888888888888888888888888888888",
      "sizeBytes": 16384,
      "mode": "0444"
    }
  ],
  "licenses": [
    {
      "spdxId": "GPL-2.0-or-later",
      "path": "share/licenses/ffmpeg/COPYING.GPLv2"
    }
  ],
  "sbom": {
    "format": "SPDX-JSON-2.3",
    "path": "share/sbom.spdx.json"
  }
}
```

- manifest는 unknown field를 거부하고 file path는 Artifact와 같은 canonical relative path 규칙을 쓴다.
- file 목록은 path byte 순으로 정렬하며 모든 payload regular file을 정확히 한 번 포함한다. unlisted file,
  symlink, hardlink, device, socket와 FIFO는 거부한다.
- mode는 `0444` 또는 `0555`만 허용한다. 적어도 Profile entrypoint는 `0555`다.
- `licenses`와 `sbom`은 metadata 계약이며 특정 license를 승인한다는 뜻이 아니다.
- Runtime Package JSON은 자신의 digest field를 갖지 않는다. importer는 manifest 전체를 RFC 8785로
  canonicalize한 bytes에서 Package digest를 계산한다. 각 file digest가 manifest에 들어가므로 이
  digest가 전체 file set을 고정한다.
- importer는 manifest와 모든 file size/digest/platform을 검증하고 daemon 소유 staging으로 copy한 뒤
  cache에 원자적으로 활성화한다.

Runtime Package는 실제 `rootfs/bin/ffmpeg` executable과 실행에 필요한 FFmpeg library를 cache해야 한다.
host `/usr/bin/ffmpeg`를 호출하는 wrapper, PATH lookup, host package manager fallback과 missing library의
동적 download는 허용하지 않는다. `libraryPaths`는 Package root 기준 canonical relative directory이며
daemon은 이 경로만으로 loader search 환경을 결정한다. platform이 허용한 system ABI 외의 dependency가
cache 밖에서 해석되면 import 또는 compatibility gate가 실패한다.

기본 cache layout은 의미를 설명하기 위한 논리 경로이며 배포 설정으로 base directory만 바꿀 수 있다.

```text
/var/lib/taskcage/packages/sha256/<64-lowercase-hex>/
  runtime-package.json
  rootfs/...
```

cache entry는 target과 UDS caller가 쓸 수 없는 immutable directory다. 같은 digest를 다시 import할 때
기존 entry 전체가 검증되면 멱등 성공하고, 하나라도 다르면 덮어쓰지 않고
`PACKAGE_INTEGRITY_FAILED`로 실패한다. Package id/version은 사람이 읽는 identity이고 실행 identity는
Bundle이 고정한 digest다.
여러 Bundle이 같은 Package digest를 참조하면 cache entry 하나를 공유한다.

submit 시 Package가 없으면 `PACKAGE_NOT_FOUND`, manifest·entrypoint·metadata가 import 상태와 다르면
`PACKAGE_INTEGRITY_FAILED`다. Package 검증이 끝나기 전에 Task, task cgroup 또는 target을 만들지 않는다.
v0.2는 cache eviction을 제공하지 않는다.

## TaskCage Bundle 형식 초안

Bundle은 Profile과 고정된 Runtime Package 참조를 담는 작은 불변 JSON 계약이다. Package bytes를 포함하지
않는다.

```json
{
  "schemaVersion": "taskcage.bundle/v0alpha1",
  "id": "org.taskcage.ffmpeg.transcode",
  "version": "1.0.0",
  "profile": {
    "schemaVersion": "taskcage.execution-profile/v0alpha1",
    "id": "org.taskcage.ffmpeg.transcode",
    "version": "1.0.0",
    "entrypoint": "bin/ffmpeg",
    "inputSchema": {},
    "outputSchema": {},
    "argv": [],
    "environment": {},
    "resourcePolicy": {}
  },
  "runtimePackage": {
    "id": "org.taskcage.ffmpeg",
    "version": "7.1.1-taskcage.1",
    "digest": "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
  },
  "platform": {
    "os": "linux",
    "architecture": "x86_64",
    "abi": "gnu"
  },
  "policy": {
    "resourcePolicySource": "PROFILE",
    "artifactInputs": ["LOCAL_INPUT"],
    "artifactOutputs": ["LOCAL_FILE"],
    "outputPublication": "PROFILE_DECLARED",
    "overwritePublishedArtifacts": false
  },
  "integrity": {
    "algorithm": "SHA-256",
    "profileDigest": "sha256:01d667dade05be47cbd6fc285aa4e13acde1961a2516b82b6b72c35591890199",
    "runtimePackageDigest": "sha256:49c3a4b8e209375766448c957f06740fae824c12f002eda5f69e700d9e4425c6"
  }
}
```

위 축약 JSON은 top-level shape를 보여주기 때문에 그대로 import할 수 없다. 규범적
[`ffmpeg-transcode-bundle.json`](../product-fixtures/v1/ffmpeg-transcode-bundle.json)은 `profile`에
[`Execution Profile과 결정적 plan`](#execution-profile과-결정적-plan)의 전체 선언을 포함한다.

Bundle 규칙:

- `schemaVersion`은 v0.2에서 정확히 `taskcage.bundle/v0alpha1`이다.
- top-level `id`/`version`은 Profile id/version과 같다. version은 SemVer canonical string이다.
- `runtimePackage.id`/`version`은 import된 Package manifest와 같고 digest는 importer가 계산한 Package
  digest와 정확히 같아야 한다.
- `platform.os`는 `linux`, architecture는 v0.2 build가 명시적으로 지원하는 값이어야 하며 daemon host와
  일치해야 한다.
- `integrity.profileDigest`는 embedded Profile object의 canonical digest,
  `integrity.runtimePackageDigest`는 `runtimePackage.digest`와 같아야 한다.
- Bundle JSON은 자신의 digest field를 갖지 않는다. importer는 Bundle 전체를 RFC 8785로 canonicalize한
  bytes에서 Bundle digest를 계산하고 catalog path와 요청·결과에서 보유한다.
- Bundle은 unknown field를 거부한다. 서명 field는 v0.2 schema에 없으며 digest는 무결성을 제공하지만
  publisher provenance를 증명하지 않는다.
- Bundle은 Package archive, executable, library, font 또는 codec bytes를 포함할 수 없다.

Bundle도 local administrative import만 제공한다. importer는 Bundle schema, Profile 참조 폐쇄성,
resource policy, platform, Bundle digest와 이미 import된 Package digest를 모두 검증한 뒤 다음 논리 경로에
원자적으로 저장한다.

```text
/var/lib/taskcage/bundles/sha256/<64-lowercase-hex>.json
```

같은 digest import는 멱등이고 다른 bytes로 기존 entry를 덮어쓸 수 없다. Hub, registry 조회, network fetch,
자동 update와 `latest` alias는 없다.

## 제출 검증과 side-effect 순서

새 `submitProfile`은 다음 순서를 지켜야 한다.

1. frame, JSON, protocolVersion, operation, UUID와 모든 field shape를 검증한다.
2. Bundle digest로 local Bundle을 찾고 Bundle, Profile, Package, platform과 cache 무결성을 read-only로
   검증한다.
3. scalar input, input Artifact descriptor, resource override를 검증하고 canonical request와 logical resolved
   plan을 만든다.
4. 기존 idempotency mapping을 read-only로 확인한다. 같은 요청이면 기존 응답, 다른 요청이면 conflict를
   반환한다.
5. 새 요청이면 단일 allowed root에서 실제 input을 열어 size/digest를 검증하며 daemon preflight
   directory에 snapshot을 만든다.
6. 동일한 canonical request를 원자적으로 재확인한 뒤에만 Registry와 실행 capacity를 예약한다. 경합에서
   진 duplicate는 preflight copy를 제거하고 기존 응답을 사용한다.
7. Task directory와 daemon-generated output staging/published identity를 준비하고 모든 준비 상태를
   read-back한다.
8. task cgroup을 만들고 resource limit을 적용·read-back한다.
9. 결정된 executable과 argv로 target을 원자적으로 task cgroup에 넣어 시작한다.
10. `profileAccepted` 또는 cleanup을 마친 즉시 `profileResult`를 공개한다.

1~5단계에서 실패한 Profile, Bundle, Package, platform, resource 또는 Artifact data는 공개 `taskId`,
Registry reservation, 실행 slot, task cgroup과 target을 만들지 않는다. preflight staging만 만들 수 있으며
응답 전에 제거해야 한다. 6단계 이후 실패는 예약과 staging을 rollback하고 정리를 확인해야 한다.

resolved plan을 만든 뒤 target 시작 전까지 Bundle과 Package cache entry를 daemon이 read-only handle로
고정한다. 경로를 다시 alias resolution해서 다른 실행물로 바꾸지 않는다.

## Protocol v2 오류 코드

Protocol v1 operation은 기존 오류 표를 그대로 사용한다. v2는 공통 코드를 같은 의미로 재사용하고 다음
코드를 추가한다.

| 코드 | 의미 | `retryable` |
|---|---|---|
| `INVALID_REQUEST` | envelope, JSON type 또는 operation/version 조합이 잘못됨 | `false` |
| `INVALID_PROFILE_INPUT` | scalar input·slot·override가 Profile schema와 맞지 않음 | `false` |
| `UNSUPPORTED_PROTOCOL_VERSION` | 지원하지 않는 protocolVersion | `false` |
| `BUNDLE_NOT_FOUND` | 요청한 Bundle digest가 local store에 없음 | `false` |
| `BUNDLE_INTEGRITY_FAILED` | 저장된 Bundle이 digest 또는 schema 검증에 실패함 | `false` |
| `PROFILE_NOT_FOUND` | Bundle의 id/version이 요청과 다르거나 Profile이 없음 | `false` |
| `PACKAGE_NOT_FOUND` | Bundle이 고정한 Runtime Package가 local cache에 없음 | `false` |
| `PACKAGE_INTEGRITY_FAILED` | Package manifest, file 또는 entrypoint 검증 실패 | `false` |
| `PLATFORM_UNSUPPORTED` | Bundle 또는 Package platform이 daemon host와 맞지 않음 | `false` |
| `ARTIFACT_PATH_NOT_ALLOWED` | input path가 절대·reserved·root 밖이거나 안전하게 resolve할 수 없음 | `false` |
| `ARTIFACT_NOT_FOUND` | input file이 없음 | `false` |
| `ARTIFACT_DIGEST_MISMATCH` | input size 또는 digest가 descriptor와 다름 | `false` |
| `TASK_KIND_MISMATCH` | Raw Task를 Profile result operation으로 조회함 | `false` |
| `IDEMPOTENCY_CONFLICT` | 같은 clientRequestId에 다른 canonical request를 사용함 | `false` |
| `LIMIT_EXCEEDS_POLICY` | effective resource가 Profile 또는 배포 정책을 넘음 | `false` |
| `CAPACITY_EXHAUSTED` | 실행 slot 또는 Registry 수용량 부족 | `true` |
| `ENVIRONMENT_UNAVAILABLE` | 안전한 cgroup, cache 또는 Artifact root 조건을 사용할 수 없음 | `false` |
| `TASK_NOT_FOUND` | Task가 없거나 보관 기간이 지남 | `false` |
| `INTERNAL_ERROR` | 예상하지 못한 준비 오류 또는 read-back 불일치 | 응답 값에 따름 |

path traversal, absolute path, reserved path와 symlink component는 attacker-controlled path를 오류에
반영하지 않고 `ARTIFACT_PATH_NOT_ALLOWED`로 처리한다. 잘못된 digest 문법이나 scalar type은
`INVALID_PROFILE_INPUT`이다.

## v0.2 acceptance gate

v0.2 Product Alpha 완료는 문서 merge나 type 추가만으로 판정하지 않는다. 다음 gate를 모두 통과해야 한다.

1. Protocol v1 fixture가 byte-for-byte 그대로 유지되고 기존 Java Raw SDK가 v0.2 daemon을 상대로 네
   operation을 통과한다.
2. daemon이 `[1,2]`를 광고하고 Rust·Java가 같은 v2 valid/invalid fixture를 검증한다.
3. generic `ProfileRequest`가 FFmpeg 전용 분기 없이 한 Profile을 실행하며 같은 canonical request의
   resolved plan digest가 반복 실행과 Rust·Java에서 같다.
4. Bundle과 Package missing, digest 손상, platform 불일치, unknown field가 Task/Registry/cgroup/target
   side effect 없이 거절된다.
5. scalar의 type confusion, extra/missing slot, resource 상한 초과와 idempotency conflict가 side
   effect 없이 거절된다.
6. `..`, absolute·reserved path, symlink·magic-link, digest mismatch와 oversized input이 fail-closed로
   거절된다.
7. 성공 FFmpeg transcode가 `video/mp4` output 하나를 no-overwrite atomic rename으로 공개하고 실제
   size/digest가 결과와 일치한다. 성공 Artifact는 Registry 만료와 daemon 재시작 뒤에도 남는다.
8. non-zero exit, exec 실패, timeout, 취소, output 누락·초과와 publish 경합에서 partial final output이
   없고 staging과 전체 process tree가 정리된다.
9. Package import 재실행은 멱등이고 두 Bundle fixture가 같은 Package digest cache entry를 공유함을
   검증한다.
10. 실제 지원 Linux cgroup v2 환경에서 CPU·memory·PID·timeout과 whole-task cleanup 회귀 gate를 다시
    통과한다. 환경 부족으로 exit 77을 반환한 실행은 통과 근거로 세지 않는다.
11. Java `FfmpegResult result = ffmpeg.transcode(...)` reference workflow가 설치 문서의 clean-host 절차로
    성공하고 Raw Command나 cgroup 세부사항을 사용자 코드에 노출하지 않는다.

## v0.2 non-goals

- Remote transport, TCP listener, Gateway, caller authentication과 remote authorization
- TaskCage Hub, network Registry, Package download와 자동 update
- 두 개 이상의 표준 Execution Profile 또는 일반 workflow engine
- 여러 output Artifact의 transaction, Artifact streaming, URI Artifact와 object storage
- Package cache eviction, garbage collection과 online dependency resolution
- Bundle publisher 서명 PKI와 public trust policy
- Raw Command 제거 또는 Protocol v1 의미 변경
- 영속 Task Registry, daemon 재시작 뒤 Task 재개와 exactly-once 보장
- queue, retry workflow, distributed scheduler와 node autoscaling
- namespace, filesystem, network, syscall을 포함한 untrusted-code sandbox

v0.2는 Local에서 하나의 실제 도구를 Profile, digest 고정 Package와 Artifact 계약으로 끝까지 실행해
TaskCage가 단순 cgroup wrapper가 아니라 **외부 프로세스를 실행 계약으로 만드는 runtime**임을 증명하는
단계다.
