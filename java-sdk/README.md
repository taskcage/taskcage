# TaskCage Java SDK

TaskCage Java SDK는 Java 애플리케이션이 Linux 호스트의 `taskcaged`에 Capsule Task를 제출·조회·취소하도록 제공하는 Java 17+ 라이브러리다. cgroup과 Local Protocol v1·v2, Remote Protocol v1의 세부 사항은 SDK 내부에 숨기고 Capsule, Profile, input/output data, 자원 예산, 상태와 실행 결과를 Java 타입으로 제공한다.

> **상태:** `0.4.0`은 Maven Central에 공개된 최신 artifact다. 기존 Local UDS Raw Command·Profile을
> 유지하면서 Local·Remote Capsule API와 TLS 1.3 Remote Artifact API를 제공한다. Capsule 실행에는
> `taskcaged` `0.5.0` 이상이 필요하다. `0.x`는 초기 개발 버전이며 SDK는 Spring Boot에 의존하지 않는다.

> **Capsule 우선:** Raw Command API는 호환성을 위해 유지한다. 새 integration은 Capsule 이름과 Profile이
> 선언한 typed input을 전달하며 실행 파일 경로와 argv를 직접 지정하지 않는다.

`0.4.0`은 Local UDS transport, Raw Command, opt-in [Local Profile Core API v2](../docs/api-profile-v2.md),
인증된 Remote transport와 Capsule runner를 구현한다. Remote Raw Command는 의도적으로 노출하지 않는다.

## Core SDK 역할

이 모듈은 특정 외부 도구 전용 API가 아니라 **TaskCage Java Core SDK**다. Java 객체와
Local Protocol v1·v2와 Remote Protocol v1 사이를 변환하고, `taskcaged` 연결과 Task 생명주기를 공통 API로 제공한다.

```text
Java application
    │
    └──── TaskCage Java Core SDK
                  ├─ UDS / Local Protocol v1·v2
                  └─ TLS / Remote Protocol v1
                  ▼
              taskcaged
```

## Capsule 실행 계약

공통 실행 API는 `CapsuleRunner`가 소유한다. Runner 구현은 private `taskcage-exec`를 사용하는 Embedded
backend 또는 daemon-backed External backend일 수 있지만, Capsule identity, typed Capsule input, 대기
timeout, idempotency key와 cleanup-confirmed result의 의미는 동일해야 한다.

현재 SDK는 Local Profile API를 Local External backend로 연결하고, TLS Remote에는
`RemoteCapsuleRunner` adapter를 제공한다.

```java
CapsuleRequest request = CapsuleRequest.builder("ffmpeg-audio-to-wav", "1.0.0")
    .artifact("source", source)
    .int64("sample_rate_hz", 16_000)
    .int64("channels", 1)
    .build();

try (CapsuleRunner runner = CapsuleRunner.external(taskCageClient)) {
    CapsuleExecutionResult result = runner.execute(request, Duration.ofMinutes(2));
}
```

`CapsuleRunner`는 실행 파일 경로와 shell 문자열을 받지 않는다. Capsule이 선언한 Profile과 Runtime
Package를 backend가 검증하며, 현재 External adapter는 그 요청을 설치된 daemon에 전달한다.

`CapsuleRequest`의 Builder는 일반 Capsule 사용자가 `ProfileIdentity`나 `ProfileRequest`를 직접 만들지
않도록 한다. SDK는 Capsule 이름·버전에서 Profile identity를 유도하고, daemon adapter 경계에서만 low-level
`ProfileRequest`로 변환한다. 직접 Profile API는 기존 호환·고급 경로로 유지된다.

Capsule-first MVP의 권장 시작점은 TLS daemon에 `RemoteCapsuleRunner`로 연결하는 경로다. daemon은 일반적인
Docker service처럼 `-p 7443:7443`으로 publish할 수 있고, Java application은 Capsule identity와 typed input만
전달한다. 기본 연결은 MySQL의 `PREFERRED`처럼 TLS를 사용하되 CA를 강제하지 않으며, 공유·운영 환경은 명시적으로
CA 또는 hostname 검증 모드를 선택할 수 있다.

