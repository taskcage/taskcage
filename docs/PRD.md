# TaskCage PRD

## 1. 문서 요약

| 항목 | 내용 |
|---|---|
| 제품명 | TaskCage |
| 한 문장 설명 | 예측하기 어려운 외부 프로그램을 작업별 cgroup v2로 격리·제한하고, 시간 초과나 오류 시 살아 있는 프로세스 트리를 자동 정리하는 Linux 실행 관리 도구와 Java SDK |
| 핵심 사용자 | FFmpeg, LibreOffice, ImageMagick, 압축 도구, AI 전처리기처럼 네이티브 프로그램을 실행하는 백엔드 개발자와 플랫폼 엔지니어 |
| 핵심 문제 | Java의 timeout과 interrupt는 호출자의 대기를 끝낼 뿐, 외부 프로세스와 그 자식이 실제로 자원 사용을 중단했다는 보장을 제공하지 않는다. |
| 핵심 차별점 | `clone3(CLONE_INTO_CGROUP)` 기반 원자적 시작, 작업별 cgroup v2 격리, `cgroup.kill` 기반 전체 프로세스 종료, 전역 동시 작업 제어, 커널 이벤트 기반 실패 원인 분류 |
| 제품 구성 | Go `taskcaged`, Rust `taskcage-launcher`, Java SDK, Spring Boot Starter |
| MVP 플랫폼 | Ubuntu LTS, Linux cgroup v2, x86-64, Go 1.25+, Rust stable, Java 21+, Spring Boot 3.x |
| 권장 개발 규모 | 2명, 10주 |
| 라이선스 | Apache-2.0 |

### OSS 대회 제출용 소개

TaskCage는 Linux에서 PDF 변환·OCR·이미지·영상 처리처럼 실행 시간과 자원 사용량을 예측하기 어려운 외부 프로그램을 안전하게 실행하고 정리하는 오픈소스 도구다. 작업마다 CPU·메모리·프로세스 수·실행 시간 한도를 적용하고, 전체 동시 실행 수를 제어한다. 한도 초과나 오류가 발생하면 해당 작업이 만든 프로세스 트리를 종료하고 종료 원인, CPU 사용 시간, 최대 메모리 등 운영에 필요한 결과를 Java SDK로 반환한다.

초기 지원 범위는 Ubuntu, cgroup v2, Go 기반 로컬 관리 daemon, Rust launcher, Java 21+, Spring Boot 3.x다. CLI, Python SDK, 원격 agent, 다중 사용자 정책, namespace·seccomp 기반 보안 격리는 후속 확장 범위로 둔다.

### 기대 효과

TaskCage는 폭주한 외부 프로그램의 영향을 해당 작업 안으로 제한해 서버 전체 장애로 번질 위험을 낮춘다. 작업별 자원 한도와 동시 실행 수를 함께 관리해 정상 작업의 실행 여력을 보호하며, 메모리 초과·시간 초과·프로세스 수 초과 같은 종료 원인과 사용량을 구조화해 장애 분석과 복구 시간을 줄인다. 또한 작업마다 컨테이너를 생성하거나 남은 프로세스를 수동으로 추적·정리하는 부담 없이 다양한 외부 프로그램에 동일한 실행 정책을 적용하게 한다.

### 이름 규칙

- 제품과 Java SDK 진입점은 `TaskCage`를 사용한다.
- Linux 관리 daemon은 `taskcaged`를 사용한다.
- Java 실행 결과와 상태는 `ExecutionResult`, `ExecutionStatus`를 사용한다.
- 설정·메트릭 prefix는 `taskcage`, 배포 파일과 모듈은 `taskcage-*`를 사용한다.
- Rust 실행 경계는 `taskcage-launcher`로 명명한다.
- 기존 이름인 `CageExecutor`는 새 문서와 공개 API에서 사용하지 않는다.

활성 구현 순서와 2인 역할 분담은 [`MVP-ROADMAP.md`](MVP-ROADMAP.md), 상세 구성은 [`ARCHITECTURE.md`](ARCHITECTURE.md)를 따른다.

## 2. 배경과 문제 정의

Spring 서버는 문서 변환, 영상 인코딩, 이미지 처리, 압축 해제, AI 모델 전처리 등의 작업을 위해 외부 프로세스를 실행한다.

```java
Process process = new ProcessBuilder("ffmpeg", "-i", input, output).start();
boolean completed = process.waitFor(10, TimeUnit.SECONDS);

if (!completed) {
    process.destroyForcibly();
}
```

이 코드는 단순해 보이지만 다음을 보장하지 못한다.

