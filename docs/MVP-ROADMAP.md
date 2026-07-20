# TaskCage OSS 대회 MVP 로드맵 — Rust·Java, 2인 팀

> 최종 제품 기준은 저장소 루트 [`README.md`](../README.md)다. 이 문서는
> Rust 단일 `taskcaged` 결정에 따른 10주 실행 계획이다.

## 1. 전제

| 항목 | 최종 기준 |
|---|---|
| 팀 | 2명 |
| Linux 관리 프로그램 | Rust 단일 `taskcaged` daemon |
| 애플리케이션 연동 | Java 21 SDK와 Spring Boot starter |
| 통신 | Unix Domain Socket, versioned length-prefixed JSON |
| 프로세스 시작 | `clone3(CLONE_INTO_CGROUP)` |
| 정리 완료 | `cgroup.kill` 후 `populated 0` 확인 |
| 권한 | systemd `Delegate=yes`, 단일 로컬 애플리케이션 계정 |
| 첫 지원 환경 | Ubuntu LTS 한 버전, x86-64, cgroup v2 |
| 핵심 데모 | plain ProcessBuilder와 TaskCage의 ghost·memory·PID 폭주 비교 |

### 대회에서 증명할 한 문장

> TaskCage는 외부 프로그램을 처음부터 제한된 cgroup 안에서 실행하고,
> 종료 조건이 발생하면 전체 프로세스 트리를 정리한 뒤 커널 증거와
> 사용량을 Java 애플리케이션에 반환한다.

## 2. 역할 분담

### 팀원 A — Rust·Linux owner

- Rust daemon lifecycle과 configuration
- UDS server, frame codec과 peer credential
- systemd delegated cgroup subtree와 preflight
- cgroup 생성, limit, statistics, event parser와 cleanup
- `clone3(CLONE_INTO_CGROUP)` 기반 atomic executor
- bounded output drain, wall timer와 termination state machine
- global concurrency, FIFO queue와 queue timeout
- daemon restart와 stale-cgroup recovery
- Linux integration, soak, fault-injection test와 packaging

### 팀원 B — Java·Spring·demo owner

- `Command`, `ResourceBudget`, `ExecutionResult` 공개 API
- Java 21 Unix Domain Socket client와 frame codec
- Rust·Java 공용 JSON fixture vector
- cancellation, daemon-unavailable과 protocol-error UX
- Spring Boot auto-configuration과 properties
- PDF 또는 OCR 예제 서비스
- README Quick Start, API 문서, 발표와 백업 데모

### 공동 책임

- protocol과 공개 enum 변경은 두 명이 승인한다.
- 팀원 A는 Java 예제를, 팀원 B는 clean-VM 설치를 직접 검증한다.
- 매주 main에서 핵심 fixture와 5분 데모를 실행한다.
- 기능 구현과 별도로 매주 최소 하루를 통합·문서·리뷰에 사용한다.

## 3. P0 범위

| 영역 | 완료 증거 |
|---|---|
| preflight | 미지원 환경에서 target 실행 0건 |
| atomic start | target의 cgroup 외부 실행 0건 |
| memory | OOM fixture가 `MEMORY_LIMIT_EXCEEDED` |
| PID | bounded fork fixture가 `PID_LIMIT_REACHED` |
| CPU | `cpu.max` 적용과 CPU 사용량 반환 |
| wall time | monotonic timeout과 whole-cgroup cleanup |
| output | stdout/stderr deadlock 없이 상한 적용 |
| cleanup | 생존 프로세스·stale cgroup 0개 |
| concurrency | 설정한 active-job 한도 위반 0건 |
| result | Rust·Java fixture가 동일한 status/reason을 해석 |
| SDK | 최소 동기 `execute` 호출 성공 |
| Spring | 설정 검증과 예제 서비스 실행 |
| OSS | 처음 보는 사용자의 15분 Quick Start |