Embedded backend는 `taskcage-exec` private helper를 SDK가 관리하고, helper가 공통 Rust execution core를
호출하는 선택적 확장이다. Embedded backend는 `taskcaged serve` child daemon을 시작하지 않는다.

## Remote Profile 실행

Remote daemon에는 TLS 1.3과 service-account 인증이 필수다. Local UDS용 `TaskCageClient`와 Remote의
`RemoteTaskCageClient`는 의도적으로 분리되어 있어, 원격에서는 Raw Command를 호출할 수 없다.

```java
try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(
        URI.create("taskcage+tls://taskcage.internal:7443"),
        ServiceCredentials.of("document-worker", Secret.fromEnvironment("TASKCAGE_CLIENT_SECRET")))) {
    RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);
    RemoteCapsuleFileRequest request = RemoteCapsuleFileRequest.builder("ffmpeg-audio-to-wav", "1.0.0")
            .inputFile("source", Path.of("input.wav"), "audio/wav")
            .int64("sample_rate_hz", 16_000)
            .int64("channels", 1)
            .outputFile("audio", Path.of("output.wav"))
            .build();

    UUID clientArtifactId = UUID.randomUUID();
    UUID clientRequestId = UUID.randomUUID();
    RemoteArtifactUpload upload = runner.upload(clientArtifactId, request);
    // submit 전에 clientArtifactId, clientRequestId와 upload receipt를 영속화한다.
    RemoteCapsuleTaskHandle task = runner.submit(clientRequestId, request, upload);
    RemoteCapsuleExecutionResult result = task.await(Duration.ofMinutes(2));
    if (result.outcome() == ProfileOutcome.SUCCEEDED) {
        runner.download(request, result);
    }
}
```

`localhost:7443`에서 실행 중인 daemon에는 endpoint를 반복할 필요 없이 다음처럼 연결한다. 기본 정책은
TLS 암호화를 유지하면서 인증서 검증을 선택 사항으로 두는 `PREFERRED`다.

```java
try (RemoteTaskCageClient client = RemoteTaskCageClient.localDefault(
        ServiceCredentials.of("document-worker", Secret.fromEnvironment("TASKCAGE_CLIENT_SECRET")))) {
    RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);
    // RemoteCapsuleFileRequest는 input Path를 TLS로 upload하고 output Path로 download한다.
}
```

`RemoteCapsuleFileRequest`는 같은 TLS 연결에서 input `Path`를 upload하고, daemon-issued Artifact reference로
Capsule을 실행한 뒤 성공한 output Artifact를 지정한 local `Path`로 download한다. Local file paths 자체는
daemon으로 전송되지 않는다. 저수준 `upload`, `RemoteCapsuleRequest`, `download` API도 재시도나 고급 흐름을
위해 그대로 제공한다.

### TLS verification modes

TaskCage keeps TLS encryption mandatory for every Remote connection, but offers MySQL-like verification choices.

| Mode | Java API | Intended use |
| --- | --- | --- |
| `PREFERRED` | default `connect(...)`, `localDefault(...)` | TLS required, but CA and hostname verification are optional |
| `VERIFY_IDENTITY` | `.tlsVerification(TlsVerificationMode.VERIFY_IDENTITY)` | CA and endpoint hostname verification; production recommendation |
| `VERIFY_CA` | `.tlsVerification(TlsVerificationMode.VERIFY_CA)` | CA verification when a stable hostname is unavailable |

`PREFERRED` does not enable plaintext fallback: Remote TaskCage always uses TLS. It is appropriate only when the
network path is already trusted. CA verification uses the JVM trust store or a caller-provided `SSLContext`.

복구 가능한 파일 실행은 다음 단계 경계를 지킨다.

