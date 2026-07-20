# TaskCage OSS 대회 MVP 로드맵 — 2인 팀 (보관본)

> 이 문서는 Java 내부 cgroup backend와 native shim을 전제로 한 이전 계획이다. 현재 구현 기준은 Go `taskcaged`, Rust `taskcage-launcher`, Java SDK 구조이며, 활성 로드맵은 [`../MVP-ROADMAP.md`](../MVP-ROADMAP.md)다.

## 1. 로드맵 기준

대회 일정이 확정되지 않은 상태를 고려해 제출일 기준 10주 역산 일정으로 작성한다. 실제 기간이 달라져도 주차별 순서와 통과 조건은 유지한다.

| 항목 | 기준 |
|---|---|
| 팀 구성 | 개발자 2명 |
| 개발 기간 | D-10주부터 D-day까지 10주 |
| MVP 환경 | 한 가지로 고정한 Ubuntu LTS, Linux x86-64, cgroup v2, Java 21+, Spring Boot 3.x |
| 핵심 사용자 | PDF·OCR·이미지·영상 변환 외부 프로세스를 실행하는 Spring 백엔드 개발자 |
| 핵심 보장 | 제한을 위반하거나 timeout된 작업의 프로세스 트리가 모두 종료되고, 서버는 계속 살아 있으며, 원인을 커널 증거로 설명한다. |
| 발표 형태 | 5분 이내의 Plain ProcessBuilder 대 TaskCage 비교 데모 |

대회 MVP에서 실행 관리 코어는 Java 프로세스 안의 Linux backend와 `taskcage-shim`으로 구성한다. 별도 권한 데몬인 `taskcage-agent`는 MVP 이후로 미룬다.

## 2. 대회 MVP 범위

### 반드시 완성할 기능

| 영역 | MVP 기능 | 심사에서 보여줄 증거 |
|---|---|---|
| 안전한 시작 | 실제 명령 실행 전 shim PID를 작업 cgroup에 연결하는 READY/GO barrier | 대상 프로세스가 처음부터 작업 cgroup 안에서 실행됨을 로그와 cgroup 상태로 확인 |
| 작업별 제한 | `memory.max`, `cpu.max`, `pids.max`, wall time, stdout·stderr 크기 제한 | Memory Hog, Safe Fork Storm, timeout fixture 결과 |
| 프로세스 트리 정리 | timeout·제한 위반·취소 시 `cgroup.kill`, empty 상태 확인, cgroup 제거 | 부모가 먼저 종료되는 Ghost Process에서도 생존 프로세스 0개 |
| 결과 분류 | 성공, non-zero exit, wall timeout, memory 초과, PID 초과, output 초과, 취소, queue 거절 | 커널 event delta와 watchdog 상태가 포함된 `ExecutionResult` |
| 사용량 반환 | wall time, CPU 사용 시간, peak memory, peak process count | SDK 결과와 구조화된 로그에서 확인 |
| 동시 작업 제어 | 최대 동시 실행 수, bounded FIFO queue, queue timeout | 100개 동시 요청에서도 실제 실행 수가 설정값을 넘지 않음 |
| Java SDK | `TaskCage.run(Command, ResourceBudget)` 동기 API | 예제 서비스의 기존 `ProcessBuilder` 호출을 최소 코드로 교체 |
| Spring 지원 | Auto-configuration, 설정 검증, preset, 최소 Micrometer 지표 | `application.yml`만으로 정책 적용 |
| 운영 안전성 | capability preflight, fail-closed, startup stale cleanup, bounded output | 권한·controller가 없을 때 보호 없이 실행하지 않음 |
| OSS 완성도 | README, 설치 문서, 아키텍처, 보안 경계, 라이선스, 기여 가이드, 재현 가능한 release | 처음 보는 사용자가 15분 안에 예제를 실행 |

### 이번 대회에서 제외할 기능

- CLI와 Python SDK
- ARM64와 여러 배포판 동시 지원
- 비동기 `submit()` API
- `taskcage-agent`, 원격 프로토콜, sidecar
- namespace, seccomp, AppArmor를 이용한 보안 sandbox
- 파일시스템·네트워크·GPU·IO 격리
- CPU 누적 사용 시간에 따른 강제 종료
- 정책 hot reload와 OpenTelemetry
- 복잡한 웹 대시보드
- 여러 실사용 예제: 대표 PDF 또는 OCR 예제 하나만 완성

CPU 사용 시간은 결과로 수집하지만, 누적 CPU 시간 한도에 따른 watchdog 종료는 후속 기능으로 둔다. `cpu.max`를 이용한 CPU 사용 속도 제한은 MVP에 포함한다.