- `ffmpeg`가 이미 만든 자식 프로세스까지 모두 종료되는가?
- 부모가 먼저 종료되어 자식이 고아 프로세스가 되면 어떻게 찾는가?
- 네이티브 프로그램 하나가 Pod 메모리를 모두 사용하지 못하게 할 수 있는가?
- fork 폭주가 서버의 PID를 소진하지 못하게 할 수 있는가?
- timeout이 발생한 원인이 시간, 메모리, CPU, 프로세스 수 중 무엇인지 알 수 있는가?
- timeout된 작업이 뒤에서 계속 실행되어 정상 요청을 방해하지 않는가?

Java의 `Future.cancel(true)`는 실행 스레드에 interrupt를 시도한다. `ProcessHandle.descendants()`는 특정 시점의 자식 프로세스 스냅샷이며, 프로세스 생성과 종료는 비동기이므로 새로 생성되는 자식과 경쟁할 수 있다.

### 핵심 문제 문장

> 외부 프로그램을 실행하는 JVM 백엔드 개발자는 작업 하나가 timeout되거나 폭주했을 때, 그 작업과 모든 자식 프로세스의 자원 소비가 실제로 중단되었음을 보장하고 실패 원인을 확인할 수 있어야 한다.

### 사용자의 struggling moment

운영 중 PDF 변환 요청이 timeout되었다. API 로그에는 timeout만 남았지만 LibreOffice 자식 프로세스는 계속 살아 있다. 같은 요청이 반복되며 프로세스와 메모리가 누적되고, 결국 정상 요청을 처리하던 Pod 전체가 OOM으로 재시작된다. 개발자는 재시작 이후 어떤 요청이 원인이었는지 알 수 없다.

## 3. 목표 사용자

### Persona A: 미디어·문서 처리 백엔드 개발자

- FFmpeg, LibreOffice, ImageMagick 등을 `ProcessBuilder`로 실행한다.
- 고객이 업로드한 파일을 신뢰할 수 없다.
- 작업 하나가 서버 전체를 죽이지 않게 하고 싶다.
- 컨테이너 오케스트레이션까지 직접 구축하고 싶지는 않다.

### Persona B: 플러그인·자동화 플랫폼 개발자

- 사용자 플러그인이나 사내 도구를 별도 프로세스로 실행한다.
- 고객·작업 등급별 자원 할당량을 적용하고 싶다.
- timeout, OOM, fork 제한을 일관된 Java 결과로 받고 싶다.

### Persona C: 플랫폼·SRE 엔지니어

- 하나의 Pod에서 여러 종류의 작업이 함께 실행되는 환경을 운영한다.
- Pod 전체 제한이 아니라 작업 단위의 blast radius를 원한다.
- 작업 실패 원인을 Micrometer 지표와 로그로 수집하고 싶다.

### 비대상 사용자

- 일반적인 CRUD 요청만 처리하는 애플리케이션
- 외부 프로세스나 JNI를 사용하지 않는 순수 Java 작업
- 작업마다 이미 별도 Kubernetes Job·Pod를 생성하는 플랫폼
- Lambda처럼 실행 단위가 이미 강하게 격리된 환경
- Windows 전용 운영 환경

## 4. 제품 가치

### V1. 작업 하나의 실패를 작업 하나로 제한

하나의 변환 작업이 메모리나 PID를 과도하게 사용해도 Spring 서버와 다른 작업은 계속 실행된다.

### V2. timeout을 실제 종료 보장으로 전환

호출자가 기다리기를 중단하는 데서 끝나지 않고 해당 cgroup의 모든 프로세스를 종료한다.

### V3. 네이티브·자식 프로세스까지 포함한 자원 제한

Java heap이 아니라 Linux 프로세스 그룹 전체를 대상으로 메모리, CPU, PID를 제한한다.

### V4. 커널 증거 기반 실패 분류

단순한 exit code `137` 대신 `MEMORY_LIMIT_EXCEEDED`, `WALL_TIMEOUT`, `PID_LIMIT_REACHED`처럼 운영자가 행동할 수 있는 결과를 제공한다.

### V5. 하나의 Java API로 자원 정책 표준화

FFmpeg, LibreOffice, ImageMagick마다 반복 작성하던 timeout·kill·출력 제한·통계 수집 코드를 공통화한다.

## 5. 제품 원칙

1. **Fail closed**: 제한을 적용할 수 없는 플랫폼에서는 보호된 것처럼 실행하지 않고 `UNSUPPORTED` 또는 명시적 예외를 반환한다.
2. **Kernel evidence first**: 종료 이유는 exit code 추측보다 cgroup 이벤트를 우선한다.
3. **No shell by default**: 명령과 인자를 배열로 전달해 shell injection을 피한다.
4. **One job, one cgroup**: 모든 작업은 독립된 cgroup과 식별자를 가진다.
5. **Honest boundary**: TaskCage를 완전한 보안 샌드박스로 홍보하지 않는다.
6. **Cleanup is a feature**: 성공·실패·취소·호스트 재시작 상황의 정리를 핵심 기능으로 취급한다.

## 6. 목표와 제외 범위