- upload 응답이 유실되면 변경되지 않은 input file과 같은 `clientArtifactId`로 `runner.upload(...)`만 재시도한다.
- upload receipt를 받은 뒤에는 receipt를 보관하고 input을 다시 upload하지 않는다.
- submit 응답 유실이나 wait timeout 뒤에는 같은 `clientRequestId`, file request와 upload receipt로
  `runner.submit(...)`만 재시도한다. daemon은 task-owned input을 다시 찾기 전에 submission idempotency를
  확인하므로 원래 Task handle을 복구한다.
- terminal 성공 뒤 download가 실패하면 같은 result로 `runner.download(...)`만 재시도한다. 이 메서드는 upload나
  submit을 수행하지 않는다.

`execute(RemoteCapsuleFileRequest, Duration)`는 내부 UUID를 사용하는 one-shot 편의 API다. 응답 유실을 복구해야
하는 Worker는 전체 `execute(...)`를 재호출하지 않고 위 단계형 API를 사용한다.

`RemoteCapsuleTaskHandle.await(waitTimeout)`의 timeout은 polling sleep뿐 아니라 각 result snapshot의 TLS 연결,
인증과 응답 read에도 남은 시간으로 전달된다. timeout이 끝나면 accepted Task를 취소하지 않고
`TimeoutException`을 반환하므로, caller는 같은 task handle이나 idempotency key로 terminal result를 다시 확인할 수
있다.

download는 목적지와 같은 디렉터리에 UUID 기반의 고유 임시 파일을 `CREATE_NEW`로 연다. Artifact의 size와 digest를
검증한 뒤에만 목적지로 atomic move하며, network·protocol·검증·move 실패 시 임시 파일을 삭제한다. 기존의 예측
가능한 partial 파일을 열거나 재사용하지 않는다.

FFmpeg Capsule reference workflow는 정상 실행, timeout, memory limit, cancel과 프로세스 트리 정리를
검증한다. Java 개발자는 Capsule archive를 import한 `taskcaged`에 선언된 Profile input으로 작업을 안전하게
실행한다. Hub는 이 흐름의 필수 구성요소가 아니다.

## 현재 구현 기반

현재 Core SDK에는 다음 기능이 구현되어 있다.

- UDS 연결과 length-prefixed JSON Protocol v1 처리
- 저수준 `capabilities()`, `submit()`, `getTask()`, `cancelTask()`
- bounded 동기 `run()`
- `submitHandle()`과 `TaskHandle.get()`, bounded `await()`, `cancel()`
- 호출자 지정 UUID를 이용한 데몬 생존 기간 내 멱등 제출
- `RUNNING`/`FINISHED` snapshot과 종료 결과 변환
- 연결·프로토콜·데몬 오류 구분
- 가짜 UDS daemon 단위 테스트와 실제 Linux daemon E2E 테스트
- `ResourceBudget.safeDefaults()`와 `TaskSpec(command)`의 유한한 요청 기본값
- 설치된 daemon과 실제 FFmpeg를 사용하는 별도 Local reference E2E
- Local Profile v2의 `ProfileRequest`, typed input과 Local Artifact 모델
- Profile 제출·조회·bounded 대기와 Protocol v1 취소를 연결하는 `ProfileTaskHandle`
- 공유 `protocol-fixtures/v2`에 대한 Java encoder/decoder 호환성 테스트
- 실제 daemon의 Profile 실행·Artifact publish·조회·멱등성과 사전 실행 오류를 검증하는 Linux E2E

Profile API는 daemon capability의 `protocolVersions`에 `2`가 있을 때만 요청을 보내며 Raw Command로
fallback하지 않는다. Core E2E는 opt-in `file-copy@1.0.0` Profile와 FFmpeg Capsule로 범용 계약을 검증한다.

## Local Profile API

