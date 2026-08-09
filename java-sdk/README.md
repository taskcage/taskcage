# TaskCage Java SDK

TaskCage Java SDK는 Java 애플리케이션이 Linux 호스트의 `taskcaged`에 작업을 제출·조회·취소하도록 제공하는 Java 17+ 라이브러리다. cgroup과 Protocol v1의 세부 사항은 SDK 내부에 숨기고 명령, 자원 예산, 상태와 결과를 Java 타입으로 제공한다.

> **상태:** `0.1.0-SNAPSHOT` PoC. 아직 Maven Central에 배포되지 않았으며 Spring Boot에 의존하지 않는다.

현재 SDK는 Local UDS transport와 Raw Command 모델을 구현한다. Execution Profile과 인증된 Remote
transport를 포함한 제품 방향은 [제품 철학과 용어](../docs/product-philosophy.md)에서 정의하며 아직 구현된
SDK 기능이 아니다.

## 역할과 v0.1 목표

이 모듈은 특정 외부 도구를 위한 Binding이 아니라 **TaskCage Java Core SDK**다. Java 객체와
Protocol v1 사이를 변환하고, `taskcaged` 연결과 Task 생명주기를 공통 API로 제공한다.

```text
Java application
    │
    ├─ 향후 FFmpeg·Chromium Profile Binding
    │             │
    └──── TaskCage Java Core SDK
                  │ UDS / Protocol v1
                  ▼
              taskcaged
```

`v0.1.0-alpha`의 목표는 처음 사용하는 Java 개발자가 10분 안에 SDK를 설치하고, 같은 Linux
호스트의 `taskcaged`를 통해 FFmpeg 작업 하나를 안전하게 실행하는 것이다. Profile·Bundle·Hub보다
현재 검증된 Raw Command 실행, 자원 제한과 프로세스 트리 정리를 쉽게 사용하는 경험을 우선한다.

## 현재 구현 기반

현재 Core SDK에는 다음 저수준 기능이 구현되어 있다.

- UDS 연결과 length-prefixed JSON Protocol v1 처리
- `capabilities()`, `submit()`, `getTask()`, `cancelTask()`
- 호출자 지정 UUID를 이용한 데몬 생존 기간 내 멱등 제출
- `RUNNING`/`FINISHED` snapshot과 종료 결과 변환
- 연결·프로토콜·데몬 오류 구분
- 가짜 UDS daemon 단위 테스트와 실제 Linux daemon E2E 테스트
- `ResourceBudget.safeDefaults()`와 `TaskSpec(command)`의 유한한 요청 기본값

현재 호출자는 필요할 때 자원 예산을 override하고 `getTask()`를 직접 polling해야 한다. Maven Central 배포,
완료 대기와 동기 실행 편의 API, 독립 FFmpeg 예제는 아직 구현되지 않았다.

## v0.1 Public Alpha 범위

### 편의 API

기존 저수준 API는 유지하고 그 위에 다음 사용 경험을 제공한다.

```java
try (TaskCageClient client = TaskCageClient.connect(config)) {
    TaskResult result = client.run(
        Command.of("/usr/bin/ffmpeg", "-i", input.toString(), output.toString())
    );
}
```

비동기 호출은 Task handle로 상태 확인, 완료 대기와 취소를 묶는다.

```java
TaskHandle task = client.submit(command);

TaskSnapshot snapshot = task.get();
TaskResult result = task.await();
// 또는 task.cancel()
```

목표 동작은 다음과 같다.

- `run()`은 제출부터 `FINISHED`까지 기다린 뒤 최종 결과를 반환한다.
- `await()`는 SDK 내부 polling으로 완료를 기다리며 polling 간격과 전체 대기 시간을 설정할 수 있다.
- 대기 중 thread interruption은 보존하고 명확한 SDK 예외 또는 interruption 계약으로 전달한다.
- `TaskHandle.cancel()`은 기존 `cancelTask()`를 사용하며 daemon의 whole-cgroup cleanup 완료 뒤 반환한다.
- client `close()`는 기존과 같이 SDK 자원만 정리하고 제출된 Task를 자동 취소하지 않는다.

### 안전한 기본 자원 정책

