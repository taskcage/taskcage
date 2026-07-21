# TaskCage

> Linux 호스트에서 신뢰된 무거운 외부 프로그램을 **작업 단위로 실행·제한·관찰·정리**하는 Rust 기반 관리 프로그램과 Maven Central 배포 Java SDK

> **Status:** 초기 설계 단계입니다. 아래 아키텍처와 API는 구현 과정에서 조정될 수 있습니다.

## 대상 사용자

TaskCage의 첫 사용자는 Linux 호스트에서 Java 애플리케이션을 운영하면서 PDF·OCR·이미지·영상 변환, 브라우저 자동화, 컴파일 같은 신뢰된 외부 명령을 호출하는 개발자다.

이 사용자는 cgroup·Unix domain socket·Rust 데몬을 직접 다루지 않고, Maven Central에서 SDK를 의존성으로 추가해 외부 명령의 자원 예산과 결과만 Java API로 다룬다.

```kotlin
dependencies {
    implementation("io.github.taskcage:taskcage-java-sdk:<version>")
}
```

Spring Boot는 지원 가능한 통합 대상이지만 SDK의 전제가 아니다. 기본 SDK는 Java 21+ 일반 애플리케이션에서 동작해야 한다.

## 왜 필요한가

PDF·OCR·이미지·영상 변환, 브라우저 자동화, 컴파일처럼 실행 시간이 예측하기 어렵고 자원을 많이 쓰는 외부 프로그램은 서버 전체에 영향을 줄 수 있습니다.

- 작업 하나가 CPU나 메모리를 과도하게 사용합니다.
- 시간 초과 또는 호출 애플리케이션 종료 뒤에도 자식·손자 프로세스가 남습니다.
- 여러 작업이 겹치면 정상 요청까지 느려지거나 실패합니다.
- 실패 원인과 실제 자원 사용량을 호출 애플리케이션에서 파악하기 어렵습니다.

## TaskCage가 하는 일

TaskCage는 Linux cgroup v2를 이용해 작업마다 자원을 제한하고, 작업이 만든 프로세스 트리를 하나의 단위로 관리합니다.

- CPU, 메모리, 프로세스 수, 벽시계 실행 시간을 작업별로 제한
- 전역 동시 실행 수와 대기열 관리
- 시간 초과, 취소, 오류, 제한 초과 시 작업 cgroup 전체 종료
- exit code, signal, 종료 원인, CPU·메모리 사용량을 Java SDK에 반환
- 데몬 재시작 뒤 남은 작업 cgroup을 정리하는 복구 절차 제공

## 구성

```text
Java 애플리케이션
  └─ TaskCage Java SDK
      └─ Unix domain socket
          └─ taskcaged (Rust 데몬)
              ├─ 작업별 cgroup v2 생성 및 제한 설정
              ├─ 외부 프로그램 실행과 stdout/stderr 수집
              ├─ 동시 실행·대기열·시간 초과 관리
              ├─ 자원 통계와 종료 원인 판정
              └─ 작업 cgroup 전체 정리
```

## 핵심 설계 결정

| 영역 | 결정 | 이유 |
|---|---|---|
| 관리 프로그램 | Rust 단일 데몬 (`taskcaged`) | Linux 프로세스·cgroup API를 직접 다루면서 작은 단일 바이너리로 배포 |
| cgroup 관리 | Rust 데몬이 cgroup v2를 직접 제어 | systemd·DBus 의존 없이 작업별 제한·정리와 통계를 일관되게 관리 |
| SDK | Java 21+ 일반 Java 라이브러리 | Maven·Gradle 기반 애플리케이션에서 프레임워크 의존 없이 사용 |
| SDK 통신 | Unix domain socket | 같은 서버 안에서 빠르고, 소켓 파일 권한으로 호출자 제어 가능 |
| 프로토콜 | 버전이 있는 length-prefixed JSON | 스트림의 부분 읽기, 메시지 크기 제한, 버전 불일치를 명확하게 처리 |
| 자원 관리 | cgroup v2 | CPU·메모리·PID 제한과 작업 전체 정리에 적합 |

## 작업 생명주기

1. SDK가 명령, 인자, 자원 예산, timeout을 데몬에 요청합니다.
2. 데몬은 전역 동시 실행 한도와 환경 조건을 검사하고 작업 전용 cgroup을 만듭니다.
3. 지원 환경에서는 `clone3(CLONE_INTO_CGROUP)`로 새 프로세스를 생성 시점부터 작업 cgroup 안에서 실행합니다.
4. 데몬은 실행 시간, 출력 크기, cgroup 이벤트와 프로세스 종료를 관찰합니다.
5. 정상 종료 시 통계와 종료 상태를 수집합니다.
6. 취소·시간 초과·오류·한도 초과 시 `cgroup.kill`로 해당 작업과 하위 cgroup의 프로세스를 정리합니다.
7. `cgroup.events`의 `populated 0`을 확인한 뒤 cgroup을 제거하고 실행 슬롯을 반환합니다.