generic Core API는 실행 파일 경로나 argv 대신 설치된 Profile identity와 typed input을 전달한다.
`TaskCageClient`가 제출·조회·취소와 동기 실행을 제공하며 client lifecycle은 항상 호출자가 소유한다.

```java
ProfileRequest request = new ProfileRequest(
    new ProfileIdentity("file-copy", "1.0.0"),
    Map.of(
        "source", new LocalInputArtifact(
            new ArtifactPath("jobs/42/source.txt"),
            new Sha256Digest(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            12),
        "label", new StringProfileInput("archive"),
        "retain_metadata", new BooleanProfileInput(true),
        "priority", new Int64ProfileInput(3)),
    ProfileResourceOverrides.builder()
        .wallTimeLimit(Duration.ofMinutes(5))
        .build());

try (TaskCageClient client = TaskCageClient.connect(config)) {
    FinishedProfileTaskSnapshot finished = client.run(request, Duration.ofMinutes(6));
    PublishedArtifact result = finished.artifacts().get("result");
}
```

`ArtifactPath`은 daemon이 설정한 Artifact root 기준 wire path이며 `java.nio.file.Path`가 아니다. SDK는
Artifact root, 실행 파일, working directory 또는 output file name을 선택하지 않는다. 연결 실패나 응답
유실 뒤 재시도가 필요하면 `run(UUID clientRequestId, ProfileRequest, Duration)` 또는
`submitProfileHandle(UUID, ProfileRequest)` overload를 사용한다.

다음 Capsule-first 공개 계약에서 `ProfileRequest`는 Capsule manifest가 등록한 Profile만 선택한다. Core SDK는
Capsule의 signature나 Package를 신뢰하지 않으며, daemon이 Capsule allowlist, Package digest, input schema와
resource override를 최종 검증한다. 자세한 계약은 [Capsule archive 형식](../docs/bundle-format.md)을 따른다.

현재 호출자는 필요할 때 자원 예산을 override하고 `run()`으로 동기 실행하거나 `TaskHandle`로 상태
조회·완료 대기·취소를 수행할 수 있다. SDK는 Maven Central의 공개 좌표로 설치할 수 있다.

## Local Raw Command 호환 API

이 절은 현재 공개된 Local Protocol v1과 SDK `0.4.0`의 호환 API를 설명한다. Capsule-first 공개 계약의
새 사용자 경로에는 포함하지 않는다. 기존 사용자는 daemon과 SDK의 지원 기간 동안 이 API를 계속 사용할 수
있지만, 새 integration은 Capsule/Profile 경로를 사용해야 한다.

### 편의 API

기존 저수준 API는 유지하고 그 위에 동기 `run()`과 비동기 `TaskHandle` 사용 경험을 제공한다.

```java
try (TaskCageClient client = TaskCageClient.connect(config)) {
    UUID clientRequestId = UUID.randomUUID();
    FinishedTaskSnapshot finished = client.run(clientRequestId, spec, Duration.ofMinutes(5));
    ExecutionResult result = finished.result();
}
```

비동기 상태 조회, 완료 대기 또는 명시적 취소가 필요하면 handle을 사용한다.

```java
TaskHandle task = client.submitHandle(clientRequestId, spec);
TaskSnapshot snapshot = task.get();
FinishedTaskSnapshot finished = task.await(Duration.ofMinutes(5));
// 또는 task.cancel()
```

동작 계약은 다음과 같다.

- `run()`은 제출 응답 뒤 `TaskHandle.await()`와 같은 계약으로 `FINISHED`까지 기다린다.
- `run()`과 `await()`의 wait timeout은 Task의 cgroup wall-time resource limit과 별개다.
- wait timeout이나 interruption은 Task를 자동 취소하지 않는다.
- `await()`는 SDK 내부 polling으로 완료를 기다리며 polling 간격과 전체 대기 시간을 설정할 수 있다.
- `await()` timeout은 Task를 취소하지 않으며, 다음 `get()`·`await()`·`cancel()` 호출을 허용한다.
- 대기 중 thread interruption은 보존하고 명확한 SDK 예외 또는 interruption 계약으로 전달한다.
- `TaskHandle.cancel()`은 기존 `cancelTask()`를 사용하며 daemon의 whole-task cleanup 완료 뒤 반환한다.
- client `close()`는 기존과 같이 SDK 자원만 정리하고 제출된 Task를 자동 취소하지 않는다.

