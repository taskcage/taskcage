# Linux 통합 시험

이 디렉터리는 실제 Linux cgroup v2에서 TaskCage의 안전 보장을 검증한다. 제어 파일 이름만 흉내 낸 일반 디렉터리는 Linux 동작의 근거로 사용하지 않는다.

## 환경

- Linux cgroup v2
- `cpu`, `memory`, `pids` controller
- `cgroup.kill`과 cgroup 제어 파일 쓰기 권한
- 데몬이 사용할 cgroup subtree 위임
- `clone3(CLONE_INTO_CGROUP)` 지원

테스트 환경은 systemd service 또는 scope의 `Delegate=yes`를 사용해 위임을 준비할 수 있다. 이는 테스트 환경 구성 방법이며, `taskcaged` 자체가 systemd나 DBus에 의존한다는 뜻은 아니다.

공유·운영 서버가 아니라 전용 VM에서 실행한다. 필요한 환경이나 권한이 없으면 스크립트는 종료 코드 77로 건너뛸 수 있지만, GitHub Actions의 Ubuntu 24.04 작업에서는 skip 없이 통과해야 한다.

## 사전 검사

```bash
bash integration-tests/preflight-fail-closed.sh
```

`preflight-fail-closed.sh`는 다음 계약을 검증한다.

- 실제 cgroup v2 위임 경로와 필수 controller·파일을 확인한다.
- 데몬의 manager cgroup 이동과 `/proc/self/cgroup` 결과를 확인한다.
- 외부 target 없이 원자적 cgroup 진입 가능 여부를 검사한다.
- 잘못된 경로, controller·권한 부족, 원자 진입 미지원에서는 외부 target을 실행하지 않는다.

## 실행·정리 smoke test

```bash
bash integration-tests/cgroup-runner-smoke.sh
```

`cgroup-runner-smoke.sh`는 다음 영역을 실제 커널 동작으로 검증한다.

| 영역 | 검증 내용 |
|---|---|
| 제한과 시작 | CPU·메모리·PID 제한 write/read-back, `clone3(CLONE_INTO_CGROUP)` 원자 시작, exec gate 이후 시간 측정 |
| 결과 | 정상·비정상 종료, exec 실패, timeout, cgroup 사용량과 종료 원인 |
| 정리 | 대표 프로세스 종료 뒤 남은 자식·손자 전체 종료, `populated 0`, cgroup·출력 reader 정리 |
| 출력 | stdout/stderr 동시 drain, 독립된 tail 상한과 truncation, 후손이 보유한 출력 FD 회수 |
| 프로토콜 | submit/get/cancel, 멱등 요청, 실행·Registry·UDS 연결 capacity, 연결 중단 뒤 작업 지속 |
| 복구 | 단일 데몬 시작 소유권, 검증된 stale socket·잔여 cgroup 복구, 준비 전 listener 차단 |
| fail-stop | 정리 불확실 시 신규 실행 차단, 활성 작업 전체 정리, 제한된 deadline과 비정상 종료 |

Java SDK와 실제 데몬의 공개 API 계약은 `java-sdk`의 `e2eTest`에서 별도로 검증한다. Protocol v1의 JSON 형태는 [`protocol-fixtures/v1`](../protocol-fixtures/v1/README.md)이 고정한다.

## Ubuntu systemd service smoke test

```bash
bash integration-tests/systemd-service-smoke.sh
```

`systemd-service-smoke.sh`는 기존 TaskCage 설치가 없는 전용 Ubuntu 24.04 host에서만 실행한다. prebuilt
binary 설치, 전용 account, `Delegate=yes`, owner-only UDS, manager membership, 설정 보존, stop과 uninstall을
실제 systemd로 검증한다. 기존 unit, binary, user 또는 group이 있으면 이를 변경하지 않고 종료 코드 77로
건너뛴다. CI에서는 종료 코드 77도 성공으로 처리하지 않는다.

## FFmpeg Local Raw Command reference workflow

```bash
sudo apt-get update
sudo apt-get install -y --no-install-recommends ffmpeg
bash integration-tests/ffmpeg-reference-workflow.sh
```

`ffmpeg-reference-workflow.sh`는 기존 TaskCage 설치가 없는 Ubuntu 24.04 host에서 설치된 daemon과
owner-only UDS를 사용한다. Java Core SDK가 FFmpeg를 shell 없이 직접 실행해 실제 WAVE 결과를 만들고,
동일한 FFmpeg descendant launcher를 일반 `ProcessBuilder`와 TaskCage timeout으로 각각 실행한다.

일반 root-only 종료 뒤에는 FFmpeg child가 살아 있음을 먼저 확인한 후 시험이 직접 정리한다. TaskCage
경로는 `TIMED_OUT`, descendant PID 소멸, task cgroup 원상 복구와 `cleanup_complete=true`를 확인한다.
출력하는 FFmpeg package version과 전체 소요 시간은 CI evidence이며 Docker 대비 성능 측정은 아니다.

## Public Alpha release artifact smoke test

```bash
bash integration-tests/release-artifact-smoke.sh \
  0.1.0-alpha.1 \
  dist/taskcage-v0.1.0-alpha.1-x86_64-unknown-linux-gnu.tar.gz \
  dist/taskcage-v0.1.0-alpha.1-x86_64-unknown-linux-gnu.tar.gz.sha256
```

`release-artifact-smoke.sh`는 checksum과 archive layout을 먼저 검사하고, archive 안의 installer와 prebuilt
binary만 사용해 전용 account·systemd service·owner-only UDS·live daemon version을 검증한 뒤 제거한다.
기존 TaskCage 설치나 account가 있는 host는 변경하지 않고 종료 코드 77로 건너뛴다. release gate와
GitHub Actions에서는 77을 성공으로 처리하지 않는다.