## 3. 역할 분담

### 팀원 A — Java SDK·Spring·제품 통합

- 공개 API와 상태 모델
- 명령·예산 validation
- stdout·stderr bounded collector
- timeout·취소 orchestration
- 동시 실행 제한과 bounded queue
- Spring Boot Starter와 설정 property
- Micrometer 지표
- 실제 PDF/OCR 예제 서비스
- README, 발표 자료, 데모 진행

### 팀원 B — Linux backend·native·신뢰성

- cgroup v2 capability detector
- cgroup lifecycle과 제한 파일 적용
- `taskcage-shim` READY/GO protocol
- `cgroup.kill`과 empty 확인
- memory·PID·CPU 통계 및 event parser
- termination classifier의 커널 증거 수집
- Ghost Process, Memory Hog, Safe Fork Storm fixture
- 설치 스크립트, native packaging, Linux 통합 테스트
- benchmark와 release artifact

### 공동 책임

- 공개 API와 shim protocol 변경은 두 명이 함께 승인한다.
- 팀원 A가 Linux backend 테스트를, 팀원 B가 Java API 사용성을 교차 검토한다.
- 매주 최소 한 번 깨끗한 VM에서 처음부터 설치하고 전체 데모를 실행한다.
- 핵심 보장과 관련된 변경은 작성자가 아닌 팀원이 반드시 리뷰한다.
- 한 명만 이해하는 핵심 구성 요소가 없도록 매주 설계와 장애 사례를 짧게 기록한다.

## 4. 10주 실행 로드맵

| 주차 | 공동 목표 | 팀원 A | 팀원 B | 주간 통과 조건 |
|---|---|---|---|---|
| 1주차 | 범위 동결과 실행 골격 | `TaskCage`, `Command`, `ResourceBudget`, `ExecutionResult` 최소 API와 plain backend | cgroup v2 preflight, delegated subtree 수동 실험, kernel capability 표 작성 | 정상 명령 1개 실행, 지원 불가 환경에서 fail-closed, API·상태 모델 동결 |
| 2주차 | 가장 큰 기술 위험 제거 | shim 프로세스 제어와 READY/GO Java protocol, 반복 테스트 harness | `taskcage-shim`, 작업 cgroup attach, `cgroup.kill`, empty 확인 | Ghost Process 100회 실행 후 생존 프로세스와 stale cgroup이 모두 0개 |
| 3주차 | 핵심 자원 제한 | budget parser·validation, watchdog·기본 결과 분류 | memory·PID·CPU rate 제한, event delta와 사용량 수집, Memory/PID fixture | 정상·wall timeout·memory·PID fixture가 각각 기대 원인으로 분류 |
| 4주차 | 전체 lifecycle 완성 | stdout·stderr 동시 bounded drain, output limit, caller cancellation | 시작 실패·kill·cleanup 경로, startup stale cleanup, 장애 주입 fixture | 500회 혼합 실행에서 pipe deadlock, 생존 프로세스, stale cgroup이 0개 |
| 5주차 | 동시 작업 제어와 통계 | max concurrency, bounded FIFO queue, queue timeout, queue metrics | 100개 동시 요청 stress fixture, CPU time·peak memory·peak PID 결과 검증 | active job이 설정 한도를 넘지 않고 queue 포화·timeout 이유가 정확히 반환 |
| 6주차 | 사용 가능한 Java/Spring 제품 | Spring Boot Auto-configuration, property validation, preset, 예제 앱 | shim packaging, systemd delegated subtree 설치 스크립트, capability 진단 출력 | 깨끗한 VM에서 15분 안에 설치하고 Spring 예제 1회 성공 |
| 7주차 | 기능 동결과 신뢰성 강화 | API 문서, error message, metrics, concurrency·cancel race 테스트 | 1,000회 soak, attach·kill·cleanup 장애 주입, kernel 차이 처리 | P0 기능 동결, 공식 fixture 원인 분류 100%, 1,000회 cleanup 누수 0건 |
| 8주차 | 실제 사용 사례와 비교 데모 | PDF 또는 OCR 예제, Plain/TaskCage 비교 화면, 사용자 흐름 정리 | 실제 도구의 자식 프로세스 구조 검증, 시작 overhead·자원 통계 benchmark | 같은 입력으로 plain 실패와 TaskCage 격리 성공을 5분 안에 재현 |
| 9주차 | OSS release candidate | README quick start, API guide, 발표 자료, 기여 가이드 | 설치·지원 matrix, architecture, threat model, reproducible build, checksum | 제3자가 15분 내 quick start 성공, `v0.1.0-rc1` artifact 생성 |
| 10주차 | 제출 안정화 | 발표 대본, FAQ, 최종 문서, 데모 진행 | 치명적 버그 수정, release 검증, 오프라인 데모 환경과 백업 영상 | 서로 역할을 바꿔도 데모 성공, 최종 release와 제출물 checksum 고정 |

