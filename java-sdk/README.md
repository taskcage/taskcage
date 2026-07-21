# TaskCage Java SDK

TaskCage Java SDK는 Linux 호스트의 `taskcaged` 데몬에 Unix domain socket으로 연결해, 신뢰된 외부 명령을 제한된 작업으로 실행하는 Java 21+ 라이브러리다.

Java 애플리케이션 개발자는 cgroup, Rust, UDS 프레임을 직접 다루지 않는다. 명령, 자원 예산, 실행 결과만 Java 타입으로 다룬다.

## 대상 사용자

- PDF·OCR·이미지·영상 변환 명령을 호출하는 Java 애플리케이션 개발자
- Chromium·Playwright·WebDriver 같은 브라우저 자동화 프로세스를 실행하는 개발자
- 컴파일·문서 생성·데이터 변환처럼 실행 시간이 예측하기 어려운 신뢰된 명령을 운영하는 개발자

이 SDK는 Spring Boot에 의존하지 않는다. Maven 또는 Gradle을 쓰는 일반 Java 애플리케이션에서 사용할 수 있어야 하며, Maven Central 배포를 목표로 한다.

```kotlin
dependencies {
    implementation("io.github.taskcage:taskcage-java-sdk:<version>")
}
```

## 클라이언트 구조

```text
애플리케이션 코드
  └─ TaskCageClient
      ├─ execute(TaskSpec)       // 제출 후 완료까지 대기하는 편의 API
      ├─ submit(TaskSpec)        // Task 반환
      └─ capabilities()          // 데몬·cgroup 준비 상태 확인
          └─ internal
              ├─ UnixDomainSocketTransport
              ├─ LengthPrefixedFrameCodec
              ├─ ProtocolV1Codec
              └─ PollingTask
                  └─ taskcaged (Rust 데몬)
```

`TaskCageClient`는 `AutoCloseable`이다. `close()`는 SDK가 보유한 UDS 연결과 polling 리소스만 정리하며, 이미 제출된 작업을 취소하지 않는다. 제출된 작업의 생명주기는 데몬이 관리한다.

## 공개 API 초안

```java
public interface TaskCageClient extends AutoCloseable {
    ExecutionResult execute(TaskSpec task);

    Task submit(TaskSpec task);

    TaskCageCapabilities capabilities();
}
```

일반적인 동기 호출은 다음과 같다.

```java
try (TaskCageClient client = TaskCageClient.connect(
        TaskCageClientConfig.builder()
            .socketPath(Path.of("/run/taskcage/taskcage.sock"))
            .build())) {

    ExecutionResult result = client.execute(
        TaskSpec.builder()
            .command(ExternalCommand.builder()
                .program(Path.of("/usr/bin/pdftotext"))
                .arguments("input.pdf", "output.txt")
                .workingDirectory(Path.of("/srv/document-jobs/job-123"))
                .build())
            .budget(ResourceBudget.builder()
                .cpu(CpuQuota.of(
                    Duration.ofMillis(100),
                    Duration.ofMillis(100)
                ))
                .memoryMaxBytes(512L * 1024 * 1024)
                .processLimit(32)
                .wallTimeLimit(Duration.ofMinutes(2))
                .outputTailBytes(64 * 1024, 64 * 1024)
                .build())
            .build()
    );

    if (result.isSuccess()) {
        // output.txt 사용
    }
}
```

긴 작업을 호출자가 직접 관리해야 한다면 비동기 API를 사용한다.

```java
Task task = client.submit(taskSpec);

TaskSnapshot current = task.status();
ExecutionResult result = task.await(Duration.ofMinutes(3));
// 필요하면 task.cancel()
```

## 공개 타입