응답 유실이나 wait timeout 뒤 Task를 복구해야 하는 `run()` 호출은 caller-owned `clientRequestId` overload를
사용하고, 같은 ID를 `submitHandle()`에 다시 전달한다.

### 안전한 기본 자원 정책

Protocol v1은 CPU·메모리·PID·벽시계 시간과 출력 tail 제한을 모두 필수로 요구한다. Raw Command API에서는
Core SDK가 문서화된 유한 기본값을 채워 전송하며, daemon이 기존 정책에 따라
최종 검증한다. 사용자는 필요한 항목만 작업별로 override할 수 있어야 한다.

```java
TaskSpec spec = new TaskSpec(command, explicitResourceBudget);
FinishedTaskSnapshot finished = client.run(spec, Duration.ofMinutes(6));
```

위 `Duration`은 SDK 완료 대기 상한이다. Task 자체의 wall-time 제한은 `explicitResourceBudget`에 별도로
포함하며, cleanup 결과를 기다릴 시간을 고려해 두 값을 독립적으로 정한다.

SDK 기본값은 무제한 값을 사용하지 않는다. 현재 수치는 FFmpeg 예제와 daemon 정책을 기준으로 공개 API
문서와 테스트에 고정했다. daemon 기본 정책과 부분 필드 생략은 현재 Raw Command 계약 범위가 아니다.

### 결과와 오류

동기·비동기 편의 API는 동일한 최종 결과 타입을 사용한다.

```text
FinishedTaskSnapshot
├─ taskId
└─ ExecutionResult
   ├─ terminationReason
   ├─ exitCode / signal
   ├─ timing / resourceUsage
   └─ stdout / stderr tail과 truncation 여부
```

외부 프로그램의 0이 아닌 종료, timeout, OOM, PID 제한과 취소는 정상적으로 완료된 Task 결과다. UDS
연결 실패, protocol 위반과 daemon 오류는 기존 `TaskCageException` 계열로 구분한다. 편의 API가
외부 프로그램의 실패를 일반 SDK 통신 예외로 바꾸지 않는다.

### 배포와 버전

daemon과 Java Core SDK는 독립적으로 버전을 관리하고 배포한다. 실제 wire 호환성은 제품
버전 문자열이 아니라 양쪽이 지원하는 Protocol 버전으로 판단한다.

```text
Daemon tag:     taskcaged-v0.5.0 (Capsule execution)
Java SDK tag:   java-sdk-v0.4.0
Java Core SDK:  0.4.0
Protocol:       Local v1, v2; Remote v1
```

Maven Central에는 다음 좌표로 main, sources와 javadoc artifact가 서명되어 공개됐다.

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.4.0")
}
```

## 빌드

```bash
./gradlew build
```

일반 빌드 결과는 `build/libs/`에 생성된다. 현재 공개 artifact는 다음 좌표로 설치한다.

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.4.0")
}
```

Central 배포 bundle의 재현과 서명 요구사항은 [릴리스 운영](../docs/releasing.md)을 따른다.

## 연결

`TaskCageClient`는 `AutoCloseable`이다. 소켓 연결은 첫 요청에서 열리며, `close()`는 SDK의 연결만 닫고 이미 제출된 데몬 작업을 취소하지 않는다.

기존 Linux host 설치에서는 표준 daemon socket에 가장 짧게 연결할 수 있다. `localDefault()`는 daemon을
설치하거나 기동하지 않으며, `/run/taskcage/taskcaged.sock`에 이미 실행 중인 daemon으로 lazy connection을
만든다. 새 컨테이너 integration의 권장 경로는 위의 TLS `RemoteTaskCageClient`다.