## 5. 단계별 게이트

### Gate 1 — 핵심 가능성 증명: 2주차 종료

- 프로세스가 target 실행 전에 cgroup에 들어간다.
- 부모가 먼저 죽어도 자식까지 모두 제거된다.
- Ghost Process를 100회 반복해 누수가 없다.

하나라도 실패하면 Spring·metric·실사용 예제를 시작하지 않고 이 문제부터 해결한다.

### Gate 2 — MVP 기능 완성: 6주차 종료

- memory, CPU rate, PID, wall time, output limit가 적용된다.
- 종료 원인과 CPU time·peak memory·peak PID가 반환된다.
- 동시 실행 제한과 bounded queue가 동작한다.
- Java와 Spring Boot에서 실제로 사용할 수 있다.

이 시점 이후에는 새로운 P0 기능을 추가하지 않는다.

### Gate 3 — 신뢰성 증명: 8주차 종료

- 공식 fixture 종료 원인 분류 정확도 100%
- 1,000회 실행 후 생존 프로세스와 stale cgroup 0개
- 100개 동시 요청에서도 concurrency limit 위반 0건
- 실제 PDF 또는 OCR 작업에서 plain 대비 효과 재현

### Gate 4 — 제출 가능 상태: 10주차 종료

- 처음 보는 사용자가 15분 안에 설치와 예제 실행
- 네트워크 없이 5분 데모를 3회 연속 성공
- README의 모든 명령을 clean VM에서 재검증
- release binary, source, checksum, tag가 서로 일치
- 한계와 비보장 범위를 발표와 README에 명시

## 6. 일정이 밀릴 때 줄이는 순서

### 절대 줄이지 않는 핵심

1. shim 시작 barrier
2. 작업별 cgroup과 `cgroup.kill`
3. wall time, memory, PID 제한
4. 프로세스 트리 empty 확인과 cleanup
5. 커널 증거 기반 종료 원인
6. capability preflight와 fail-closed
7. 반복·동시성 누수 테스트

### 먼저 줄일 항목

1. Micrometer 지표 개수와 Actuator 부가 기능
2. 여러 preset과 세부 설정 편의 기능
3. CPU rate 이외의 고급 CPU 정책
4. 데모 웹 UI의 시각 효과
5. 두 번째 실사용 예제
6. benchmark 종류와 지원 배포판 수

핵심 보장이 흔들리면 발표용 UI를 만드는 대신 CLI 로그와 터미널 비교 화면으로 데모한다.

## 7. 주간 운영 방식

| 시점 | 활동 | 결과물 |
|---|---|---|
| 월요일 30분 | 이번 주 통과 조건과 interface 확인 | 최대 5개의 주간 필수 issue |
| 화~목요일 | 각자 담당 구현과 상호 review | 매일 main branch에서 실행 가능한 상태 유지 |
| 금요일 오전 | Linux 환경 전체 통합 테스트 | fixture 결과와 실패 로그 |
| 금요일 오후 | 5분 데모와 문서 업데이트 | 주간 demo 영상 또는 터미널 기록, 다음 주 risk 목록 |

운영 규칙은 다음과 같다.

- 각 주의 필수 issue가 끝나기 전에는 편의 기능을 시작하지 않는다.
- 금요일 통합 테스트가 실패하면 다음 주 첫 작업은 실패 원인 제거다.
- 7주차부터는 feature freeze로 운영하고 bug, 문서, 테스트, 데모만 수정한다.
- 두 명 모두 최소 주 1일은 통합·테스트·문서에 사용한다.
- 심사 데모에 사용하지 않고 핵심 보장에도 필요 없는 기능은 backlog로 보낸다.

## 8. MVP 완료 기준

### 기능

- `TaskCage.run()`으로 정상 프로세스를 실행하고 결과를 반환한다.
- wall timeout 후 대상 cgroup의 프로세스가 0개다.
- orphan child가 남지 않는다.
- memory와 PID 초과가 각각 올바르게 분류된다.
- 출력 폭주가 설정한 크기에서 종료된다.
- 실제 실행 수가 max concurrency를 넘지 않는다.
- 지원 불가 환경에서 보호 없이 target을 실행하지 않는다.

### 품질

