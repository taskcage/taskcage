# TaskCage OSS 대회 MVP 로드맵 — Go·Rust·Java, 2인 팀

## 1. 로드맵 전제

대회 제출일을 기준으로 10주를 역산한다. 실제 남은 기간이 다르더라도 단계 순서와 Gate 통과 조건은 유지한다.

| 항목 | 결정 |
|---|---|
| 팀 구성 | 2명 |
| Linux 관리 프로그램 | Go `taskcaged` daemon |
| 실행 경계 | Rust `taskcage-launcher` |
| 애플리케이션 연동 | Java 21 SDK와 Spring Boot Starter |
| 통신 | 로컬 Unix Domain Socket, versioned length-prefixed JSON |
| 프로세스 시작 | Go `UseCgroupFD`를 통한 `clone3(CLONE_INTO_CGROUP)` |
| 권한 모델 | systemd `Delegate=yes`, 단일 애플리케이션 계정 |
| MVP 환경 | 한 가지 Ubuntu LTS x86-64, cgroup v2 |
| 핵심 데모 | Plain ProcessBuilder 대 TaskCage의 Ghost Process·Memory Hog 비교 |

### 대회에서 증명할 한 문장

> TaskCage는 target을 처음부터 제한된 cgroup 안에서 실행하고, timeout이나 자원 위반이 발생하면 살아 있는 프로세스 트리를 모두 제거한 뒤 커널 증거와 사용량을 Java 애플리케이션에 반환한다.

## 2. 2인 역할 분담

### 팀원 A — Linux owner: Go·Rust·cgroup·systemd

이 역할은 사용자가 담당하는 것을 기본으로 한다.

- Go daemon lifecycle과 configuration
- systemd delegated subtree와 startup preflight
- cgroup v2 생성·제한·통계·event parser·cleanup
- `clone3(CLONE_INTO_CGROUP)` 기반 atomic start
- Rust launcher와 static release binary
- global concurrency, bounded queue, queue timeout
- stdout·stderr bounded collector와 wall-time watchdog
- termination classifier
- daemon restart와 stale cgroup recovery
- Ghost Process, Memory Hog, Safe Fork Storm fixture
- Linux integration·soak·fault-injection test
- 설치 스크립트와 release packaging

### 팀원 B — Application owner: Java SDK·Spring·데모

- `Command`, `ResourceBudget`, `ExecutionResult` 공개 API
- Java 21 Unix Domain Socket client
- protocol framing, validation, timeout, cancellation
- Go·Java 공용 protocol fixture vector
- Spring Boot Auto-configuration과 property validation
- preset과 최소 Micrometer 지표
- PDF 또는 OCR 예제 서비스
- README Quick Start와 사용자 문서
- 비교 데모 화면, 발표 자료, FAQ
- Maven artifact와 Java compatibility test

### 공동 책임

- protocol과 공개 API 변경은 두 명이 함께 승인한다.
- 팀원 A가 Java SDK 예제를 직접 실행하고, 팀원 B가 Linux Quick Start를 clean VM에서 직접 실행한다.
- 핵심 보장 변경은 작성자가 아닌 사람이 테스트 결과를 확인한다.
- 매주 금요일 main branch에서 전체 데모를 실행한다.
- 한 사람이 자리를 비워도 상대가 release와 데모를 수행할 수 있게 운영 절차를 문서화한다.

## 3. MVP 범위

### P0: 반드시 구현

| 영역 | 기능 | 증거 |
|---|---|---|
| daemon | systemd supervision, UDS, peer credential, preflight | 지원하지 않는 환경에서 target 미실행 |
| atomic start | Go `UseCgroupFD`로 Rust launcher를 job cgroup에 생성 | target이 cgroup 밖에서 실행된 순간 0건 |
| launcher | parent-death signal, argv 보존, shell-free exec | 공백·특수문자가 있는 argv fixture 통과 |
| memory | `memory.max`, `memory.oom.group`, OOM event delta | `MEMORY_LIMIT_EXCEEDED` |
| CPU | `cpu.max`, `cpu.stat` 수집 | CPU rate 제한과 사용 시간 반환 |
| PID | `pids.max`, `pids.events` delta | `PID_LIMIT_REACHED` |
| time | monotonic wall timer | `WALL_TIMEOUT` |
| output | stdout·stderr 동시 drain과 크기 제한 | deadlock 없이 `OUTPUT_LIMIT_EXCEEDED` |
| cleanup | `cgroup.kill`, `populated 0`, 디렉터리 삭제 | orphan process와 stale cgroup 0개 |
| concurrency | global max active jobs, bounded FIFO queue | 100개 요청에서도 설정 한도 미초과 |
| result | status, reason, exit, queue/wall/CPU, peak memory/PID, bounded output | Go·Java 동일 JSON vector |
| SDK | 동기 `TaskCage.run()` | 최소 코드로 ProcessBuilder 교체 |
| Spring | auto-configuration, YAML properties, preset | 예제 서비스 실행 |
| OSS | 설치·아키텍처·보안 경계·기여·release 문서 | 제3자 15분 Quick Start |