프로세스를 시작한 뒤 cgroup으로 옮기는 방식에는 짧지만 실제로 존재하는 경쟁 구간이 있습니다. TaskCage는 이를 줄이기 위해 원자적 cgroup 진입이 가능한 지원 환경을 우선 대상으로 합니다. 조건을 만족하지 못하면 보호되지 않은 상태로 실행하지 않고 명확하게 실패하는 것을 원칙으로 합니다.

## MVP 기능

### 작업별 제한

- CPU 쿼터: `cpu.max`
- 메모리 상한: `memory.max`
- 프로세스 수 상한: `pids.max`
- 벽시계 실행 시간: 데몬 타이머
- stdout/stderr 및 요청 프레임 크기 상한
- 전역 최대 실행 수, 제한된 FIFO 대기열, 대기 시간 제한

### 결과와 종료 원인

SDK는 다음 정보를 받습니다.

- 정상 종료, 실행 실패, 사용자 취소, 대기열 거절, timeout, 출력 제한 초과
- cgroup 이벤트에 근거한 메모리 OOM 및 PID 제한 초과
- exit code 또는 종료 signal
- `queueTime`, 실제 `wallTime`, CPU 사용량, 최대 메모리 사용량
- 제한된 stdout/stderr 또는 그에 대한 참조

단일 exit code만으로 OOM이나 timeout을 추측하지 않습니다. 실행 전후의 `memory.events.local`, `pids.events`, `cpu.stat` 등 커널 통계를 함께 사용해 종료 원인을 판정합니다.

## Java SDK API 방향

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
    // 재시도 또는 사용자 안내 정책 처리
}
```

공개 API의 최종 타입명과 enum은 Rust 데몬·Java SDK가 공유하는 프로토콜 fixture와 함께 확정합니다.

## 지원 범위

초기 대상은 다음과 같습니다.

- Linux cgroup v2 환경
- Ubuntu LTS 한 버전과 x86-64 조합을 먼저 검증한 뒤 Ubuntu 22.04/24.04 및 ARM64로 확대 검토
- Java 21 이상 애플리케이션
- PDF·OCR·이미지·영상 변환, 브라우저 자동화, 컴파일 등 신뢰된 외부 프로그램

시작 시 cgroup v2, `cpu`·`memory`·`pids` controller, `cgroup.kill`, 필요한 쓰기 권한, 원자적 cgroup 진입 가능 여부를 검사합니다.

## 범위 밖

TaskCage는 운영 안정성을 위한 자원 관리·프로세스 정리 도구입니다. 신뢰할 수 없는 코드를 완전히 격리하는 보안 샌드박스가 아니며, 파일·네트워크·시스템 호출 보안은 별도 정책 또는 컨테이너가 담당합니다.

CLI, Python SDK, Docker·Kubernetes 지원은 초기 기능과 실제 사용 사례가 정리된 뒤 검토합니다.

## 구현 순서

1. Rust 데몬: cgroup 생성·제한·원자적 시작·정리·통계 수집과 UDS 프로토콜
2. Linux 통합 테스트: ghost process, memory hog, 안전한 fork 폭주 fixture를 반복 검증
3. Java SDK: Gradle 기반 라이브러리, 타입 안전한 API·예외 모델·예제 애플리케이션, Maven Central 배포 준비
4. 운영성 강화: 재시작 복구, 진단 명령, 구조화 로그, 메트릭과 대기열 정책

## Java SDK 배포 계획

1. `java-sdk/`에 Java 21+ 라이브러리를 구현하고 Gradle에서 테스트·패키징한다.
2. `TaskCageClient`, `Command`, `ResourceBudget`, `ExecutionResult`를 프레임워크 독립적인 공개 API로 제공한다.
3. Maven Central에 `io.github.taskcage:taskcage-java-sdk` artifact를 배포한다.
4. Gradle Kotlin DSL, Gradle Groovy DSL, Maven 사용 예제를 README와 별도 예제 프로젝트로 제공한다.

Spring Boot starter는 실제 수요가 확인된 후 별도 artifact로 검토한다.

## 기여

문제 사례와 기능 제안은 [Issues](https://github.com/taskcage/taskcage/issues)에서 공유해 주세요.