- Ghost Process 100/100 cleanup 성공
- 공식 종료 원인 fixture 100% 분류 성공
- 1,000회 혼합 실행에서 프로세스·cgroup 누수 0건
- 100개 동시 요청에서 Job ID 충돌과 실행 슬롯 누수 0건
- benchmark 방법과 raw result 공개
- clean VM quick start 15분 이내

### OSS 제출물

- Apache-2.0 `LICENSE`
- `README.md`와 5분 quick start
- 아키텍처와 프로세스 lifecycle 문서
- threat model과 비보장 항목
- 지원 kernel·JDK·Ubuntu matrix
- `CONTRIBUTING.md`, issue template, code of conduct
- fixture와 Linux integration test 실행 방법
- source archive, native shim, checksum이 포함된 `v0.1.0`
- 5분 발표 자료, 라이브 데모, 실패 대비 백업 영상

## 9. 5분 심사 데모 구성

| 시간 | 내용 | 심사 포인트 |
|---:|---|---|
| 0:00~0:40 | PDF/OCR timeout 뒤 자식 프로세스가 남는 문제 | 실제 서버 장애로 이어지는 명확한 문제 |
| 0:40~1:30 | Plain ProcessBuilder로 Ghost Process 재현 | 기존 timeout의 한계 |
| 1:30~2:30 | 동일 작업을 TaskCage로 실행하고 프로세스 0개 확인 | `cgroup.kill` 기반 핵심 보장 |
| 2:30~3:20 | memory·PID·queue 제한과 구조화된 결과 | 자원 격리와 운영 가능한 원인 분류 |
| 3:20~4:10 | Spring 코드와 YAML 설정 | 낮은 도입 비용 |
| 4:10~5:00 | shim barrier, honest boundary, OSS 확장 계획 | 기술적 깊이와 과장하지 않은 범위 |

데모는 외부 네트워크에 의존하지 않는다. fixture, 입력 파일, native binary, dependency cache를 발표 장비에 미리 준비하고 같은 화면의 백업 영상을 보관한다.

## 10. 주요 위험과 대응

| 위험 | 조기 신호 | 대응 |
|---|---|---|
| cgroup 권한 설정이 복잡함 | clean VM 설치가 15분을 넘음 | 한 가지 Ubuntu 환경에 집중하고 preflight·설치 스크립트를 6주차 전에 완성 |
| attach 전 프로세스가 실행되는 race | 반복 테스트에서 작업 밖 PID 발견 | READY/GO shim을 Gate 1로 두고 해결 전 상위 기능 중단 |
| OOM과 일반 SIGKILL 오분류 | 동일 fixture의 결과가 실행마다 달라짐 | 실행 전후 kernel event delta와 watchdog 상태를 함께 저장 |
| 취소·오류 시 실행 슬롯 누수 | stress test 이후 active count가 0으로 돌아오지 않음 | permit 소유권을 단일 lifecycle 객체로 관리하고 장애 주입 테스트 추가 |
| 두 사람의 작업이 마지막에만 합쳐짐 | 주중 main branch에서 전체 실행 불가 | 매주 금요일 clean integration, 짧은 branch, interface 변경 공동 승인 |
| 데모 환경 실패 | 권한·kernel 설정이 발표 장비와 다름 | 환경 고정, capability 사전 출력, 오프라인 backup 영상 준비 |
| 범위 증가 | 6주차 이후 신규 기능 issue 발생 | feature freeze와 삭제 우선순위 적용 |

## 11. 바로 시작할 첫 주 backlog

### 공동

- 공개 MVP 범위와 제외 범위를 README 초안에 고정
- 모듈 구조 결정: `taskcage-api`, `taskcage-core`, `taskcage-linux`, `taskcage-spring-boot-starter`, `fixtures`
- 한 가지 Ubuntu LTS·kernel·JDK 조합을 공식 데모 환경으로 고정
- CI의 일반 Java test와 별도 Linux privileged integration test 경계 결정
- `LICENSE`, 기본 issue label, Architecture Decision Record 폴더 생성

### 팀원 A

- `TaskCage.run()` 최소 interface와 `ExecutionResult` 정의
- `Command`, `ResourceBudget`, size·duration parser 작성
- plain backend와 정상·non-zero fixture 연결
- 상태·원인 우선순위 단위 테스트 작성

### 팀원 B

- cgroup v2 mount·controller·permission preflight 작성
- delegated subtree 생성 절차를 수동으로 재현하고 문서화
- 작업 cgroup 생성·limit write·PID attach·kill·remove spike
- Ghost Process fixture 초안 작성

첫 주의 완료 기준은 "API로 정상 명령을 실행하고, 현재 Linux 환경이 TaskCage 보장을 제공할 수 있는지 명확한 capability 결과를 반환한다"이다.
