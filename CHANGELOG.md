# Changelog

TaskCage는 daemon과 SDK의 버전을 독립적으로 관리한다. 이 문서는 공개 컴포넌트 릴리스에서 사용자에게
영향을 주는 변경과 호환성 정보를 기록한다.

## TaskCage Java SDK 0.1.0 - 2026-08-11

TaskCage Java Core SDK의 첫 Local Public Alpha 릴리스다. Java 애플리케이션이 같은 Linux host의
`taskcaged`에 Protocol v1 Task를 제출하고, 상태·완료 결과·취소를 Java 타입으로 다룰 수 있게 한다.

### 주요 기능

- Java 17 이상에서 Spring Boot 의존성 없이 사용할 수 있는 `java-library`를 제공한다.
- `TaskCageClient`와 Local Unix domain socket transport로 daemon capability를 조회한다.
- 저수준 `submit`, `getTask`, `cancelTask`와 bounded 동기 `run` API를 제공한다.
- `TaskHandle`로 상태 조회, polling 기반 완료 대기와 cleanup-confirmed 취소를 제공한다.
- 실행 파일과 argv 배열을 분리한 `ExternalCommand`로 shell 해석 없는 외부 명령 실행을 요청한다.
- CPU, memory, PID, 벽시계 시간과 출력 tail에 유한한 `ResourceBudget.safeDefaults()`를 제공한다.
- `RUNNING`/`FINISHED` snapshot, 종료 원인, 시간·자원 사용량과 stdout/stderr tail을 타입으로 반환한다.
- 연결, Protocol과 daemon 오류를 구분하고 daemon 오류의 code와 retryable 여부를 보존한다.
- 호출자 지정 UUID를 이용해 같은 daemon 생존 기간 안에서 멱등 제출과 응답 유실 복구를 지원한다.
- 실제 Ubuntu daemon과 FFmpeg를 이용해 정상 실행, timeout과 전체 프로세스 정리를 검증한다.

### 설치

Maven Central의 다음 좌표를 Gradle 또는 Maven 프로젝트에 추가한다.

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.1.0")
}
```

SDK 사용 전 같은 Linux host에 호환되는 `taskcaged`를 설치하고 Local UDS 접근 권한을 설정해야 한다.
사용법과 공개 타입은 [`java-sdk/README.md`](java-sdk/README.md)를 따른다.

### 호환성

- 지원 Java: 17 이상
- 지원 Protocol: v1
- 권장 daemon: `taskcaged` 0.1.0
- 연결 방식: 같은 Linux host의 Local Unix domain socket

daemon과 Java SDK의 제품 버전은 독립적이며 지원 Protocol version으로 연결 호환성을 판단한다.

### 알려진 제한과 보안 경계

- Local UDS만 지원하며 인증된 Remote transport와 TCP 연결은 제공하지 않는다.
- 기본 daemon 설치의 UDS는 `taskcage:taskcage` 소유의 `0600`이므로 Java caller도 같은 service UID로
  실행해야 한다.
- Raw Command만 지원하며 Execution Profile, Profile Binding, Bundle과 Runtime Package는 제공하지 않는다.
- `run`과 `await`의 SDK wait timeout은 Task를 자동 취소하지 않으며, client `close()`도 제출한 Task를
  자동 취소하지 않는다.
- daemon 재시작을 가로지르는 작업 복구나 exactly-once 실행을 보장하지 않는다.
- TaskCage는 신뢰된 외부 프로그램을 위한 자원·수명주기 관리 도구이며 보안 sandbox가 아니다.
- `0.x`는 공개 API와 운영 계약을 검증하는 초기 버전이며 GitHub prerelease로 게시한다.

## taskcaged 0.1.0 - 2026-08-11

TaskCage daemon의 첫 Local Public Alpha 릴리스다. 신뢰된 외부 프로그램을 작업별 cgroup v2 경계에서
실행하고, 자원 제한과 프로세스 트리 수명주기를 관리한다.

### 주요 기능

- CPU, memory, PID 수와 벽시계 실행 시간을 Task 단위로 제한한다.
- cgroup과 제한을 적용·확인한 뒤에만 외부 프로그램을 시작한다.
- timeout, 취소와 실행 오류 시 루트 PID가 아닌 Task의 전체 cgroup을 정리한다.
- 종료 원인, exit code 또는 signal, CPU 시간, memory peak와 제한된 stdout/stderr tail을 반환한다.
- 호스트 단위 동시 실행 상한과 부작용 없는 capacity 거절을 제공한다.
- Local Unix domain socket에서 Protocol v1의 제출·조회·취소·capability API를 제공한다.
- Ubuntu systemd service, 환경 사전 검사, readiness 상태 명령과 구조화 로그를 제공한다.
- GitHub Release archive, SHA-256 checksum과 검증·설치를 수행하는 bootstrap installer를 제공한다.
- Ubuntu FFmpeg 작업의 정상 완료와 timeout 후 전체 프로세스 정리 흐름을 검증한다.

### 설치

지원되는 Ubuntu 24.04 x86-64 host에서 릴리스에 첨부된 설치 스크립트를 내려받아 내용을 확인한 뒤
실행한다. 기본 동작은 설치 후 `taskcaged.service`를 활성화하고 시작한다.

```bash
curl --fail --location \
  --output install-taskcaged.sh \
  https://github.com/taskcage/taskcage/releases/download/taskcaged-v0.1.0/install-taskcaged.sh
less install-taskcaged.sh
sudo bash install-taskcaged.sh --version 0.1.0
```

설정 파일을 먼저 검토하려면 `--no-autostart`를 사용한다. 자세한 설치·재설치·제거 절차는
[`docs/install-ubuntu.md`](docs/install-ubuntu.md)를 따른다.

### 호환성

- 지원 OS: Ubuntu 24.04
- 지원 architecture: Linux x86-64
- 지원 Protocol: v1
- 필수 환경: unified cgroup v2의 `cpu`, `memory`, `pids` controller와 systemd delegation

daemon과 Java SDK는 제품 버전이 아니라 지원 Protocol version으로 연결 호환성을 판단한다. Java SDK는
별도 컴포넌트와 태그로 릴리스한다.

### 알려진 제한과 보안 경계

- Local UDS만 지원하며 Remote Runtime과 TCP 연결은 제공하지 않는다.
- 기본 UDS는 `taskcage:taskcage` 소유의 `0600`이므로 caller도 같은 service UID로 실행해야 한다.
- Linux cgroup v2 자원·수명주기 관리 도구이며, 파일시스템·네트워크·syscall을 격리하는 보안 sandbox가
  아니다. 신뢰된 외부 프로그램만 실행해야 한다.
- 작업 queue, 분산 scheduler, 업무 재시도와 daemon 재시작을 가로지르는 작업 복구는 제공하지 않는다.
- `0.x`는 공개 API와 운영 계약을 검증하는 초기 버전이며 GitHub prerelease로 게시한다.