Protocol v1은 CPU·메모리·PID·벽시계 시간과 출력 tail 제한을 모두 필수로 요구한다. v0.1에서는
프로토콜을 바꾸지 않고 Core SDK가 문서화된 유한 기본값을 채워 전송하며, daemon이 기존 정책에 따라
최종 검증한다. 사용자는 필요한 항목만 작업별로 override할 수 있어야 한다.

```java
TaskResult result = client.run(
    command,
    ResourcePolicy.builder()
        .timeout(Duration.ofMinutes(5))
        .memoryMaxBytes(1024L * 1024 * 1024)
        .build()
);
```

SDK 기본값은 무제한 값을 사용하지 않는다. 실제 수치는 구현 전에 FFmpeg 예제와 daemon 정책을 기준으로
확정하고, 공개 API 문서와 테스트에 함께 고정한다. daemon 기본 정책과 부분 필드 생략은 향후 protocol
변경 후보이며 v0.1 범위가 아니다.

### 결과와 오류

동기·비동기 편의 API는 동일한 최종 결과 타입을 사용한다.

```text
TaskResult
├─ taskId
├─ terminationReason
├─ exitCode / signal
├─ timing / resourceUsage
└─ stdout / stderr tail과 truncation 여부
```

외부 프로그램의 0이 아닌 종료, timeout, OOM, PID 제한과 취소는 정상적으로 완료된 Task 결과다. UDS
연결 실패, protocol 위반과 daemon 오류는 기존 `TaskCageException` 계열로 구분한다. 편의 API가
외부 프로그램의 실패를 일반 SDK 통신 예외로 바꾸지 않는다.

### 배포와 버전

v0.x 동안 daemon과 Java Core SDK는 같은 release train과 버전을 사용한다. 실제 wire 호환성은 제품
버전과 별개인 Protocol 버전으로 판단한다.

```text
GitHub release: v0.1.0-alpha.1
taskcaged:      0.1.0-alpha.1
Java Core SDK:  0.1.0-alpha.1
Protocol:       1
```

Maven Central에는 다음 좌표로 main, sources와 javadoc artifact를 서명해 배포하는 것을 목표로 한다.

```kotlin
dependencies {
    implementation("io.github.taskcage:taskcage-java-sdk:0.1.0-alpha.1")
}
```

### FFmpeg 예제

`examples/ffmpeg-java/`에 Core SDK의 Raw Command API만 사용하는 독립 예제를 제공한다. 별도 FFmpeg
Profile Binding을 만들지 않으며, 설치부터 변환 결과 확인까지 10분 안에 재현할 수 있어야 한다.

예제는 최소한 다음을 보여준다.

- `/usr/bin/ffmpeg`와 argv 배열을 이용한 변환
- 안전한 기본 정책과 작업별 timeout override
- 성공·실패·timeout 결과 처리
- stdout/stderr tail과 자원 사용량 확인
- timeout 뒤 잔여 자식 프로세스가 없다는 Linux E2E 검증 경로

## 구현 순서

각 단계는 독립적으로 리뷰 가능한 PR과 커밋으로 나눈다.

1. **공개 API 확정:** 현재 `ExternalCommand`, `ResourceBudget`, `ExecutionResult`와 목표 API의
   `Command`, `ResourcePolicy`, `TaskResult` 관계를 확정하고 Alpha 이전 불필요한 중복 타입을 피한다.
2. **Task 편의 API:** `TaskHandle`, `get`, `await`, `cancel`과 polling·deadline·interruption 계약을
   구현하고 가짜 daemon 단위 테스트를 추가한다.
3. **동기 실행과 기본 정책:** `run()`과 유한 기본 자원 정책, 부분 override를 구현하고 실제 daemon
   E2E로 정상 종료·timeout·취소·출력 결과를 검증한다.
4. **배포 준비:** Maven Central publishing, 서명, POM metadata, sources/javadoc artifact와
   `0.1.0-alpha.1` 버전을 구성한다.
5. **첫 사용자 경로:** 독립 FFmpeg 예제와 설치·daemon 연결·실행·문제 해결 문서를 완성한다.

## v0.1 완료 기준