후속 범위는 CLI, Python, 원격 agent, 다중 사용자 정책, namespace,
seccomp, ARM64, 다중 배포판, 웹 대시보드다.

## 4. 10주 실행 계획

| 주차 | 공동 목표 | 팀원 A: Rust·Linux | 팀원 B: Java·Demo | 통과 조건 |
|---|---|---|---|---|
| 1 | contract와 환경 고정 | delegated subtree·cgroup file spike, UDS/frame skeleton, `clone3` 조사 | API record·enum, Java UDS/frame spike, Gradle 정리 | Rust daemon과 Java client가 fixture JSON 왕복 |
| 2 | atomic start와 cleanup 증명 | `clone3(CLONE_INTO_CGROUP)`, child `execve`, `cgroup.kill`, ghost fixture | 반복 실행 harness와 결과 모델 | 100회에서 cgroup 외부 실행·생존 프로세스·stale group 0건 |
| 3 | 자원 제한과 분류 | memory·PID·CPU limit, event baseline/delta | budget 검증, enum mapping, golden vectors | normal/non-zero/OOM/PID 결과 정확도 100% |
| 4 | timeout·output·cancel | monotonic timer, 두 stream drain, disconnect cancel, lifecycle | socket-close cancel, output decoding, 예외 UX | 500회 혼합 실행에서 deadlock·누수 0건 |
| 5 | admission control | bounded FIFO, max active, queue timeout, permit ownership | Java concurrency test와 queue result | 동시 100요청에서 active 한도·permit 위반 0건 |
| 6 | 설치 가능한 제품 | preflight, `SO_PEERCRED`, scavenger, systemd, release build | Spring starter, properties, 예제 API | clean VM 설치부터 정상 실행까지 15분 이내 |
| 7 | 기능 동결·복구 | daemon crash, clone/exec/stat/cleanup fault injection, soak | compatibility와 unavailable UX | 신규 P0 동결, 1,000회 process·cgroup 누수 0건 |
| 8 | 실제 PDF/OCR 데모 | 실제 도구 자식 구조, resource statistics, overhead 측정 | plain/TaskCage 비교 서비스와 5분 데모 | 네트워크 없이 문제와 격리 성공 재현 |
| 9 | release candidate | reproducible Rust build, checksum, support/threat 문서 | Javadoc, Java artifact, 발표·기여 문서 | 제3자 Quick Start와 `v0.1.0-rc1` 생성 |
| 10 | 제출 안정화 | 설치 재검증과 치명적 버그만 수정 | 대본, FAQ, 백업 영상과 교차 리허설 | 오프라인 5분 데모 3회 연속 성공 |

## 5. 단계별 Gate

### Gate 1 — 2주차: 기술 성립

- daemon이 systemd 위임 subtree를 올바르게 구성한다.
- Rust가 target을 처음부터 job cgroup에 생성한다.
- child path가 shell 없이 argv를 보존해 `execve`한다.
- `cgroup.kill` 후 `populated 0`을 확인한다.
- ghost fixture 100회에서 생존 process와 stale group이 없다.

실패하면 Spring 편의 기능을 늘리지 않고 atomic start와 cleanup을 먼저
해결한다.

### Gate 2 — 6주차: 기능 완성

- memory, CPU, PID, wall time과 output 제한이 동작한다.
- kernel-event delta로 memory와 PID 원인을 판정한다.
- bounded queue와 concurrency 제한이 동작한다.
- Java와 Spring이 실제 daemon을 호출한다.
- clean VM 설치가 15분 안에 완료된다.

### Gate 3 — 8주차: 신뢰성

- ghost cleanup 100/100
- 공식 reason fixture 정확도 100%
- 혼합 job 1,000회 후 process·cgroup·permit 누수 0건
- 동시 100요청에서 active 한도 위반 0건
- daemon restart 후 stale populated group 0개
- 실제 PDF 또는 OCR 도구로 핵심 효과 재현

