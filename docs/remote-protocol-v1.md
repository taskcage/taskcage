# TaskCage Remote Protocol v1

> 상태: daemon과 SDK 구현을 시작하기 위한 **승인된 공통 계약**이다. Local UDS Protocol v1/v2를
> TCP에 그대로 공개하는 규격이 아니며, Remote Raw Command는 지원하지 않는다.

## 목적과 범위

Remote Protocol v1은 다른 Linux host의 `taskcaged`에 설치된 Execution Profile을 안전하게 제출·조회·취소하는
최소 wire 계약이다. 이 문서는 Rust daemon과 언어 SDK가 독립적으로 구현할 수 있도록 transport, 인증,
authorization, request/response, 오류와 장애 의미를 정한다.

포함 범위:

- 직접 daemon TLS listener와 `taskcage+tls://host:port` URI
- TLS server identity 검증과 service-account ID/secret 인증
- Profile submit, query, cancel과 daemon 생존 기간 내 멱등성
- Profile별 authorization과 resource override policy
- SDK가 관리하는 input Artifact upload와 output Artifact download

범위 밖:

- plaintext TCP, UDS fallback, interactive user login, Remote Raw Command
- mTLS, OAuth/OIDC, external gateway, HTTP API
- object-storage URI reference와 공유 filesystem path
- daemon 재시작 뒤 task recovery·exactly-once·durable queue

## topology와 TLS

초기 topology는 **daemon 직접 listener**다. Gateway는 이 계약을 유지하는 별도 배포 선택지이며 Remote
Protocol v1의 필수 구성요소가 아니다.

- URI: `taskcage+tls://<host>:<port>`; user info, query string, password, private key는 금지한다.
- TLS 1.3을 사용한다. plaintext listener와 TLS downgrade는 지원하지 않는다.
- daemon은 server certificate와 private key를 갖고, certificate SAN에 URI host의 DNS name 또는 IP address가
  있어야 한다.
- SDK는 explicit trust store 또는 platform trust store로 certificate chain과 hostname을 검증한다. 검증을 끄는
  옵션은 제공하지 않는다.
- ALPN protocol ID는 `taskcage/remote/1`이다. ALPN 불일치 또는 TLS handshake 실패는 application frame을
  보내지 않고 연결을 닫는다.
- `connectTimeout`은 DNS, TCP connect, TLS handshake 전체에 적용한다. `requestTimeout`은 인증 완료 뒤 한
  request/response round trip에만 적용하며 running Task를 취소하지 않는다.

### Listener limits and deadlines

The daemon deployment must configure positive values for `maxRemoteConnections`, `tlsHandshakeTimeout`,
`authenticationTimeout`, and `idleConnectionTimeout`. These are deployment policy, not SDK defaults or
capabilities negotiated by the client.

- `maxRemoteConnections` counts accepted TCP connections from the start of the TLS handshake until close.
  When the cap is reached, the daemon closes newly accepted sockets before TLS application data; it does not
  consume a Task execution slot or send a protocol response.
- `tlsHandshakeTimeout` starts after TCP acceptance. An incomplete or failed handshake closes without a
  protocol response.
- `authenticationTimeout` starts after a successful TLS handshake and ends only after a valid
  `authenticated` response. Expiry closes without a protocol response.
- `idleConnectionTimeout` starts again after each complete request or response. Expiry closes without a
  protocol response and does not cancel submitted Tasks or completed Artifacts.
- The daemon applies the same connection cap to all Remote principals. Task execution capacity remains a
  separate, host-wide admission control limit.

## framing과 envelope

TLS application data 안에는 다음 Remote 전용 framing을 사용한다.

```text
+-----------------------+--------------------+
| 4-byte unsigned N     | N-byte UTF-8 JSON  |
| big-endian            | object             |
+-----------------------+--------------------+
```

- `N`은 1 이상 1,048,576 이하(1 MiB)다.
- JSON top-level은 object이며 duplicate key를 허용하지 않는다.
- 각 연결은 요청과 응답을 순서대로 하나씩 처리한다. server push와 multiplexing은 없다.
- `remoteProtocolVersion`은 integer `1`이어야 한다. Local Protocol의 `protocolVersion`과 별도 namespace다.
- 인증 후 요청의 unknown field, unknown `type`, 잘못된 UUID 또는 잘못된 JSON type은 `INVALID_REQUEST`다.
  성공 응답의 unknown field는 SDK가 무시한다.

