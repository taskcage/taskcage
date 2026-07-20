# TaskCage PRD

> 최종 제품 결정과 범위는 저장소 루트 [`README.md`](../README.md)를
> 따른다. 이 PRD는 해당 README를 구현 가능한 요구사항으로 구체화한다.

## 1. 제품 요약

| 항목 | 내용 |
|---|---|
| 제품명 | TaskCage |
| 한 문장 | 무거운 외부 프로그램을 작업별로 실행·제한·관찰·정리하는 Linux 관리 프로그램과 Java SDK |
| Linux 프로그램 | Rust 단일 `taskcaged` daemon |
| SDK | Java 21+, Spring Boot starter |
| 핵심 기반 | cgroup v2, systemd, Unix Domain Socket |
| 핵심 차별점 | 원자적 cgroup 진입, 전체 cgroup 정리, 커널 증거 기반 종료 원인 |
| 첫 환경 | Ubuntu LTS 한 버전, x86-64 |
| 팀과 일정 | 2명, 10주 MVP |
| 라이선스 | Apache-2.0 |

TaskCage는 PDF·OCR·이미지·영상 변환, 브라우저 자동화, 컴파일처럼
실행 시간과 자원 사용량을 예측하기 어려운 외부 프로그램을 안전하게
운영하기 위한 도구다. CPU, 메모리, 프로세스 수, 실행 시간과 전체
동시 실행 수를 제한하고, 종료 조건이 발생하면 작업 cgroup 전체를
정리해 Java 애플리케이션으로 결과를 돌려준다.

## 2. 문제 정의

Java의 timeout이나 thread interrupt는 호출자의 대기를 끝낼 수 있지만,
외부 프로그램과 그 자손의 자원 소비가 실제로 중단되었다는 보장은
아니다.

운영자는 다음 문제를 해결해야 한다.

- 작업 하나가 서버 CPU나 메모리를 독점한다.
- fork 폭주가 호스트의 process/thread 한도를 소진한다.
- 부모가 종료된 뒤에도 자식·손자 프로세스가 남는다.
- 여러 작업이 동시에 시작되어 정상 요청까지 실패한다.
- timeout, OOM, PID 제한과 일반 오류가 같은 exit code로 보인다.
- 실패 작업의 CPU 시간과 최대 메모리를 사후 확인하기 어렵다.

### 핵심 문제 문장

> JVM 백엔드 개발자는 외부 작업이 종료되거나 폭주했을 때 그 작업의
> 전체 프로세스 트리가 실제로 멈췄음을 확인하고, 종료 원인과 사용량을
> 일관된 결과로 받아야 한다.

## 3. 목표 사용자

### 문서·미디어 처리 백엔드 개발자

- FFmpeg, LibreOffice, ImageMagick, OCR 도구를 실행한다.
- 하나의 요청이 서버 전체 장애로 번지지 않게 하고 싶다.
- 작업마다 컨테이너나 Kubernetes Job을 만들고 싶지는 않다.

### 플랫폼·SRE 엔지니어

- 한 서버에서 여러 종류의 native 작업을 운영한다.
- 작업 단위 blast radius와 동시 실행 한도가 필요하다.
- 구조화된 종료 원인과 사용량을 로그·지표로 수집하려 한다.

### 비대상

- 외부 프로세스를 실행하지 않는 일반 CRUD 애플리케이션
- 작업마다 이미 별도 VM, Pod 또는 Job을 만드는 플랫폼
- Windows 또는 macOS 전용 환경
- 신뢰할 수 없는 코드를 완전히 격리해야 하는 서비스

## 4. 제품 원칙

1. **Fail closed:** 제한을 적용할 수 없으면 target을 실행하지 않는다.
2. **One job, one cgroup:** 모든 작업은 독립된 cgroup과 ID를 가진다.
3. **Atomic entry:** target은 생성 시점부터 job cgroup 안에 있어야 한다.
4. **Kernel evidence first:** 모호한 exit code보다 cgroup event를 우선한다.
5. **No shell:** executable과 argv를 분리하고 shell 문자열을 받지 않는다.
6. **Cleanup is completion:** `populated 0` 이전에는 작업 완료로 보지 않는다.
7. **Honest boundary:** TaskCage를 보안 샌드박스로 설명하지 않는다.
8. **README authority:** 활성 문서가 README와 충돌하면 README를 따른다.

## 5. MVP 목표

