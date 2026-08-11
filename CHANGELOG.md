# Changelog

TaskCage는 daemon과 SDK의 버전을 독립적으로 관리한다. 이 문서는 공개 컴포넌트 릴리스에서 사용자에게
영향을 주는 변경과 호환성 정보를 기록한다.

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