공통 envelope:

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "authenticate",
  "payload": {}
}
```

`requestId`는 UUID request/response correlation ID이고, response는 요청 값을 그대로 돌려준다. `message`는
사람을 위한 진단 값이며 SDK는 `code`와 `retryable`만으로 분기한다.

Before authentication, a malformed length prefix, invalid UTF-8/JSON, duplicate key, non-object JSON, or
incomplete frame closes the connection without a response. A complete, syntactically valid envelope with an
unsupported `remoteProtocolVersion` receives `UNSUPPORTED_REMOTE_PROTOCOL_VERSION` and then closes. A valid
version whose first `type` is not `authenticate` receives `AUTHENTICATION_REQUIRED` and then closes. These
rules prevent unauthenticated callers from using the Remote operation surface.

## 인증과 authorization

TLS handshake가 끝난 뒤 각 새 연결의 **첫 frame은 반드시 `authenticate`**여야 한다. 인증 전에는 다른
operation을 처리하지 않으며 daemon은 연결을 닫는다.

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "authenticate",
  "payload": {
    "clientId": "document-worker",
    "secret": "supplied-outside-the-uri"
  }
}
```

성공 응답:

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "authenticated",
  "payload": {
    "principal": "document-worker",
    "sessionExpiresAt": "2026-08-13T12:30:00Z"
  }
}
```

- `clientId`는 1~63 ASCII bytes의 `[a-z][a-z0-9-]{0,62}`다.
- `secret`은 1~4096 UTF-8 bytes이며 URI, log, error response 또는 metric label에 기록하면 안 된다.
- daemon은 secret의 salted, memory-hard verifier만 보관한다. 유효하지 않은 ID와 secret은 동일하게
  `AUTHENTICATION_FAILED`로 처리한다.
- `AUTHENTICATION_FAILED` response를 보낸 daemon은 해당 TLS connection을 닫는다. SDK는 secret을 바꾸지
  않은 자동 인증 재시도를 하지 않는다.
- `sessionExpiresAt` 이후 daemon은 새 request를 받지 않고 연결을 닫는다. SDK는 새 TLS connection을 만들고
  다시 인증할 수 있지만 이전 request를 자동 재제출하지 않는다.
- `sessionExpiresAt` is an RFC 3339 UTC timestamp using a trailing `Z`, with second or fractional-second
  precision. It is strictly later than the time at which the daemon sends `authenticated`.
- 인증은 identity를, authorization은 허용된 Profile identity와 resource override 범위를 결정한다. 권한 밖의
  Profile 또는 override는 `AUTHORIZATION_DENIED`다.
- secret은 explicit SDK option, environment-backed secret provider 또는 secret-manager integration으로
  제공한다. explicit option이 이를 우선한다.
- Credential rotation accepts a newly configured secret for new connections and rejects the replaced secret
  for new authentication. Existing authenticated sessions remain valid only until their advertised
  `sessionExpiresAt`.
- Credential revocation immediately rejects new authentication and closes all currently authenticated
  connections for that principal. It does not cancel existing Tasks; their owner may reconnect only with a
  non-revoked credential before the normal Task retention period expires.
- mTLS는 future optional authentication mode다. 도입하더라도 Remote Protocol v1의 Profile authorization
  모델과 error code를 변경하지 않는다.

## operations

인증 뒤 허용되는 operation은 `getCapabilities`, Artifact upload/download operation,
`submitProfile`, `getProfileResult`, `cancelTask`다. `submitTask`와 모든 Raw Command field는 항상
`AUTHORIZATION_DENIED`다.

### `getCapabilities`

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "22222222-2222-4222-8222-222222222222",
  "type": "getCapabilities",
  "payload": {}
}
```

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "22222222-2222-4222-8222-222222222222",
  "type": "capabilities",
  "payload": {
    "daemonVersion": "0.3.0",
    "remoteProtocolVersions": [1],
    "maxFrameBytes": 1048576,
    "artifactModes": ["MANAGED_TRANSFER"],
    "maxArtifactBytes": 104857600,
    "maxArtifactChunkBytes": 780000,
    "artifactRetentionSeconds": 600
  }
}
```

SDK는 인증 성공 뒤 `getCapabilities`를 호출해 `1`과 `MANAGED_TRANSFER`를 확인한다. 조건이 없으면 Profile
API를 Raw Command로 fallback하지 않는다. `maxArtifactChunkBytes`는 raw byte 상한이며 base64 encoding 뒤에도
1 MiB frame 상한을 넘지 않는 양의 정수여야 한다.

### Managed Artifact transfer

Remote client의 파일 경로는 protocol field가 아니다. SDK는 파일 bytes를 TLS connection으로 업로드하고,
daemon은 principal 전용 staging area에서 크기와 SHA-256 digest를 검증한다. upload가 완료되기 전에는
Profile Task, cgroup, target process 또는 output Artifact를 만들지 않는다.

Each Remote principal has an explicit daemon deployment policy containing `artifactUploadAllowed`,
`maxPrincipalArtifactBytes`, and `maxPrincipalArtifacts`. Before allocating an Artifact ID, opening a staging
file, reserving bytes, or otherwise creating an Artifact side effect, `beginArtifactUpload` verifies all of:

- the authenticated principal has `artifactUploadAllowed`;
- the declared `sizeBytes` is within both `maxArtifactBytes` and `maxPrincipalArtifactBytes`;
- accepting the upload would not exceed the principal's retained input Artifact count or byte budget.

Missing upload permission returns `AUTHORIZATION_DENIED`; a declared size or retained Artifact quota violation
returns `ARTIFACT_UPLOAD_LIMIT_EXCEEDED`. Neither response creates an Artifact record, staging file, or Task.
The daemon releases the principal quota when it deletes an aborted, expired, failed-preflight, or otherwise
unreferenced input Artifact. An accepted Task holds its input snapshot outside this upload quota until Task
cleanup finishes.

#### `beginArtifactUpload`

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "beginArtifactUpload",
  "payload": {
    "clientArtifactId": "44444444-4444-4444-8444-444444444444",
    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "sizeBytes": 1234,
    "mediaType": "audio/mpeg"
  }
}
```