### MVP 목표

- Go daemon을 systemd delegated service로 실행하고 Java SDK와 Unix Domain Socket으로 통신한다.
- 외부 명령을 Java API로 요청하고 Go daemon이 실행한다.
- daemon이 작업별 cgroup v2를 생성한다.
- Rust launcher를 `clone3(CLONE_INTO_CGROUP)`으로 작업 cgroup 안에 원자적으로 시작한다.
- 메모리, CPU rate, 프로세스 수, wall time, 출력 크기를 제한한다.
- 전체 동시 실행 수와 대기열 크기를 제한한다.
- timeout 또는 정책 위반 시 모든 자식 프로세스를 종료한다.
- 커널 통계를 읽어 종료 원인을 구조화된 결과로 반환한다.
- Spring Boot Auto-configuration과 Micrometer 지표를 제공한다.
- 정상 작업, 메모리 폭주, fork 폭주, 고아 프로세스 fixture를 제공한다.

### MVP에서 하지 않는 것

- 임의 Java lambda 또는 `Runnable`의 별도 JVM 실행
- 사용자 제공 소스 코드 컴파일·실행
- namespace 기반 파일시스템·네트워크 격리
- seccomp, AppArmor, SELinux 정책 생성
- GPU 자원 제한
- Windows·macOS 자원 제한 backend
- Kubernetes Job 생성기
- 다중 호스트 스케줄러와 작업 큐
- 웹 기반 운영 플랫폼

## 7. 사용자 경험

### 기본 실행

```java
ExecutionResult result = taskCage.run(
    Command.of("ffmpeg", "-i", inputPath, outputPath),
    ResourceBudget.builder()
        .memory("256MiB")
        .cpuRate(0.5)
        .maxProcesses(8)
        .wallTime(Duration.ofSeconds(10))
        .maxOutput("10MiB")
        .build()
);
```

### 결과 처리

```java
if (!result.isSuccess()) {
    log.warn(
        "job={} reason={} peakMemory={} cpuTime={} pidsPeak={}",
        result.jobId(),
        result.terminationReason(),
        result.peakMemory(),
        result.cpuTime(),
        result.processPeak()
    );
}
```

### Spring 설정

```yaml
taskcage:
  enabled: true
  cgroup-root: /sys/fs/cgroup/taskcage.slice
  cleanup-on-startup: true
  max-concurrent-jobs: 4
  queue-capacity: 32
  queue-timeout: 5s
  presets:
    video-convert:
      memory: 512MiB
      cpu-rate: 1.0
      processes: 16
      wall-time: 30s
      max-output: 10MiB
```

```java
ExecutionResult result = taskCage.run(
    Command.of("ffmpeg", "-i", input, output),
    ResourceBudget.preset("video-convert")
);
```

## 8. 기능 요구사항

### P0: 반드시 구현

#### F1. 플랫폼 사전 검사

- Linux 여부 확인
- cgroup v2 mount 확인
- 필요한 controller 확인: `memory`, `cpu`, `pids`
- 지정된 cgroup root의 생성·쓰기 권한 확인
- systemd delegated subtree와 single-writer 조건 확인
- Go `UseCgroupFD`를 통한 `clone3(CLONE_INTO_CGROUP)` probe
- `cgroup.kill`, `memory.peak` 등 선택 기능 지원 여부 표시
- 지원하지 않는 환경에서 fail-fast

#### F2. 명령 모델

- executable과 인자를 별도 필드로 저장
- working directory 지정
- 환경 변수 allowlist 또는 명시적 map
- stdin 정책: closed, bytes, file
- shell 실행은 별도 opt-in API로만 제공하거나 MVP에서 제외

#### F3. 작업별 cgroup lifecycle

- 충돌하지 않는 Job ID 생성
- 작업 cgroup 생성
- 제한 파일 쓰기와 재읽기 검증
- 프로세스 연결
- 작업 종료 후 빈 cgroup 제거
- daemon 시작 시 stale cgroup 정리

#### F4. 원자적 시작과 Rust launcher

- Go daemon은 제한이 적용된 job cgroup directory FD를 연다.
- `exec.Cmd.SysProcAttr.UseCgroupFD`와 `CgroupFD`를 사용해 Rust launcher를 처음부터 job cgroup 안에 생성한다.
- `PidFD`를 함께 사용해 PID 재사용 경쟁을 줄인다.
- Rust launcher는 parent-death signal을 설정하고 shell 문자열 조합 없이 argv를 보존해 target으로 `exec`한다.
- atomic start가 불가능하면 사후 PID 이동으로 fallback하지 않고 fail-closed 한다.
- launcher는 Linux x86-64 static musl binary와 checksum으로 배포한다.

#### F5. 자원 제한