### P1: 대회 이후

- CLI와 Python SDK
- protocol v2의 reconnectable cancel과 result replay
- 다중 사용자·다중 애플리케이션 정책
- TCP·원격 agent
- ARM64와 여러 배포판 지원
- namespace, seccomp, AppArmor 기반 선택적 security sandbox
- IO·GPU·네트워크 제한
- CPU 누적 시간 watchdog
- OpenTelemetry와 정책 hot reload
- 웹 대시보드

### 의도적으로 사용하지 않는 것

- target 시작 후 `cgroup.procs`로 옮기는 fallback
- target 실행을 위한 shell 문자열
- `ProcessHandle.descendants()` 기반 cleanup
- `systemd-run`을 job backend로 사용하는 wrapper
- gRPC, 웹 framework, 복잡한 config framework
- Docker/Testcontainers 결과만으로 핵심 cgroup 보장을 주장하는 테스트

## 4. 10주 실행 계획

| 주차 | 공동 목표 | 팀원 A: Go·Rust·Linux | 팀원 B: Java·Spring·데모 | 주간 통과 조건 |
|---|---|---|---|---|
| 1주차 | contract와 개발 환경 고정 | daemon flag·preflight 골격, delegated subtree 수동 실험, Rust launcher exec smoke | API record·enum 초안, UDS 연결·framing spike, Gradle module 정리 | Go daemon과 Java client가 UDS로 fixture JSON 왕복, Rust launcher가 argv를 보존해 target 실행 |
| 2주차 | 가장 위험한 atomic start·cleanup 증명 | cgroup create/limit/open FD, `UseCgroupFD`, `PidFD`, Rust launcher, `cgroup.kill`, Ghost fixture | Java test harness, 반복 실행 결과 수집, failure result 모델 | Ghost Process 100회에서 target 외부 실행 0건, 생존 프로세스 0개, stale cgroup 0개 |
| 3주차 | memory·PID·CPU rate 제한 | memory/pids/cpu 파일 적용·재검증, event baseline/delta, safe fixtures | budget validation, status/reason mapping, protocol golden vectors | 정상·non-zero·memory·PID 결과가 100% 기대 값, CPU rate 적용 확인 |
| 4주차 | timeout·output·cancel과 lifecycle 완성 | monotonic timer, stdout/stderr concurrent bounded drain, disconnect cancel, cleanup state machine | SDK timeout·socket-close cancel, output decoding, 예외 mapping | wall/output/cancel fixture 통과, 500회 혼합 실행에서 deadlock·프로세스·cgroup 누수 0건 |
| 5주차 | global admission control | bounded FIFO queue, max active jobs, queue timeout, permit 단일 반납 | Java concurrency test, queue result 처리, 기본 metrics mapping | 100개 동시 요청에서 active limit 위반 0건, queue 두 종료 원인 정확, permit 누수 0건 |
| 6주차 | 설치 가능한 제품 | daemon config, `SO_PEERCRED`, startup scavenger, systemd unit, Go/Rust packaging | Spring Boot Starter, property validation, preset, 예제 API | clean VM에서 systemd 설치부터 Spring 정상 실행까지 15분 이내 |
| 7주차 | 기능 동결과 장애 복구 | daemon crash·launcher failure·통계 읽기 실패·cleanup permission fault injection, 1,000회 soak | protocol 호환성·daemon unavailable·재시작 UX, 사용자 오류 문구 | feature freeze, 1,000회 누수 0건, 공식 fixture 원인 분류 100% |
| 8주차 | 실제 PDF/OCR 데모와 benchmark | 실제 도구 자식 구조 확인, resource statistics, startup overhead와 raw benchmark | Plain/TaskCage 비교 서비스와 5분 화면, 사용성 테스트 | 동일 입력으로 plain 문제와 TaskCage 격리 성공을 네트워크 없이 재현 |
| 9주차 | OSS release candidate | reproducible Go/Rust build, static launcher, checksum, Linux matrix·threat model | README, API guide, Javadoc, Maven RC, 발표 자료·기여 가이드 | 제3자가 Quick Start 성공, `v0.1.0-rc1` source와 artifacts 생성 |
| 10주차 | 제출 안정화 | 치명적 버그만 수정, package install 검증, 오프라인 환경과 백업 영상 | 발표 대본·FAQ·최종 문서, 서로 역할을 바꾼 리허설 | 최종 release 고정, README 명령 재검증, 5분 데모 3회 연속 성공 |