`clientArtifactId`는 principal-scoped idempotency key다. 같은 principal과 같은 descriptor는 existing upload의
`artifactUploadStarted` response를 반환하고, descriptor가 다르면 `IDEMPOTENCY_CONFLICT`다. `sizeBytes`는
`maxArtifactBytes` 이하의 positive integer이고, digest는 lower-case SHA-256이다. `mediaType`은 optional
non-empty string이며 daemon은 이를 실행 권한으로 해석하지 않는다.

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "artifactUploadStarted",
  "payload": {
    "artifactId": "55555555-5555-4555-8555-555555555555",
    "nextOffset": 0
  }
}
```

#### `uploadArtifactChunk`와 `completeArtifactUpload`

SDK는 `nextOffset`부터 `maxArtifactChunkBytes` 이하의 raw bytes를 standard base64로 보낸다. chunk는 순차적이며,
offset이 다르거나 empty/invalid base64, declared size 초과, complete 뒤 추가 write는 `INVALID_ARTIFACT_UPLOAD`다.

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "66666666-6666-4666-8666-666666666666",
  "type": "uploadArtifactChunk",
  "payload": {
    "artifactId": "55555555-5555-4555-8555-555555555555",
    "offset": 0,
    "dataBase64": "dGVzdA=="
  }
}
```

성공 응답 `artifactChunkAccepted`는 같은 `artifactId`와 다음 raw byte offset `nextOffset`을 반환한다. SDK는
connection loss 뒤 새 TLS connection에서 인증하고 마지막으로 확인된 offset의 chunk부터 재개할 수 있다.

모든 bytes를 보낸 뒤 `completeArtifactUpload`를 호출한다. daemon은 declared size와 digest를 검증하고 성공하면
`artifactUploaded`를 반환한다. mismatch는 `ARTIFACT_DIGEST_MISMATCH`이며 incomplete staging data는 Profile에
노출하지 않는다.

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "77777777-7777-4777-8777-777777777777",
  "type": "artifactUploaded",
  "payload": {
    "artifactId": "55555555-5555-4555-8555-555555555555",
    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "sizeBytes": 1234,
    "expiresAt": "2026-08-13T12:10:00Z"
  }
}
```

#### `abortArtifactUpload`

The owning principal may explicitly discard an incomplete upload or a completed input Artifact that has not
yet been referenced by an accepted Task.

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "88888888-8888-4888-8888-888888888888",
  "type": "abortArtifactUpload",
  "payload": {
    "artifactId": "55555555-5555-4555-8555-555555555555"
  }
}
```

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "88888888-8888-4888-8888-888888888888",
  "type": "artifactUploadAborted",
  "payload": {
    "artifactId": "55555555-5555-4555-8555-555555555555"
  }
}
```

On success the daemon removes all staging or completed-input bytes and releases the principal quota before
returning `artifactUploadAborted`. An expired, nonexistent, other-principal, already-aborted, or output
Artifact returns `ARTIFACT_NOT_FOUND`. An input Artifact referenced by an accepted Task returns
`ARTIFACT_IN_USE` and remains available to that Task until cleanup. Connection loss, expiry, failed Profile
preflight, and daemon restart discard incomplete uploads; the daemon may discard unreferenced completed input
Artifacts after `artifactRetentionSeconds`.

### `submitProfile`

`submitProfile`은 [Local Profile Core API v2](api-profile-v2.md)의 `ProfileRequest` 의미를 유지하되,
`LOCAL_INPUT`을 받지 않는다. Remote Artifact input은 complete된 `MANAGED_INPUT`으로만 표현한다.

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "33333333-3333-4333-8333-333333333333",
  "type": "submitProfile",
  "payload": {
    "clientRequestId": "44444444-4444-4444-8444-444444444444",
    "profile": { "name": "ffmpeg-audio-to-wav", "version": "1.0.0" },
    "inputs": {
      "source": {
        "kind": "MANAGED_INPUT",
        "artifactId": "55555555-5555-4555-8555-555555555555"
      }
    },
    "resourceOverrides": {
      "limits": { "wallTimeLimitMs": 300000 }
    }
  }
}
```