```java
try (TaskCageClient client = TaskCageClient.localDefault()) {
    TaskCageCapabilities capabilities = client.capabilities();
}
```

사용자 지정 Local UDS는 경로만 전달한다.

```java
try (TaskCageClient client = TaskCageClient.connectUnixSocket(
        Path.of("/custom/taskcaged.sock"))) {
    TaskCageCapabilities capabilities = client.capabilities();
}
```

원격 daemon은 별도 `RemoteTaskCageClient`로 연결한다. endpoint는 항상
`taskcage+tls://host:port` 형식이어야 하며, SDK는 기본적으로 JVM의 platform trust configuration과 TLS 1.3을
사용한다. 사설 CA, 별도 trust store 또는 timeout은 `RemoteConnectionOptions`로 명시한다. Local/Remote API를
분리해 원격 endpoint에서 Raw Command를 실행할 수 없게 한다.

```java
TaskCageClientConfig config = TaskCageClientConfig.builder()
    .socketPath(Path.of("/run/taskcage/taskcaged.sock"))
    .connectTimeout(Duration.ofSeconds(1))
    .requestTimeout(Duration.ofSeconds(5))
    .build();

try (TaskCageClient client = TaskCageClient.connect(config)) {
    TaskCageCapabilities capabilities = client.capabilities();
}
```

소켓 경로와 timeout을 제어해야 하는 경우에는 `TaskCageClientConfig`를 사용한다.

## 작업 제출

외부 명령은 shell 문자열이 아니라 실행 파일과 인자 배열로 전달한다. `program`과 `workingDirectory`는 절대 경로여야 하며, 환경 변수는 명시한 값만 전달한다. `TaskSpec(command)`는 SDK 안전 기본값을 사용한다.

```java
TaskSpec spec = new TaskSpec(
    new ExternalCommand(
        Path.of("/usr/bin/pdftotext"),
        List.of("input.pdf", "output.txt"),
        Path.of("/srv/taskcage/jobs/42"),
        Map.of("LANG", "C.UTF-8")));

try (TaskCageClient client = TaskCageClient.connect(config)) {
    FinishedTaskSnapshot finished = client.run(spec, Duration.ofMinutes(3));
    ExecutionResult result = finished.result();
}
```

안전 기본값은 CPU `100000/100000`, memory 512 MiB, PID 32, 벽시계 2분, stdout/stderr tail 각각
65,536 bytes다. 이 값은 SDK가 보내는 요청값이며 daemon과 협상한 capability가 아니다. 배포 정책이 더
낮으면 `LIMIT_EXCEEDS_POLICY`로 거절된다. override는 `new TaskSpec(command, new ResourceBudget(...))`로
명시하며 배포 최대값을 넘을 수 없다.

`submit(spec)`은 SDK가 멱등 키를 생성한다. 응답 유실 뒤 동일한 제출을 복구해야 하는 호출자는 UUID를 직접 보관하고 재사용할 수 있다.

```java
UUID clientRequestId = UUID.randomUUID();
TaskHandle task = client.submitHandle(clientRequestId, spec);
```

같은 데몬 프로세스에서 같은 UUID와 같은 요청을 다시 보내면 기존 작업을 반환한다. 데몬 재시작을 가로지르는 exactly-once 실행은 보장하지 않는다.

## 조회, 완료 대기와 취소

```java
TaskHandle task = client.submitHandle(clientRequestId, spec);
TaskSnapshot snapshot = task.get();

if (snapshot instanceof RunningTaskSnapshot running) {
    // 실행 중
} else if (snapshot instanceof FinishedTaskSnapshot finished) {
    ExecutionResult result = finished.result();
}

FinishedTaskSnapshot finished = task.await(Duration.ofMinutes(3), Duration.ofMillis(100));
// 완료 대기 대신 명시적으로 취소할 때: TaskCancellation cancellation = task.cancel();
```