## 5. 단계별 Gate

### Gate 1 — 기술 성립: 2주차 종료

- systemd가 위임한 subtree 안에서 daemon과 job leaf가 올바르게 분리된다.
- Go가 Rust launcher를 처음부터 job cgroup에 생성한다.
- Rust launcher가 shell 없이 argv를 보존해 target으로 `exec`한다.
- 부모가 먼저 종료되는 fixture도 `cgroup.kill` 후 `populated 0`이다.
- 위 과정을 100회 반복해 누수가 없다.

Gate 1 실패 시 Spring·metrics·실사용 예제 개발을 멈추고 atomic start와 cleanup만 해결한다.

### Gate 2 — MVP feature complete: 6주차 종료

- memory, CPU rate, PID, wall time, output 제한이 동작한다.
- kernel event delta로 memory와 PID 원인을 분류한다.
- global concurrency와 bounded queue가 동작한다.
- Java와 Spring에서 daemon을 실제로 호출한다.
- clean VM 설치가 15분 안에 끝난다.

이 Gate 이후 신규 P0 요구사항을 받지 않고 bug·test·문서·데모만 수행한다.

### Gate 3 — 신뢰성 증명: 8주차 종료

- Ghost cleanup 100/100
- 공식 reason fixture 분류 정확도 100%
- 1,000회 혼합 실행에서 process·cgroup·permit 누수 0건
- 100개 동시 요청에서 active limit 위반 0건
- daemon restart 이후 stale job 0개
- 실제 PDF 또는 OCR 도구로 핵심 효과 재현

### Gate 4 — 제출 가능: 10주차 종료

- 처음 보는 사람이 15분 안에 설치와 예제 실행
- 네트워크 없이 5분 데모 3회 연속 성공
- Go binary, Rust binary, Java artifacts, checksum, tag 일치
- 지원 조건과 비보장 범위가 README와 발표에 동일하게 명시
- 두 사람 모두 설치·release·발표·복구 절차 수행 가능

## 6. 팀원 A 세부 작업 순서

### 1단계: systemd와 cgroup root

1. `/proc/self/cgroup` parser
2. canonical delegated root resolver
3. `manager` child 생성과 self move
4. `jobs` internal node와 controller enable
5. controller·파일·permission capability report
6. stale `job-*` startup scavenger

### 2단계: atomic executor

1. job ID와 leaf 경로 생성
2. limit write와 read-back 검증
3. event baseline snapshot
4. job directory FD open
5. Go `exec.Cmd.SysProcAttr`의 `UseCgroupFD`, `CgroupFD`, `PidFD`
6. Rust launcher static build와 checksum
7. target exit와 pidfd lifecycle

### 3단계: monitor와 classifier

1. stdout·stderr drain goroutine
2. wall timer
3. memory·PID event monitoring
4. caller disconnect monitoring
5. 단일 termination trigger
6. `cgroup.kill`과 empty wait
7. final evidence snapshot과 reason priority
8. cleanup·permit release

### 4단계: daemon productization

1. UDS listener와 peer credential
2. request size·budget ceiling validation
3. global scheduler와 queue
4. structured `slog`
5. graceful shutdown과 restart recovery
6. systemd install·uninstall·diagnostic scripts

## 7. Rust launcher 완료 기준

Rust launcher는 작다는 사실 자체가 요구사항이다.

- `taskcage-launcher -- <executable> [args...]` 형태만 허용
- shell 해석 없음
- 비 UTF-8 Unix argv 보존
- daemon parent-death signal 설정
- target environment와 working directory는 Go가 설정하고 launcher가 그대로 상속
- 성공 시 동일 PID에서 target으로 `exec`
- 실행 실패 시 고정된 launcher 오류 prefix와 126 또는 127 계열 종료
- release는 지원 아키텍처의 static musl binary와 SHA-256 제공
- launcher에 cgroup policy, JSON, queue, network, logging framework를 추가하지 않음
- source review와 integration fixture로 동작 증명

## 8. 테스트 매트릭스

### Go unit tests

- cgroup flat-key parser와 unknown key 허용
- size·CPU quota·duration validation
- event delta와 reason priority
- queue FIFO, timeout, cancellation, permit 단일 반납
- frame length와 malformed JSON
- canonical path와 traversal 차단

### Rust tests

- missing separator와 missing executable
- argv 보존
- target not found
- parent-death signal 설정
- exec 이후 PID 유지

### Go·Java contract tests