- scalar inputs와 `resourceOverrides`는 Local Profile Core API v2와 같은 validation 및 canonical identity를
  사용한다.
- `MANAGED_INPUT.artifactId`는 authenticated principal이 소유한 completed input Artifact여야 한다. caller의
  local path, daemon host path, URI, digest, size 또는 output file name은 request에 허용하지 않는다.
- daemon은 private staging area의 completed input만 Profile working area에 materialize한다.
- accepted response는 Local v2의 `profileAccepted`와 동일한 `taskId`, `profile`, `effectiveResources`를
  반환한다.
- `clientRequestId` idempotency는 `(authenticated principal, clientRequestId)` namespace로 한정한다. 같은
  principal과 같은 canonical payload만 기존 task를 반환하며, 다른 payload는 `IDEMPOTENCY_CONFLICT`다.
  같은 실행 중인 daemon으로 재연결한 caller는 같은 ID로 lost response를 복구할 수 있다. daemon restart를
  가로지르는 exactly-once와 task recovery는 보장하지 않는다.

### `getProfileResult`와 `cancelTask`

두 operation은 Remote envelope 안에서 Local Profile Core API v2의 `getProfileResult`, Local Protocol v1의
`cancelTask` payload/result 의미를 따른다. task ID는 authenticated principal의 소유여야 하며, 다른 principal의 task ID는
존재 여부를 밝히지 않고 `TASK_NOT_FOUND`를 반환한다.

### Local and Remote ownership boundary

The daemon has one host-wide cgroup runner and capacity budget, but task visibility is split into ownership
namespaces:

- A Remote task belongs to exactly one authenticated principal. Remote `getProfileResult` and `cancelTask`
  may access only that principal's Remote tasks and return `TASK_NOT_FOUND` for every other task ID.
- Local UDS Protocol v1/v2 operations may access only tasks submitted over Local UDS. They return
  `TASK_NOT_FOUND` for Remote task IDs and cannot read, cancel, download, or reuse Remote Artifacts.
- Remote operations return `TASK_NOT_FOUND` for Local UDS task IDs and cannot access Local Artifacts.
- The daemon's operator-only status, recovery, and cleanup tooling is not an application protocol client and
  may inspect aggregate state without exposing task arguments, output, Artifact names, or service secrets.

This separation prevents a local application account or a different Remote principal from inferring another
caller's task existence. It does not create separate cgroup capacity pools.

finished `profileResult`의 output Artifact는 `MANAGED_OUTPUT`이다. caller가 output path, URI나 파일 이름을
지정할 수 없으며 daemon은 output digest와 size를 검증한 뒤 cleanup-confirmed `FINISHED` 결과와 함께 공개한다.

```json
{
  "kind": "MANAGED_OUTPUT",
  "artifactId": "88888888-8888-4888-8888-888888888888",
  "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "sizeBytes": 4567,
  "mediaType": "audio/wav",
  "expiresAt": "2026-08-13T12:12:00Z"
}
```

SDK는 `readArtifactChunk`로 `artifactId`, raw `offset`, `maxBytes`를 요청하고, `artifactChunk` response의
base64 `dataBase64`, `nextOffset`, `finished`를 순서대로 받아 목적 `Path`에 쓴다. `maxBytes`는 capability의
`maxArtifactChunkBytes` 이하의 positive integer다. artifact는 owning principal만 read할 수 있으며, expiry,
다른 principal 또는 존재하지 않는 ID는 `ARTIFACT_NOT_FOUND`다. SDK는 partial download를 자동 공개하지 않고
caller가 지정한 temporary file을 성공한 digest/size 검증 뒤에만 최종 Path로 publish한다.