| 타입 | 책임 |
|---|---|
| `TaskCageClient` | 데몬 연결, 작업 제출, capability 조회 |
| `TaskCageClientConfig` | UDS 경로, 연결 timeout, polling 간격 |
| `TaskSpec` | `ExternalCommand`와 `ResourceBudget`을 묶는 불변 작업 요청 |
| `ExternalCommand` | 실행 파일 절대 경로, 인자 배열, working directory, 환경 변수 |
| `ResourceBudget` | CPU·메모리·PID·벽시계 시간·출력 tail 제한 |
| `CpuQuota` | cgroup v2 `cpu.max`의 quota/period를 타입 안전하게 표현 |
| `Task` | `taskId`, 상태 조회, 완료 대기, 취소 |
| `TaskSnapshot` | 실행 중 또는 완료된 작업의 현재 상태 |
| `ExecutionResult` | 종료 원인, exit code/signal, 사용량, stdout/stderr tail |
| `TerminationReason` | `EXITED`, `TIMED_OUT`, `MEMORY_LIMIT_EXCEEDED` 등의 최종 원인 |
| `TaskCageCapabilities` | 데몬 버전, 지원 프로토콜, cgroup 준비 상태 |

`ExternalCommand`의 `program`과 `workingDirectory`는 모두 절대 경로이며 필수 값이다. SDK는 호출자의 현재 작업 디렉터리를 추정하거나 기본값으로 전송하지 않는다. 환경 변수는 명시적으로 제공한 항목만 데몬에 전달한다.

## 결과와 예외

외부 명령의 종료는 SDK 통신 오류와 구분한다.

- `TIMED_OUT`, `MEMORY_LIMIT_EXCEEDED`, `PROCESS_LIMIT_EXCEEDED`, `CANCELLED`, exit code 1은 `ExecutionResult`로 반환한다.
- UDS 연결 실패, 잘못된 프레임, 지원하지 않는 프로토콜은 `TaskCageException` 계열 예외로 처리한다.
- 실행 슬롯이 모두 사용 중인 `CAPACITY_EXHAUSTED`는 `TaskCageRejectedException`으로 표현한다.

`ExecutionResult.isSuccess()`는 `terminationReason == EXITED`이고 exit code가 0일 때만 `true`다.

## 코드 구조

```text
java-sdk/
├── README.md
├── build.gradle.kts
├── settings.gradle.kts
└── src/
    ├── main/java/io/github/taskcage/sdk/
    │   ├── TaskCageClient.java
    │   ├── TaskCageClientConfig.java
    │   ├── TaskSpec.java
    │   ├── ExternalCommand.java
    │   ├── ResourceBudget.java
    │   ├── CpuQuota.java
    │   ├── Task.java
    │   ├── TaskSnapshot.java
    │   ├── ExecutionResult.java
    │   ├── TerminationReason.java
    │   ├── TaskCageCapabilities.java
    │   ├── TaskCageException.java
    │   └── internal/
    │       ├── transport/
    │       ├── protocol/v1/
    │       └── client/
    └── test/java/io/github/taskcage/sdk/
```

`internal` 패키지의 UDS·JSON·프로토콜 DTO는 공개 API로 노출하지 않는다. Rust 데몬의 프로토콜 구현이 바뀌어도 Java 사용자가 보는 `TaskSpec`과 `ExecutionResult`를 안정적으로 유지하기 위해서다.

## MVP 구현 순서

1. Gradle Java 21 라이브러리와 공개 value type을 만든다.
2. length-prefixed UDS transport와 Protocol v1 JSON codec을 구현한다.
3. `capabilities()`와 `submit()`을 구현하고 protocol fixture로 직렬화·역직렬화를 검증한다.
4. `Task.status()`, `Task.await()`, `Task.cancel()`과 polling을 구현한다.
5. `execute()` 편의 API와 연결·거절·프로토콜 예외 모델을 추가한다.
6. Maven Central 배포 설정과 Gradle/Maven 사용 예제를 추가한다.

프로토콜의 정확한 요청·응답 필드는 [MVP API 명세](../docs/api-mvp.md)를 따른다.