### Gate 4 — 10주차: 제출 가능

- 처음 보는 사용자가 15분 안에 설치와 예제 실행
- 오프라인 5분 데모 3회 연속 성공
- Rust binary, Java artifacts, checksum과 tag 일치
- README와 발표의 지원·비보장 범위 일치
- 두 사람 모두 설치, release, 발표와 복구 수행 가능

## 6. Rust owner 구현 순서

### 1단계: delegated root

1. `/proc/self/cgroup` parser
2. canonical delegated-root resolver
3. `manager` self-move와 `jobs` internal node
4. controller enable과 capability report
5. stale `job-*` startup scavenger

### 2단계: atomic executor

1. job leaf와 limit read-back
2. event baseline snapshot
3. argv/env/pipe/error-channel 사전 준비
4. `clone3(CLONE_INTO_CGROUP)`와 pidfd
5. 최소 child path와 shell-free `execve`
6. parent monitor와 exec-error 전달

### 3단계: lifecycle

1. stdout/stderr 동시 drain
2. wall timer와 caller disconnect
3. first-trigger-wins termination
4. `cgroup.kill`과 empty wait
5. final evidence, classification, removal과 permit 반환

### 4단계: daemon productization

1. UDS와 peer credential
2. request·budget ceiling validation
3. scheduler와 bounded queue
4. structured tracing
5. graceful shutdown과 crash recovery
6. systemd install·diagnostic·release scripts

## 7. 테스트 매트릭스

### Rust unit

- cgroup flat-key parser와 unknown key
- budget와 canonical path validation
- event delta와 reason priority
- FIFO, timeout, cancellation과 permit 단일 반환
- frame length, malformed JSON과 protocol mismatch
- child setup-plan 검증

### Rust·Java contract

- 모든 status와 reason enum
- unknown additive field와 unknown enum
- oversized frame와 invalid UTF-8
- base64 stdout/stderr와 truncation

### Linux integration

- normal, non-zero와 exec failure
- wall timeout, memory OOM, PID와 CPU limit
- output limit와 pipe deadlock
- disconnect cancellation과 orphan cleanup
- concurrent fork during `cgroup.kill`
- queue full과 queue timeout
- daemon SIGKILL과 restart scavenger
- permission과 cleanup failure

## 8. 일정이 밀릴 때

절대 줄이지 않는다:

1. atomic cgroup entry
2. memory·PID·wall limit
3. whole-cgroup kill과 empty verification
4. kernel evidence 기반 reason
5. fail-closed preflight
6. concurrency와 누수 테스트
7. Java SDK 최소 동기 호출

먼저 줄인다:

1. Micrometer 지표 종류
2. preset 수와 설정 편의 기능
3. 두 번째 실사용 예제
4. 데모 UI 효과
5. Ubuntu·architecture matrix
6. 추가 benchmark와 진단 명령

## 9. 첫 주 backlog

### 공동

- 공식 Ubuntu LTS·kernel·Rust·JDK 조합 고정
- protocol v1 frame과 JSON fixture review
- `ExecutionStatus`와 `TerminationReason` 초안 고정
- Gate 1 fixture와 정량 통과 조건 합의

### 팀원 A

- systemd delegated subtree 수동 실험
- `/proc/self/cgroup`와 capability probe
- `clone3(CLONE_INTO_CGROUP)` 최소 spike
- prepared argv/env로 child `execve` smoke test
- `cgroup.kill` 후 `populated 0` 확인

### 팀원 B

- Java API record와 enum 초안
- Java 21 UDS 연결과 four-byte frame codec
- daemon unavailable과 protocol mismatch UX
- Spring 예제 골격과 golden-vector test

첫 주 완료 조건은 Rust daemon과 Java client가 UDS로 동일 fixture를
교환하고, Rust executor spike가 target을 준비된 cgroup 안에서 실행하는
것이다.