`await()`는 monotonic deadline 안에서 polling하며, timeout 시 `TimeoutException`을 던지지만 Task를
취소하지 않는다. 대기 thread가 interrupt되면 interrupt 상태를 보존하고 `InterruptedException`을 전달한다.
이미 받은 `FINISHED` 결과는 handle에 보관되므로 다시 조회하지 않는다.

`cancel()`은 취소 접수 시점이 아니라 daemon이 whole-task cleanup을 확인한 뒤 반환한다. 상세 최종 결과가
필요하면 같은 handle에서 `get()` 또는 `await()`를 호출한다. `run()` wait timeout 뒤 Task를 복구하려면
caller-owned ID를 같은 `submitHandle()` 호출에 재사용한다.

## 주요 타입

| 타입 | 역할 |
|---|---|
| `TaskCageClient` | capability 조회, 동기 실행, 작업 제출·조회·취소 |
| `TaskCageClientConfig` | UDS 경로와 연결·요청 timeout |
| `TaskSpec` | 외부 명령과 필수 자원 예산 |
| `ExternalCommand` | 실행 파일, argv, 작업 디렉터리, 환경 변수 |
| `ResourceBudget` | CPU·메모리·PID·벽시계 시간·출력 tail 상한 |
| `TaskSubmission` | 수락된 `Task` 또는 즉시 완료된 결과 |
| `TaskHandle` | 한 Task의 상태 조회, bounded 완료 대기와 cleanup-confirmed 취소 |
| `TaskSnapshot` | `RUNNING` 또는 `FINISHED` 불변 snapshot |
| `ExecutionResult` | 종료 원인, 프로세스 상태, 시간, 사용량, 출력 tail |

## 결과와 오류

외부 프로그램의 종료는 SDK 또는 데몬 통신 오류와 구분한다.

- `EXITED`, `EXECUTION_FAILED`, `CANCELLED`, `TIMED_OUT`, `MEMORY_LIMIT_EXCEEDED`, `PROCESS_LIMIT_EXCEEDED`, `DAEMON_ERROR`는 완료된 작업의 `TerminationReason`이다.
- UDS 연결 실패는 `TaskCageConnectionException`이다.
- 잘못된 프레임이나 응답은 `TaskCageProtocolException`이다.
- `CAPACITY_EXHAUSTED`, `TASK_NOT_FOUND` 같은 데몬 오류는 code와 `retryable`을 가진 `TaskCageDaemonException`이다.

호출 코드는 오류 `message`가 아니라 예외 타입, code와 `retryable`을 기준으로 분기해야 한다.

## 테스트

단위 테스트는 Linux cgroup이나 실제 Rust 데몬을 요구하지 않는다.

```bash
./gradlew test
```

E2E 테스트는 cgroup v2 위임이 준비된 Linux에서 실행 중인 데몬과 fixture 경로를 명시해야 한다.

```bash
TASKCAGE_SOCKET=/home/ubuntu/.local/state/taskcage-dev/taskcaged.sock \
TASKCAGE_GHOST_TREE=/home/ubuntu/TaskCage/target/debug/ghost-tree \
TASKCAGE_OUTPUT_FLOOD=/home/ubuntu/TaskCage/target/debug/output-flood \
  ./gradlew e2eTest
```

현재 E2E는 `run()`, `TaskHandle` 제출·조회·완료 대기·취소, exec 시작 실패, timeout, 자식 프로세스 정리,
출력 tail, 멱등 제출을 검증한다. wire 계약은 [Protocol v1 API 명세](../docs/api-mvp.md)를 따른다.

Legacy Raw Command E2E는 기존 API의 회귀를 검증한다. 새 integration의 FFmpeg Capsule 실행과 cleanup은
[Compose 기반 Remote E2E](../dev/container/README.md)와 `remoteE2eTest` source set에서 검증한다.
