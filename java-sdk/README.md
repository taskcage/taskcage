# TaskCage Java SDK

TaskCage Java SDK는 Java 애플리케이션이 Linux 호스트의 `taskcaged`에 작업을 제출·조회·취소하도록 제공하는 Java 17+ 라이브러리다. cgroup과 Protocol v1의 세부 사항은 SDK 내부에 숨기고 명령, 자원 예산, 상태와 결과를 Java 타입으로 제공한다.

> **상태:** `0.1.0-SNAPSHOT` PoC. 아직 Maven Central에 배포되지 않았으며 Spring Boot에 의존하지 않는다.

현재 SDK는 Local UDS transport와 Raw Command 모델을 구현한다. Execution Profile과 인증된 Remote
transport를 포함한 제품 방향은 [제품 철학과 용어](../docs/product-philosophy.md)에서 정의하며 아직 구현된
SDK 기능이 아니다.

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

외부 명령은 shell 문자열이 아니라 실행 파일과 인자 배열로 전달한다. `program`과 `workingDirectory`는 절대 경로여야 하며, 환경 변수는 명시한 값만 전달한다.

```java
TaskSpec spec = new TaskSpec(
    new ExternalCommand(
        Path.of("/usr/bin/pdftotext"),
        List.of("input.pdf", "output.txt"),
        Path.of("/srv/taskcage/jobs/42"),
        Map.of("LANG", "C.UTF-8")),
    new ResourceBudget(
        new CpuQuota(100_000, 100_000),
        512L * 1024 * 1024,
        32,
        Duration.ofMinutes(2),
        65_536,
        65_536));

try (TaskCageClient client = TaskCageClient.connect(config)) {
    TaskSubmission submission = client.submit(spec);

    if (submission instanceof Task task) {
        TaskSnapshot snapshot = client.getTask(task.taskId());
    } else if (submission instanceof FinishedTaskSnapshot finished) {
        ExecutionResult result = finished.result();
    }
}
```

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