| 사용자 옵션 | cgroup 또는 구현 방식 | 의미 |
|---|---|---|
| `memory` | `memory.max` | 작업 트리의 메모리 상한 |
| `swap` | `memory.swap.max` | swap 사용 상한 |
| `oomGroup` | `memory.oom.group=1` | OOM 시 작업을 하나의 단위로 처리 |
| `maxProcesses` | `pids.max` | task/thread 생성 상한 |
| `cpuRate` | `cpu.max` | 단위 시간당 CPU 사용량 제한 |
| `wallTime` | monotonic timer | 실제 경과 시간 초과 시 종료 |
| `maxOutput` | bounded stdout/stderr reader | 출력 폭주 시 종료 |

#### F6. 전체 작업 종료

- 지원되는 kernel에서는 `cgroup.kill=1` 사용
- 종료 후 `cgroup.events` 또는 `cgroup.procs`로 empty 상태 확인
- 일정 시간 내 비지 않으면 cleanup 오류를 별도로 기록
- 사용자 취소와 정책 위반을 구분

#### F7. 실행 결과 원인 분류

우선순위는 다음과 같다.

1. 명시적 사용자 취소
2. 실행 전 대기열 포화 또는 대기 시간 초과
3. wall time watchdog 발동
4. output limit watchdog 발동
5. `memory.events`의 `oom_kill` 또는 `oom_group_kill` 증가
6. `pids.events`의 `max` 증가
7. 정상 exit code
8. signal 또는 알 수 없는 비정상 종료

#### F8. stdout·stderr 처리

- 두 stream을 동시에 소비해 pipe deadlock 방지
- 각각 또는 합산 출력 크기 제한
- 결과에 truncation 여부 표시
- 비밀정보 노출을 줄이기 위한 최대 보존 크기 설정

#### F9. 구조화된 결과

```java
public record ExecutionResult(
    JobId jobId,
    ExecutionStatus status,
    TerminationReason terminationReason,
    Integer exitCode,
    Duration queueTime,
    Duration wallTime,
    Duration cpuTime,
    long peakMemoryBytes,
    int processPeak,
    CapturedOutput stdout,
    CapturedOutput stderr,
    CapabilityReport capabilities
) {}
```

#### F10. 동시 작업 수와 대기열 제어

- Go daemon 전체에서 실제 실행 작업 수가 `maxConcurrentJobs`를 넘지 않게 한다.
- 실행 슬롯이 없으면 크기가 제한된 FIFO 대기열에서 기다린다.
- 대기열이 가득 차면 `QUEUE_CAPACITY_EXCEEDED`, 대기 시간이 초과되면 `QUEUE_TIMEOUT`을 구조화된 결과로 반환한다.
- 대기 중인 작업은 cgroup과 native 프로세스를 생성하지 않는다.
- `wallTime`은 실행 슬롯 획득 후 측정하고, 대기 시간은 `queueTime`으로 별도 반환한다.
- 성공, 실패, 취소, 시작 실패, cleanup 실패를 포함한 모든 경로에서 실행 슬롯을 정확히 한 번 반납한다.

#### F11. Spring Boot Starter

- `TaskCage` Auto-configuration
- 설정 property validation
- Application shutdown hook
- Actuator health contributor
- Micrometer metrics

### P1: MVP 이후

- 비동기 `submit()`과 `ExecutionHandle.cancel()`
- protocol v2의 reconnectable cancel과 result replay
- 다중 사용자·원격 agent 정책
- ARM64 Rust launcher
- IO 제한
- 누적 CPU 시간 watchdog
- 작업별 파일시스템 디렉터리 관리
- 정책 preset hot reload
- OpenTelemetry span attributes

### P2: 장기 확장

- namespace·seccomp를 결합한 선택적 sandbox backend
- Kubernetes sidecar 또는 node agent
- GraalVM native client
- Quarkus·Micronaut integration
- 원격 TaskCage protocol

## 9. 실행 결과 상태 모델

```java
enum ExecutionStatus {
    SUCCEEDED,
    FAILED,
    KILLED,
    CANCELLED,
    REJECTED,
    UNSUPPORTED,
    INTERNAL_ERROR
}

enum TerminationReason {
    COMPLETED,
    NON_ZERO_EXIT,
    WALL_TIMEOUT,
    MEMORY_LIMIT_EXCEEDED,
    PID_LIMIT_REACHED,
    OUTPUT_LIMIT_EXCEEDED,
    QUEUE_CAPACITY_EXCEEDED,
    QUEUE_TIMEOUT,
    CANCELLED_BY_CALLER,
    PROCESS_SIGNALLED,
    BACKEND_UNAVAILABLE,
    UNKNOWN
}
```

### 의미상 주의사항