- Java 17에서 단위 테스트와 실제 Ubuntu 24.04 daemon E2E가 통과한다.
- 제한 없는 기본 실행 경로가 없다.
- `run`, `await`, `cancel`이 같은 종료·정리 계약을 유지한다.
- Maven Central에서 SDK를 설치할 수 있다.
- 새 사용자가 문서만 보고 10분 안에 FFmpeg 변환을 실행할 수 있다.
- Alpha 버전과 Protocol v1 호환 범위가 문서에 명시된다.

## v0.1 범위 밖

- Execution Profile과 범용 `ProfileRequest`
- FFmpeg·Chromium Profile Binding
- TaskCage Bundle과 Runtime Package cache
- TaskCage Hub 연동
- Remote TCP/TLS와 Artifact upload/download
- Spring Boot starter와 다른 언어 SDK

이 기능들은 Raw Command 기반 v0.1 사용 경험을 외부 사용자가 검증한 뒤 v0.2 후보로 다룬다.

## 빌드

```bash
./gradlew build
```

현재 프로젝트에서 로컬 Maven 저장소 배포는 구성하지 않았다. 빌드 결과는 `build/libs/`에 생성된다.

## 연결

`TaskCageClient`는 `AutoCloseable`이다. 소켓 연결은 첫 요청에서 열리며, `close()`는 SDK의 연결만 닫고 이미 제출된 데몬 작업을 취소하지 않는다.

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

소켓 경로는 필수다. SDK는 데몬 위치나 기본 경로를 추정하지 않는다.

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
    TaskSubmission submission = client.submit(spec);

    if (submission instanceof Task task) {
        TaskSnapshot snapshot = client.getTask(task.taskId());
    } else if (submission instanceof FinishedTaskSnapshot finished) {
        ExecutionResult result = finished.result();
    }
}
```

안전 기본값은 CPU `100000/100000`, memory 512 MiB, PID 32, 벽시계 2분, stdout/stderr tail 각각
65,536 bytes다. 이 값은 SDK가 보내는 요청값이며 daemon과 협상한 capability가 아니다. 배포 정책이 더
낮으면 `LIMIT_EXCEEDS_POLICY`로 거절된다. override는 `new TaskSpec(command, new ResourceBudget(...))`로
명시하며 배포 최대값을 넘을 수 없다.

`submit(spec)`은 SDK가 멱등 키를 생성한다. 응답 유실 뒤 동일한 제출을 복구해야 하는 호출자는 UUID를 직접 보관하고 재사용할 수 있다.

```java
UUID clientRequestId = UUID.randomUUID();
TaskSubmission submission = client.submit(clientRequestId, spec);
```

같은 데몬 프로세스에서 같은 UUID와 같은 요청을 다시 보내면 기존 작업을 반환한다. 데몬 재시작을 가로지르는 exactly-once 실행은 보장하지 않는다.

## 조회와 취소

```java
TaskSnapshot snapshot = client.getTask(taskId);

if (snapshot instanceof RunningTaskSnapshot running) {
    // 실행 중
} else if (snapshot instanceof FinishedTaskSnapshot finished) {
    ExecutionResult result = finished.result();
}

TaskCancellation cancellation = client.cancelTask(taskId);
```

`cancelTask()`는 취소 접수 시점이 아니라 데몬이 작업 cgroup 전체 정리를 확인한 뒤 반환한다. 상세 최종 결과가 필요하면 `getTask(taskId)`로 다시 조회한다.

현재 SDK에는 완료까지 polling하는 `await()`나 동기 `run()` 편의 API가 없다. 호출자가 `getTask()`의 polling 간격과 deadline을 정한다.

## 주요 타입

| 타입 | 역할 |
|---|---|
| `TaskCageClient` | capability 조회, 작업 제출·조회·취소 |
| `TaskCageClientConfig` | UDS 경로와 연결·요청 timeout |
| `TaskSpec` | 외부 명령과 필수 자원 예산 |
| `ExternalCommand` | 실행 파일, argv, 작업 디렉터리, 환경 변수 |
| `ResourceBudget` | CPU·메모리·PID·벽시계 시간·출력 tail 상한 |
| `TaskSubmission` | 수락된 `Task` 또는 즉시 완료된 결과 |
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

현재 E2E는 제출·조회·취소, exec 시작 실패, timeout, 자식 프로세스 정리, 출력 tail, 멱등 제출을 검증한다. wire 계약은 [Protocol v1 API 명세](../docs/api-mvp.md)를 따른다.