## 오류와 연결 장애

오류 response:

```json
{
  "remoteProtocolVersion": 1,
  "requestId": "11111111-1111-4111-8111-111111111111",
  "type": "error",
  "payload": {
    "code": "AUTHENTICATION_FAILED",
    "message": "authentication failed",
    "retryable": false
  }
}
```

| code | 의미 | retryable |
|---|---|---|
| `INVALID_REQUEST` | frame, envelope 또는 value validation 실패 | false |
| `UNSUPPORTED_REMOTE_PROTOCOL_VERSION` | 지원하지 않는 Remote version | false |
| `AUTHENTICATION_FAILED` | service account ID 또는 secret 검증 실패 | false |
| `AUTHENTICATION_REQUIRED` | authenticate 이전 operation 요청 | false |
| `AUTHORIZATION_DENIED` | Profile, Raw Command 또는 override 권한 없음 | false |
| `INVALID_ARTIFACT_UPLOAD` | upload descriptor, offset, chunk 또는 completion 순서 위반 | false |
| `ARTIFACT_UPLOAD_LIMIT_EXCEEDED` | principal Artifact size, count 또는 retained byte policy 초과 | false |
| `ARTIFACT_DIGEST_MISMATCH` | uploaded bytes의 digest 또는 size 불일치 | false |
| `ARTIFACT_NOT_FOUND` | expired, incomplete, 다른 principal 소유 또는 없는 Artifact | false |
| `ARTIFACT_IN_USE` | accepted Task가 참조하는 input Artifact를 abort하려 함 | false |
| `CAPACITY_EXHAUSTED` | daemon 실행 slot 또는 Registry 여유 없음 | true |
| `TASK_NOT_FOUND` | task가 없거나 다른 principal 소유 | false |
| `TASK_ALREADY_FINISHED` | 이미 완료된 task의 cancel 요청 | false |
| `PROFILE_NOT_FOUND` | identity가 설치·허용된 Profile과 일치하지 않음 | false |
| `INVALID_PROFILE_INPUT` | slot, typed scalar, Artifact kind 또는 resource override 위반 | false |
| `IDEMPOTENCY_CONFLICT` | 같은 principal의 key에 다른 payload 사용 | false |
| `LIMIT_EXCEEDS_POLICY` | 요청이 Profile 또는 deployment 정책을 초과 | false |
| `ENVIRONMENT_UNAVAILABLE` | 안전한 cgroup 실행·정리 조건 없음 | false |
| `INTERNAL_ERROR` | daemon 내부 오류; response의 retryable을 따른다 | 응답 값 |

DNS, TCP, TLS certificate, ALPN, frame I/O, request timeout과 peer close는 wire error response가 아니다. SDK는
각각 연결 계열 exception으로 구분한다. lost response 뒤 SDK는 자동 `submitProfile` 재시도를 하지 않는다.
호출자는 동일한 `clientRequestId`로 재연결 뒤 다시 제출하거나 결과를 조회한다.

## Java SDK mapping

```java
TaskCageClient.connect(
    "taskcage+tls://taskcage.internal:7443",
    RemoteConnectionOptions.builder()
        .credentials(ServiceCredentials.of(
            "document-worker",
            Secret.fromEnvironment("TASKCAGE_CLIENT_SECRET")))
        .trustStore(trustStore)
        .connectTimeout(Duration.ofSeconds(3))
        .requestTimeout(Duration.ofSeconds(30))
        .build());
```

`RemoteConnectionOptions`는 URI parsing, trust material, service credentials, connect timeout, request timeout과
optional secret provider만 가진다. SDK의 Remote Artifact API는 local source/destination `Path`를 streaming
bytes로 변환하고, protocol에는 `artifactId`만 보낸다. Profile authorization, Artifact retention, secret verifier와
resource policy는 daemon 배포 설정이며 SDK public API가 아니다.

## 구현 gate

Rust daemon과 Java SDK는 [`protocol-fixtures/remote-v1/`](../protocol-fixtures/remote-v1/)의 같은 fixture를
검증해야 한다. daemon 구현은 TLS/authentication 단위 테스트와 real TLS listener E2E를, Java SDK는
encoder/decoder 단위 테스트와 real-daemon TLS E2E를 제공한다. shared fixture가 갱신되지 않은 wire 변경은
호환 변경으로 취급하지 않는다.