- Rust `taskcaged`를 systemd delegated service로 실행한다.
- Java SDK와 versioned length-prefixed JSON으로 통신한다.
- 요청마다 cgroup v2 leaf를 만들고 limit을 적용한다.
- `clone3(CLONE_INTO_CGROUP)`으로 target을 원자적으로 시작한다.
- CPU quota, memory, PID, wall time과 output size를 제한한다.
- 전역 동시 실행 수, bounded FIFO queue와 queue timeout을 제공한다.
- timeout, cancel, error와 limit 초과 시 `cgroup.kill`을 사용한다.
- `populated 0`을 확인한 뒤 cgroup과 scheduler permit을 정리한다.
- 커널 통계를 사용해 종료 원인과 사용량을 반환한다.
- Spring Boot starter와 실제 PDF 또는 OCR 예제를 제공한다.
- daemon 재시작 후 stale job cgroup을 정리한다.

## 6. 범위 밖

- 사용자 제공 소스나 임의 코드를 안전하게 실행하는 sandbox
- namespace 기반 filesystem·network 격리
- seccomp, AppArmor 또는 SELinux 정책 생성
- GPU와 network bandwidth 제한
- Windows와 macOS backend
- Kubernetes Job 생성기와 다중 호스트 scheduler
- CLI, Python SDK, 원격 agent와 웹 대시보드
- ARM64 및 여러 배포판의 최초 동시 지원
- daemon 재시작을 넘는 result replay

## 7. 사용자 경험

```java
ExecutionResult result = taskCage.execute(
    Command.of("pdftotext", "input.pdf", "output.txt"),
    ResourceBudget.builder()
        .timeout(Duration.ofMinutes(2))
        .cpuQuota(1.0)
        .memoryLimitMb(512)
        .processLimit(32)
        .build()
);

if (result.terminationReason() == TerminationReason.TIMEOUT) {
    // 재시도 또는 사용자 안내 정책
}
```

공개 타입과 enum은 Rust·Java 공용 protocol fixture와 함께 동결한다.

## 8. 기능 요구사항

### F1. 플랫폼 사전 검사

- Linux와 unified cgroup v2를 확인한다.
- `cpu`, `memory`, `pids` controller를 확인한다.
- systemd delegated root와 single-writer 조건을 확인한다.
- `clone3(CLONE_INTO_CGROUP)`과 `cgroup.kill`을 probe한다.
- 필요한 통계·event 파일과 쓰기 권한을 확인한다.
- 실패 시 target 생성 전에 `UNSUPPORTED`를 반환한다.

### F2. 로컬 프로토콜

- `/run/taskcage/taskcaged.sock`을 기본 socket으로 사용한다.
- four-byte big-endian length와 UTF-8 JSON payload를 사용한다.
- protocol version과 message type을 필수로 둔다.
- request/result frame 크기에 상한을 둔다.
- socket permission과 peer UID/GID를 검증한다.
- protocol v1은 연결 하나당 동기 작업 하나를 처리한다.

### F3. 명령과 정책 모델

- 명령은 executable과 argv 배열로 전달한다.
- NUL과 빈 executable을 거부한다.
- working directory와 환경 변수는 정책 검증 후 적용한다.
- resource budget은 모두 양수이며 daemon ceiling을 넘지 못한다.
- SDK 검증과 관계없이 daemon이 최종 권한을 가진다.

### F4. 작업 cgroup 생명주기

- 충돌하지 않는 job ID와 leaf를 만든다.
- limit을 쓴 뒤 read-back으로 적용을 확인한다.
- 실행 전 event baseline을 저장한다.
- terminal path마다 whole-cgroup cleanup을 시도한다.
- 빈 cgroup만 제거한다.
- daemon 시작 시 stale `job-*`을 scavenging한다.

### F5. 원자적 프로세스 시작

- target 생성 전에 argv, environment, working directory와 FD action을
  준비한다.
- job cgroup FD를 사용해 `clone3(CLONE_INTO_CGROUP)`을 호출한다.
- 가능하면 pidfd를 함께 확보한다.
- child path는 parent-death signal, FD 설치, `chdir`, `execve`만 수행한다.
- child path에서 heap allocation, lock과 structured logging을 금지한다.
- post-start `cgroup.procs` 이동 fallback을 제공하지 않는다.

### F6. 자원 제한

| 옵션 | 구현 | 의미 |
|---|---|---|
| CPU quota | `cpu.max` | 단위 시간당 CPU 상한 |
| memory | `memory.max` | 작업 트리 메모리 상한 |
| process count | `pids.max` | process/thread 생성 상한 |
| wall time | monotonic timer | 실제 경과 시간 상한 |
| output | bounded drain | stdout/stderr 저장량 상한 |

지원되는 환경에서는 `memory.oom.group=1`과 추가 peak 통계를 활용한다.

### F7. 동시 실행과 대기열

- 실제 running job 수는 `maxConcurrentJobs`를 넘지 않는다.
- 대기 작업은 bounded FIFO queue에 들어간다.
- queue full과 queue timeout을 서로 다른 결과로 반환한다.
- queued job은 cgroup이나 native process를 만들지 않는다.
- `queueTime`과 실제 `wallTime`을 분리한다.
- permit은 cleanup 이후 정확히 한 번 반환한다.