- `cpu.max`는 CPU 사용 속도를 throttle하며 자동 종료하지 않는다.
- MVP는 `cpu.stat`의 누적 CPU 사용 시간을 결과로 수집하지만 누적 CPU 시간 초과 종료는 하지 않는다.
- `pids.max`는 추가 fork·clone을 거부하지만 기존 프로세스를 자동 종료하지 않는다.
- `PID_LIMIT_REACHED` 정책에서는 이벤트 감지 후 TaskCage가 전체 작업을 종료한다.
- `memory.max`는 hard limit이지만 kernel 문서상 일시적인 초과 가능성을 고려해야 한다.
- exit code 하나만으로 OOM을 단정하지 않는다.

## 10. 기술 아키텍처

```text
Spring Application
    |
    | Unix Domain Socket
    v
TaskCage Java SDK
    |
    v
taskcaged (Go)
    |
    +--> Capability Detector
    +--> Admission Controller
    +--> Cgroup Manager
    +--> Output and Event Monitor
    +--> Termination Classifier
    |
    `--> clone3(CLONE_INTO_CGROUP)
                |
                v
       taskcage-launcher (Rust)
                `- exec target argv...
```

### 프로세스 실행 순서

1. Java SDK가 명령과 예산을 검증하고 UDS로 요청한다.
2. Go daemon이 peer credential, protocol, budget ceiling과 capability를 검증한다.
3. Admission Controller에서 실행 슬롯을 획득하거나 제한된 대기열에서 기다린다.
4. `job-{uuid}` cgroup을 생성하고 제한을 적용한 뒤 다시 읽어 검증한다.
5. 실행 전 kernel event baseline을 저장하고 job cgroup directory FD를 연다.
6. Go `UseCgroupFD`가 Rust launcher를 처음부터 job cgroup 안에 생성한다.
7. Rust launcher가 parent-death signal을 설정하고 target으로 `exec`한다.
8. output collector와 cgroup monitor가 동작한다.
9. 정상 종료 또는 정책 위반을 처리한다.
10. 커널 통계를 수집하고 원인을 분류한다.
11. cgroup이 비었는지 확인해 제거한 뒤 실행 슬롯을 반납한다.
12. Go daemon이 `ExecutionResult`를 Java SDK에 반환한다.

## 11. 권한과 배포 모델

### MVP: systemd delegated daemon 방식

```text
일반 권한 Spring 앱
    |
    | Unix Domain Socket
    v
taskcaged systemd service (Go)
    |
    +-- delegated cgroup subtree
    `-- taskcage-launcher (Rust) -> target
