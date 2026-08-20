# Changelog

TaskCage는 daemon과 SDK의 버전을 독립적으로 관리한다. 이 문서는 공개 컴포넌트 릴리스에서 사용자에게
영향을 주는 변경과 호환성 정보를 기록한다.

## Unreleased

현재 문서화된 변경 없음.

## taskcaged 0.5.0 - 2026-08-20

- signed Capsule archive import와 immutable catalog 기반 Profile 실행을 공개한다.
- Runtime Package digest·platform·entrypoint를 매 Task 전에 재검증하고, Capsule이 선언한 typed input, 제한된 argv와 output publish 규칙만 실행한다.
- FFmpeg Capsule의 정상 실행, timeout, memory/PID limit, 취소와 전체 cgroup cleanup을 Linux container 환경에서 검증한다.

## TaskCage Java SDK 0.4.0 - 2026-08-20 (Maven Central)

- Local `CapsuleRunner`와 Remote TLS `RemoteCapsuleRunner`로 Capsule identity와 typed Profile request를 실행한다.
- Remote Capsule의 submit, idempotent request ID, terminal result, cancel, managed Artifact upload/download를 Java 타입으로 제공한다.
- 개발 CA를 사용하는 Docker Compose E2E에서 FFmpeg Capsule의 정상·timeout·memory limit·cancel 및 artifact cleanup을 실제 daemon과 검증한다.

### Compatibility

- Java SDK 0.4.0의 Capsule 실행에는 `taskcaged` 0.5.0 이상이 필요하다.
- Local Protocol v1·v2와 Remote Protocol v1은 계속 지원한다. 기존 Local Raw Command는 호환 API로 유지한다.

## taskcaged 0.4.0 - 2026-08-14

- TLS 1.3과 service-account 인증을 사용하는 Remote Protocol v1 daemon listener를 추가한다.
- Remote principal별 Profile authorization, resource override 정책, connection·handshake·session 제한을 적용한다.
- TLS connection 위에서 관리되는 Artifact upload/download, quota, 재개 가능한 upload와 single-use input ownership을 제공한다.
- Remote Task의 Profile 실행·조회·취소를 principal 경계 안에서 제공하고, Local UDS Task·Artifact와 분리한다.
- Linux ARM64 Runtime Package와 GitHub Release archive를 지원한다.

## TaskCage Java SDK 0.3.0 - 2026-08-14 (Maven Central)

- `RemoteTaskCageClient`로 TLS 1.3 Remote Profile 실행, 조회와 취소를 제공한다. Remote Raw Command는 제공하지 않는다.
- `Path` 기반 Managed Artifact upload/download를 bounded chunk와 증분 SHA-256 검증으로 처리한다.
- service-account 인증 오류, TLS connection 실패와 daemon의 구조화 오류를 구분한다.
- Docker Compose에서 다중 chunk transfer, Profile 실행, output download와 인증 거부를 실제 daemon과 검증한다.
- Remote Profile 실행에는 `taskcaged` 0.4.0 이상이 필요하다.

## taskcaged 0.3.0 - 2026-08-13

- Linux x86-64와 함께 native ARM64 Runtime Package와 release archive를 지원한다.
- Runtime Package architecture가 실행 host와 정확히 일치하는지 검증한다.
- bootstrap installer가 host architecture에 맞는 archive를 선택한다.
- ARM64 Runtime Package, container E2E와 release packaging을 CI에서 검증한다.

## taskcaged 0.2.0 - 2026-08-12

- Protocol v1 Raw Command 동작을 유지하면서 Protocol v2 Local Execution Profile 제출과 결과 조회를 추가한다.
- owner-controlled Local Artifact root에서 입력 snapshot을 검증하고, Task 정리 뒤에만 선언된 출력 Artifact를
  원자적으로 공개한다.
- digest-addressed Runtime Package를 관리자 명령으로 import하고 플랫폼·manifest·파일 무결성을 실행 전과
  매 Task마다 검증한다.
- `file-copy@1.0.0`과 opt-in `ffmpeg-audio-to-wav@1.0.0` Profile을 제공한다.
- Runtime Package 실행은 검증된 entrypoint descriptor를 사용하며 shell과 `PATH` lookup을 거치지 않는다.
- Local UDS, Ubuntu 24.04 x86-64와 Protocol v1·v2를 지원한다. Hub와 Remote transport는 포함하지 않는다.

### 수정

- Ubuntu service가 systemd의 `DelegateSubgroup=manager`를 사용해 daemon을 처음부터 manager cgroup에
  배치한다. daemon은 이 opt-in 설정에서 현재 membership의 부모를 위임 root로 검증하므로 WSL cold boot
  뒤 첫 시작의 `219/CGROUP` 재시도를 피하면서 기존 fail-closed 경계를 유지한다.

### 호환성

- 배포되는 Ubuntu service는 `DelegateSubgroup=`를 지원하는 systemd 254 이상이 필요하다. 지원 기준인
  Ubuntu 24.04는 이 요구사항을 충족한다.

## TaskCage Java SDK 0.2.0 - 2026-08-12

- Protocol v1 Raw Command API를 유지하면서 Protocol v2 Local Profile 제출·조회·대기·취소 API를 추가한다.
- typed Profile input, Local Artifact reference, resource override와 published Artifact 결과 모델을 제공한다.
- 공유 Protocol v2 fixture, 실제 daemon Profile E2E와 FFmpeg Binding E2E로 호환성을 검증한다.
- Java 17 이상, Local UDS와 Protocol v1·v2를 지원한다. Remote transport는 포함하지 않는다.

## TaskCage FFmpeg Binding 0.1.0 - 2026-08-12

- `ffmpeg-audio-to-wav@1.0.0` Profile을 Java typed request와 result로 제공한다.
- FFmpeg executable path와 argv를 애플리케이션 API에서 숨기고 mono/stereo와 16/44.1/48 kHz 출력을 제한된
  enum으로 선택한다.
- `org.taskcage:taskcage-java-sdk:0.2.0`을 transitive dependency로 사용한다.
- taskcaged `0.2.0`, 검증된 FFmpeg Runtime Package와 Local Artifact root가 필요하다.

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