### F8. 출력과 취소

- stdout과 stderr를 동시에 읽어 pipe deadlock을 막는다.
- capture 상한과 truncation 여부를 결과에 포함한다.
- output watchdog이 terminal trigger가 될 수 있다.
- protocol v1에서 active socket close를 caller cancellation로 해석한다.

### F9. 전체 작업 종료

- 첫 terminal trigger를 기록하고 이후 trigger가 결과를 덮지 않게 한다.
- `cgroup.kill=1`로 job과 descendants를 종료한다.
- bounded timeout 안에 `cgroup.events`의 `populated 0`을 기다린다.
- final evidence를 읽고 cgroup을 제거한다.
- 비지 않는 cgroup은 cleanup failure로 명시한다.

### F10. 종료 원인 판정

판정 우선순위:

1. caller cancellation
2. queue capacity 또는 queue timeout
3. wall timeout
4. output limit
5. `memory.events.local` OOM delta
6. `pids.events` max delta
7. zero 또는 non-zero exit
8. signal 또는 unknown failure

### F11. 구조화된 결과

결과에는 최소한 다음을 포함한다.

- job ID, status와 termination reason
- exit code 또는 signal
- queue time과 wall time
- CPU time과 peak memory
- peak process count when supported
- bounded stdout/stderr와 truncation
- cleanup failure와 capability 정보

### F12. 복구

- systemd는 daemon failure 후 재시작한다.
- child에는 parent-death signal을 설정한다.
- 시작 시 stale populated job을 kill하고 empty 확인 후 제거한다.
- 재시작 이전 요청의 final result replay는 protocol v2 이후 범위다.

## 9. 비기능 요구사항

### 안정성

- malformed request가 daemon을 종료시키지 않는다.
- queue, process와 cgroup resource가 모든 terminal path에서 회수된다.
- daemon 내부 panic은 systemd restart와 startup scavenger로 복구한다.

### 보안 경계

- socket은 world-writable이면 안 된다.
- daemon은 canonical delegated root 밖에 쓰지 않는다.
- shell command와 caller-supplied cgroup path를 거부한다.
- TaskCage가 filesystem, network와 syscall을 격리한다고 주장하지 않는다.

### 관찰 가능성

- request ID와 job ID를 구조화 로그에 포함한다.
- cgroup create, start, terminal trigger, kill, empty와 removal을 기록한다.
- 민감할 수 있는 argv와 output은 기본 로그에 전체 출력하지 않는다.

### 배포

- 지원 환경용 Rust release binary와 checksum을 제공한다.
- systemd unit, 설치·제거·진단 절차를 제공한다.
- Java artifact와 Rust binary의 protocol version 호환성을 문서화한다.

## 10. 성공 기준

### 정량 기준

- ghost process cleanup 100/100
- 공식 termination-reason fixture 정확도 100%
- 혼합 job 1,000회 후 살아 있는 process와 stale cgroup 0개
- 동시 100요청에서 active-job 한도 위반과 permit 누수 0건
- daemon restart 20회 후 stale populated group 0개
- clean VM Quick Start 15분 이내
- 오프라인 5분 데모 3회 연속 성공

### 제출 산출물

- Rust `taskcaged` x86-64 Linux binary와 checksum
- Java API, client와 Spring Boot starter artifacts
- systemd unit과 설치·진단 scripts
- safe ghost, memory와 bounded-fork fixtures
- Linux integration, soak와 fault-injection tests
- README, PRD, architecture, protocol, ADR와 support 문서
- 라이브 데모, 발표 자료와 백업 영상

## 11. 주요 위험과 대응

| 위험 | 대응 |
|---|---|
| Rust `clone3` child path 구현 지연 | 1~2주차 Gate에서 최우선 spike, 실패 시 기능 확장 중지 |
| post-clone unsafe 동작 | parent에서 모든 데이터 준비, child path 최소화와 source review |
| cgroup 권한·systemd 차이 | 첫 Ubuntu 조합 고정, capability preflight와 fail closed |
| stdout/stderr deadlock | 두 stream 동시 drain과 bounded integration fixture |
| OOM·PID 오분류 | 실행 전후 kernel-event counter delta 사용 |
| cleanup 중 fork 경쟁 | `cgroup.kill`과 `populated 0` 반복 검증 |
| 2인 일정 초과 | Gate 기반 feature freeze와 후순위 기능 제거 |

## 12. 개발 계획

2인 역할, 주차별 목표, Gate와 축소 순서는
[`MVP-ROADMAP.md`](MVP-ROADMAP.md)를 따른다. 아키텍처 세부 불변식은
[`ARCHITECTURE.md`](ARCHITECTURE.md), wire contract는
[`PROTOCOL.md`](PROTOCOL.md)를 따른다.