```

- Java 애플리케이션에서 cgroup 권한을 분리한다.
- systemd `Delegate=yes`가 daemon 전용 subtree를 제공한다.
- daemon은 자신의 PID를 `manager` child로 옮기고 sibling `jobs` subtree를 단독 관리한다.
- 허용 executable, 최대 budget, working root를 daemon 정책으로 제한한다.
- peer credential로 로컬 호출자를 식별한다.
- daemon이 죽으면 systemd가 재시작하고 startup scavenger가 stale job cgroup을 종료·정리한다.
- daemon은 resolved delegated root 바깥으로 이동하지 않고 traversal·symlink escape를 차단한다.

## 12. 유사 프로젝트와 차별점

| 프로젝트·기능 | 해결하는 문제 | TaskCage와의 차이 |
|---|---|---|
| [Java ProcessBuilder](https://docs.oracle.com/en/java/javase/17/docs/api/java.base/java/lang/ProcessBuilder.html) | 프로세스 시작과 표준 입출력 연결 | 작업별 CPU·메모리·PID 제한과 커널 실패 분류가 없다. |
| [Java ProcessHandle](https://docs.oracle.com/en/java/javase/21/core/methods-process-handle-class.html) | 프로세스·자식 조회와 종료 | 자식 목록은 변화할 수 있으며 cgroup 단위 종료·자원 제한이 아니다. |
| [Piston](https://github.com/engineer-man/piston) | 임의 소스 코드를 격리 실행하는 별도 서비스 | 코드 실행 플랫폼과 runtime package 관리가 중심이다. TaskCage는 기존 JVM 서비스가 실행하는 외부 명령을 위한 로컬 Linux daemon과 Java SDK다. |
| Docker·Kubernetes Job | 컨테이너·Pod 단위 자원 제한 | 작업마다 컨테이너를 생성하고 운영해야 한다. TaskCage는 기존 프로세스·Pod 내부의 짧은 작업 단위를 대상으로 한다. |
| `systemd-run` | transient service·scope와 자원 property 적용 | 운영 CLI·서비스 관리자이며 Java 작업 모델, 출력 수집, 종료 원인 분류, Spring integration을 제공하지 않는다. |

### 검색 기반 경쟁 밀도 판단

2026-07-12 기준 GitHub 저장소·코드 검색에서 `ExecutorService`와 유사한 Java API, 작업별 cgroup, Spring integration, kernel event 기반 종료 분류를 함께 제공하는 성숙한 범용 프로젝트는 확인하지 못했다. 다만 제출 전 Maven Central, GitHub, GitLab에서 재검색해야 하며 “유사 구현이 전혀 없다”는 표현은 사용하지 않는다.

### 차별점이 무너지는 조건

- `ProcessBuilder`에 timeout만 추가한 wrapper로 끝난다.
- 자식 프로세스를 `ProcessHandle.descendants()` 순회로만 죽인다.
- cgroup 제한은 걸지만 실패 원인을 exit code로만 추측한다.
- 시작 barrier 없이 프로세스를 먼저 실행한 후 cgroup에 옮긴다.
- Linux에서 실제 보장이 없는데 Windows에서도 동작하는 것처럼 fallback한다.
- 완전한 보안 sandbox라고 과장한다.

## 13. 관측성과 운영 기능

### Micrometer metrics

```text
taskcage.jobs.total{status,reason,command}
taskcage.jobs.active
taskcage.jobs.queued
taskcage.job.queue.duration
taskcage.job.wall.duration
taskcage.job.cpu.duration
taskcage.job.memory.peak
taskcage.job.processes.peak
taskcage.cleanup.failures.total
taskcage.capability.available{controller}
```

명령 전체와 파일 경로는 metric label에 넣지 않는다. cardinality와 비밀정보 노출을 막기 위해 사용자가 지정한 command alias만 허용한다.

### 구조화된 로그

```json
{
  "event": "taskcage.job.terminated",
  "jobId": "01J...",
  "commandAlias": "pdf-render",
  "status": "KILLED",
  "reason": "MEMORY_LIMIT_EXCEEDED",
  "wallTimeMs": 2341,
  "cpuTimeMs": 1818,
  "peakMemoryBytes": 268435456,
  "processPeak": 4
}
```

## 14. 심사 데모

### 데모의 핵심 주장

> 기존 timeout은 요청만 끝내고 자식 프로세스를 남길 수 있지만, TaskCage는 같은 작업을 kernel cgroup 단위로 종료하고 이유를 증명한다.

### Fixture 1: Ghost Process

`orphan-maker`가 메모리를 사용하는 자식 프로세스를 시작한 후 부모만 종료한다.

#### Plain ProcessBuilder

1. 부모 프로세스는 종료된다.
2. Java 작업은 끝난 것처럼 보인다.
3. 자식 프로세스는 계속 실행된다.
4. host 메모리와 프로세스 수가 유지된다.

#### TaskCage

1. 동일한 fixture를 작업 cgroup에서 실행한다.
2. timeout 또는 취소 시 `cgroup.kill`을 호출한다.
3. 자식까지 모두 사라진다.
4. `cgroup.events`가 empty 상태임을 보여준다.
5. `WALL_TIMEOUT`과 사용 통계를 반환한다.

### Fixture 2: Memory Hog

- 통제된 프로그램이 일정 단위로 메모리를 할당한다.
- plain 모드에서는 설정한 데모 안전선까지 증가한다.
- caged 모드에서는 `memory.max`에서 OOM이 발생한다.
- 결과가 `MEMORY_LIMIT_EXCEEDED`로 분류된다.

### Fixture 3: Safe Fork Storm

- 실제 shell fork bomb 대신 최대 500개까지만 생성하는 fixture를 사용한다.
- `pids.max=16`을 적용한다.
- host 전체 PID가 아니라 작업 내부 생성만 거부되는 모습을 보여준다.
- 정책에 따라 작업을 종료하고 `PID_LIMIT_REACHED`를 반환한다.

### 데모 화면

복잡한 제품 UI 대신 좌우 비교 한 화면을 사용한다.

```text
Plain ProcessBuilder              TaskCage
--------------------              ------------
API status: TIMEOUT               Job status: KILLED
living processes: 17              living processes: 0
memory: 620 MiB and rising        peak memory: 128 MiB
reason: UNKNOWN                   reason: MEMORY_LIMIT_EXCEEDED
server health: DEGRADED           server health: HEALTHY
```

### 5분 발표 흐름

1. 40초: timeout 후 프로세스가 남는 실제 문제 설명
2. 60초: plain 모드에서 Ghost Process 재현
3. 30초: 호출부를 `taskCage.run()`으로 교체
4. 60초: caged 모드에서 자식 전체 종료와 서버 생존 시연
5. 50초: atomic cgroup start, Rust launcher, kernel event 분류 설명
6. 40초: 유사 도구와 차이, 사용자, 한계 설명

## 15. 성공 지표와 인수 조건

### 기능 인수 조건

- 정상 fixture 100회가 동일한 출력과 exit code로 성공한다.
- wall timeout 후 대상 cgroup의 프로세스가 0개가 된다.
- 부모가 먼저 종료되는 fixture에서도 자식 프로세스가 남지 않는다.
- memory fixture가 `MEMORY_LIMIT_EXCEEDED`로 분류된다.
- fork fixture가 host PID를 소진하지 않고 `PID_LIMIT_REACHED`로 분류된다.
- 무한 출력 fixture가 설정 크기를 넘으면 종료된다.
- 동시에 100개 작업을 요청해도 실제 실행 수가 설정한 동시성 한도를 한 번도 넘지 않고 Job ID·cgroup 충돌이 없다.
- 대기열 포화와 대기 시간 초과가 각각 `QUEUE_CAPACITY_EXCEEDED`, `QUEUE_TIMEOUT`으로 분류된다.
- 1,000회 실행 후 stale cgroup 디렉터리가 남지 않는다.
- 제한 적용이 불가능한 환경에서 보호 없이 실행하지 않는다.

### 품질 지표

- 종료 원인 fixture 분류 정확도 100%
- 공식 지원 fixture의 재현 성공률 100%
- cleanup 누수 0건/1,000회
- pass-through 대비 시작 overhead를 측정하고 README에 공개
- 지원 kernel·systemd·JDK matrix 공개

### 사용자 검증 지표

- 외부 프로세스를 운영하는 백엔드 개발자 5명 이상 인터뷰
- 3명 이상이 현재 사용하는 workaround 코드나 장애 사례를 제공
- 2개 이상의 실제 오픈소스 Spring 프로젝트에 적용 spike
- 사용자가 15분 내 demo 프로젝트를 실행할 수 있음

## 16. 테스트 전략

### 단위 테스트

- memory·duration·CPU rate 파서
- cgroup key-value parser
- 종료 원인 우선순위
- command argv와 환경 변수 검증
- capability report
- 동시성 permit의 단일 반납과 FIFO 대기 순서

### Linux 통합 테스트

- 정상 종료
- non-zero exit
- wall timeout
- CPU time timeout
- memory OOM
- PID limit
- stdout·stderr limit
- caller cancellation
- orphan child cleanup
- 동시 fork 중 `cgroup.kill`
- 동시 실행 한도, 대기열 포화, 대기 시간 초과

### 장애 주입 테스트

- Rust launcher 시작 전·실행 중 daemon 종료
- `clone3(CLONE_INTO_CGROUP)` 시작 실패
- target 실행 중 애플리케이션 강제 종료
- 통계 파일 읽기 실패
- cleanup 도중 권한 제거
- PID 재사용과 stale metadata

### 개발 환경

- 일반 Java 개발은 Windows에서도 가능
- 실제 cgroup 통합 테스트는 WSL2 Ubuntu 또는 Linux VM에서 실행
- 최종 발표는 cgroup v2가 명확히 설정된 Ubuntu 환경 사용
- unsupported 환경용 mock backend는 테스트용으로만 제공하고 보호 기능으로 홍보하지 않음

## 17. 주요 위험과 대응

| 위험 | 심각도 | 대응 |
|---|---:|---|
| cgroup 생성 권한 때문에 사용자가 시작하지 못함 | 매우 높음 | 명확한 preflight, systemd 설치 스크립트, delegated subtree 예제 제공 |
| 프로세스 시작 후 attach 전 자식이 생성되는 경쟁 조건 | 매우 높음 | Go `UseCgroupFD`와 `CLONE_INTO_CGROUP`으로 target을 원자적으로 job cgroup에서 시작 |
| 보안 sandbox로 오해 | 매우 높음 | README 첫 화면에 resource isolation과 security isolation의 차이 명시 |
| kernel·배포판별 cgroup 기능 차이 | 높음 | capability detection, 기능별 지원 matrix, 없는 기능은 fail-fast 또는 명시적 degraded 상태 |
| OOM·signal 원인 오분류 | 높음 | 실행 전후 event delta와 watchdog state를 함께 사용하고 불확실하면 `UNKNOWN` 반환 |
| 애플리케이션 crash 후 작업·cgroup 누수 | 높음 | startup scavenger, owner metadata, stale TTL, agent roadmap |
| stdout·stderr 미소비로 deadlock | 높음 | 두 stream을 동시에 bounded drain |
| 취소·오류 경로에서 실행 슬롯 누수 | 높음 | permit 소유권을 단일 lifecycle 객체로 관리하고 반복·장애 주입 테스트 수행 |
| Kubernetes 보안 정책이 cgroup 위임을 금지 | 높음 | 별도 agent·sidecar 모델을 P1로 제공하고 Kubernetes Job이 더 적합한 환경을 문서화 |
| Go·Rust·Java 세 toolchain의 배포 부담 | 중간 | 고정 toolchain matrix, reproducible build, checksum, x86-64부터 제한 지원 |
| 짧은 작업에서 overhead가 큼 | 중간 | benchmark 공개, 최소 작업 시간 가이드 제공 |
| Piston·container wrapper로 보임 | 중간 | 로컬 daemon·atomic job cgroup start·kernel evidence classification 데모를 전면 배치 |

## 18. 2주 기술 스파이크

### 반드시 증명할 것

1. Go daemon이 systemd delegated subtree에서 작업 cgroup을 만들고 메모리·PID 제한을 적용한다.
2. Go가 Rust launcher를 `CLONE_INTO_CGROUP`으로 job cgroup 안에 원자적으로 생성한다.
3. 부모가 먼저 종료된 후에도 `cgroup.kill`로 자식 전체가 종료된다.
4. `memory.events`로 OOM을 정확히 구분한다.
5. 동일 fixture를 100회 반복해 프로세스와 cgroup 누수가 없다.

### 스파이크 산출물

- `TaskCage.run()` 최소 API
- Go `taskcaged` 최소 daemon과 UDS protocol
- Rust x86-64 Linux launcher
- Ghost Process fixture
- Memory Hog fixture
- 실행 전후 `ps`, cgroup stats, JSON 결과
- 지원·미지원 조건 문서
- 3분짜리 비교 데모

### 범위 축소 기준

다음 중 하나라도 2주 안에 해결하지 못하면 부가 기능을 줄이고 핵심 보장부터 다시 검증한다.

- 실제 명령 실행 전 안전하게 cgroup에 넣을 수 없다.
- 자식 프로세스가 반복 테스트에서 한 번이라도 남는다.
- OOM과 일반 `SIGKILL`을 fixture에서 구분하지 못한다.
- 일반 사용자 설치 절차를 재현 가능한 문서로 만들지 못한다.
- 핵심 결과가 `systemd-run` 명령 wrapper 이상의 차이를 보이지 못한다.

## 19. 개발 계획

| 주차 | 목표 |
|---|---|
| 1주 | UDS protocol, Java API, Go preflight, Rust exec smoke |
| 2주 | delegated cgroup lifecycle, atomic launcher start, Ghost Process Gate |
| 3주 | memory·PID·CPU rate 제한, kernel event delta와 결과 분류 |
| 4주 | wall time, output limit, caller cancellation, `cgroup.kill` |
| 5주 | global concurrency, bounded queue, CPU·memory·PID 통계 |
| 6주 | systemd packaging, startup recovery, Spring Boot Starter |
| 7주 | 장애 주입, 1,000회 soak, feature freeze |
| 8주 | PDF·OCR 예제, benchmark, 5분 비교 데모 |
| 9주 | OSS 문서, reproducible Go·Rust·Java release candidate |
| 10주 | 제출 안정화, clean VM 검증, 발표 리허설과 final release |

### 2인 팀 역할

- A: Go daemon, Rust launcher, cgroup·systemd, Linux 통합 테스트와 packaging
- B: Java SDK, Spring Boot Starter, protocol contract, 예제·문서·발표

공통으로 protocol, 종료 의미, atomic start와 cleanup 결과를 교차 리뷰한다. 상세 일정은 [`MVP-ROADMAP.md`](MVP-ROADMAP.md)를 따른다.

## 20. 오픈소스 배포 전략

- Maven Central: `taskcage-api`, `taskcage-client`, `taskcage-spring-boot-starter`
- GitHub Release: Go `taskcaged`, Rust `taskcage-launcher`, SHA-256 checksums
- `examples/ffmpeg-service`, `examples/libreoffice-service`
- 지원 kernel·JDK·배포판 matrix
- threat model과 비보장 항목 문서
- benchmark 방법과 raw result 공개
- Good First Issue: 새로운 fixture, 배포판 검증, ARM64 build
- 핵심 cgroup semantics마다 Linux kernel 공식 문서 링크와 conformance test 연결

## 21. 참고 자료

- [Linux Control Group v2 공식 문서](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html)
- [Java ProcessBuilder](https://docs.oracle.com/en/java/javase/17/docs/api/java.base/java/lang/ProcessBuilder.html)
- [Java Process와 descendants](https://docs.oracle.com/en/java/javase/16/docs/api/java.base/java/lang/Process.html#descendants())
- [Java Unix Domain Socket](https://docs.oracle.com/en/java/javase/17/core/java-nio.html)
- [Piston](https://github.com/engineer-man/piston)

## 22. 최종 판단

TaskCage의 성공 여부는 지원 기능 개수보다 한 가지 보장을 얼마나 확실하게 증명하느냐에 달려 있다.

> timeout된 외부 작업과 그 자식 프로세스가 실제로 모두 사라지고, Spring 서버는 살아 있으며, 종료 이유를 kernel 증거로 설명할 수 있다.

OSS 대회 제출에서는 기능 수보다 이 보장의 반복 재현, 명확한 비보장 범위, 설치 재현성, 실제 운영자가 활용할 수 있는 결과를 앞세운다. 2주 스파이크에서 보장이 흔들리면 부가 기능을 줄이고 자식 프로세스 정리와 커널 증거 기반 원인 분류를 먼저 완성한다.