- 공용 JSON fixture vector
- 모든 status·reason enum
- unknown additive field
- unknown enum의 `UNKNOWN` mapping
- oversized frame와 protocol version mismatch
- base64 stdout·stderr

### Linux integration tests

- normal and non-zero exit
- wall timeout
- memory OOM
- PID limit
- CPU rate
- output limit and pipe deadlock
- caller disconnect cancellation
- orphan child cleanup
- concurrent fork during `cgroup.kill`
- queue full and queue timeout
- daemon SIGKILL and restart scavenger
- launcher missing or corrupt
- cleanup permission loss

## 9. 일정이 밀릴 때 줄이는 순서

### 절대 줄이지 않는 것

1. `CLONE_INTO_CGROUP` atomic start
2. Rust shell-free launcher
3. memory·PID·wall limit
4. `cgroup.kill`와 empty verification
5. kernel evidence 기반 reason
6. capability preflight와 fail-closed
7. concurrency limit와 cleanup 누수 테스트
8. Java SDK의 최소 동기 호출

### 먼저 줄이는 것

1. Micrometer metric 종류
2. 여러 preset과 세부 property 편의 기능
3. CPU rate 외의 고급 CPU 정책
4. peer credential 이외의 다중 사용자 정책
5. 데모 웹 UI 시각 효과
6. 두 번째 실사용 예제
7. 지원 Ubuntu·kernel matrix 수
8. benchmark 종류

핵심 보장이 흔들리면 발표 화면 대신 터미널 비교를 사용하고 테스트 시간을 지킨다.

## 10. 주간 운영 규칙

| 시점 | 활동 | 산출물 |
|---|---|---|
| 월요일 | 이번 주 통과 조건과 interface 확인 | 팀 전체 필수 issue 최대 6개 |
| 화~목요일 | 담당 구현과 짧은 branch review | 매일 main에서 최소 smoke test 성공 |
| 금요일 오전 | pinned Linux 환경 전체 통합 | raw test log와 누수 검사 결과 |
| 금요일 오후 | 5분 데모와 문서 갱신 | 주간 영상 또는 terminal recording, 다음 risk 목록 |

- protocol schema와 public enum은 ADR 또는 공동 review 없이 변경하지 않는다.
- Linux integration 실패는 다음 주 첫 작업보다 우선한다.
- 7주차부터 feature freeze다.
- 각자 주 1일 이상을 integration·문서·review에 사용한다.
- 대회 데모와 핵심 보장에 쓰이지 않는 기능은 P1 backlog로 이동한다.

## 11. MVP 완료 기준

### 정량 기준

- Ghost Process cleanup 100/100
- 공식 reason fixture 정확도 100%
- 혼합 job 1,000회 후 살아 있는 process 0, stale cgroup 0
- 동시 요청 100개에서 max active 위반 0, permit 누수 0
- daemon restart 20회 후 stale populated group 0
- clean VM Quick Start 15분 이내
- 5분 오프라인 데모 3회 연속 성공

### 제출 artifacts

- `taskcaged` Go x86-64 Linux binary
- `taskcage-launcher` Rust static x86-64 Linux binary
- Java API·client·Spring Starter artifacts
- systemd unit과 설치·진단 스크립트
- safe native fixtures와 Linux integration tests
- source archives와 SHA-256 checksums
- `README`, PRD, architecture, protocol, ADR, threat model, support matrix
- Apache-2.0 license, contributing guide, code of conduct, issue templates
- benchmark raw data와 재현 명령
- 발표 자료, 라이브 데모, 백업 영상

## 12. 첫 주 실행 backlog

### 공동

- 공식 Ubuntu LTS·kernel·JDK·Go·Rust 조합 고정
- protocol v1 schema review
- `ExecutionStatus`와 `TerminationReason` 동결
- main 보호·PR review·CI 정책 결정
- Gate 1 fixture 입력과 결과 형식 합의

### 팀원 A

- Go와 Rust toolchain 설치 문서 작성
- systemd delegated subtree 수동 실험 기록
- `/proc/self/cgroup`와 capability probe spike
- Rust launcher를 target으로 exec하는 smoke test
- `UseCgroupFD`로 launcher를 test cgroup에 생성하는 spike
- `cgroup.kill` 후 `populated 0` 확인 script

### 팀원 B

- Java API record와 enum 초안
- Java 21 UDS 연결 spike
- 4-byte frame codec와 JSON vector test
- daemon unavailable·protocol mismatch UX 정의
- Spring 예제 프로젝트 골격

첫 주 완료 조건은 Go daemon과 Java client가 UDS request/result fixture를 교환하고, Go가 실행한 Rust launcher가 argv를 보존해 target으로 교체되는 것이다.
