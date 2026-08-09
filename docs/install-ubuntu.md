# Ubuntu 24.04 daemon 설치

이 문서는 이미 빌드하고 검증한 `taskcaged` binary를 Ubuntu 24.04의 전용 service account와 delegated
systemd service로 설치하는 Local Public Alpha 경로를 설명한다. release binary와 checksum 생성은 아직
이 설치 자산의 책임이 아니다.

## 배포 계약

| 자산 | 경로·값 |
|---|---|
| binary | `/usr/local/bin/taskcaged` (`root:root`, `0755`) |
| 설정 | `/etc/taskcage/taskcaged.env` (`root:taskcage`, `0640`) |
| unit | `/etc/systemd/system/taskcaged.service` |
| service account | `taskcage:taskcage`, home과 login shell 없음 |
| runtime directory | `/run/taskcage` (`taskcage:taskcage`, `0700`, systemd가 관리) |
| Local UDS | `/run/taskcage/taskcaged.sock` (`taskcage:taskcage`, `0600`) |
| cgroup | `taskcaged.service` 아래에 `Delegate=yes`로 위임된 manager와 task cgroup |

현재 Local UDS는 owner-only다. 따라서 Java SDK caller도 daemon과 같은 `taskcage` UID로 실행해야 한다.
같은 UID의 모든 process는 socket에 접근할 수 있으며 Local caller별 authorization은 아직 제공하지 않는다.
target process도 이 account로 실행되므로 필요한 binary와 작업 디렉터리 권한을 운영자가 명시해야 한다.

## 사전 조건

- Ubuntu 24.04와 systemd
- unified cgroup v2의 `cpu`, `memory`, `pids` controller
- `clone3(CLONE_INTO_CGROUP)`와 `cgroup.kill`을 지원하는 kernel
- root 권한
- 신뢰할 수 있는 source에서 얻어 별도로 검증한 Linux `taskcaged` binary

저장소에서 직접 준비할 때는 다음처럼 release binary를 빌드한다.

```bash
cargo build --workspace --release
```

## 설치

installer는 기존 `/etc/taskcage/taskcaged.env`를 덮어쓰지 않는다. 처음 설치할 때 생성된 값을 검토한 뒤
service를 시작하는 경로가 기본이다.

```bash
sudo packaging/ubuntu/install-taskcaged.sh \
  --binary target/release/taskcaged

sudoedit /etc/taskcage/taskcaged.env
sudo systemctl enable --now taskcaged.service
```

검토 없이 저장소의 명시적 기본값으로 바로 시작하는 smoke 환경에서는 `--start`를 사용할 수 있다.

```bash
sudo packaging/ubuntu/install-taskcaged.sh \
  --binary target/release/taskcaged \
  --start
```

installer를 `--start`로 다시 실행하면 실행 중인 service를 restart해 새 binary를 사용한다. 설정 파일은
그대로 유지한다.

## 설정

`taskcaged.env`의 모든 값은 systemd unit의 `serve` 인자로 전달된다.

| 환경 변수 | 설치 기본값 | 의미 |
|---|---:|---|
| `TASKCAGE_SOCKET` | `/run/taskcage/taskcaged.sock` | 절대 Local UDS 경로 |
| `TASKCAGE_MAX_CONCURRENT_TASKS` | `4` | 동시에 RUNNING일 수 있는 Task 상한 |
| `TASKCAGE_MAX_REGISTRY_TASKS` | `1000` | 메모리에 보존하는 Task record 상한 |
| `TASKCAGE_MAX_CONCURRENT_CONNECTIONS` | `32` | 동시에 열린 UDS 연결 상한 |
| `TASKCAGE_CLEANUP_TIMEOUT_MS` | `5000` | 개별 cleanup 시간 예산 |
| `TASKCAGE_FAIL_STOP_TIMEOUT_MS` | `10000` | fail-stop 전체 시간 예산 |
| `TASKCAGE_LOG_FORMAT` | `json` | `json` 또는 개발용 `compact` log 형식 |
| `RUST_LOG` | `taskcaged=info` | daemon log filter |

이 값들은 daemon process와 Registry의 운영 상한이며 Task별 CPU·memory·PID 기본 정책이 아니다. 변경한
cleanup과 fail-stop 예산의 합이 unit의 `TimeoutStopSec=30s`에 가까워지면 stop 정책도 함께 검토한다.

```bash
sudoedit /etc/taskcage/taskcaged.env
sudo systemctl restart taskcaged.service
```

## 확인

daemon은 socket을 열기 전에 cgroup recovery와 fail-closed preflight를 수행한다.

```bash
systemctl is-active taskcaged.service
systemctl show taskcaged.service --property=User --property=Group --property=Delegate
sudo stat -c '%a %U %G %n' /run/taskcage/taskcaged.sock
sudo journalctl -u taskcaged.service --since today
```

기대하는 socket 값은 `600 taskcage taskcage`다. 현재 socket 존재와 `systemctl is-active`는 process
liveness만 증명한다. 실제 daemon 준비 상태는 같은 service UID로 Protocol v1 `getCapabilities`를 호출하는
`status` 명령으로 확인한다.

```bash
sudo -u taskcage /usr/local/bin/taskcaged status \
  --socket /run/taskcage/taskcaged.sock \
  --timeout-ms 2000
```

준비된 daemon은 한 줄 JSON과 종료 코드 `0`을 반환한다.

```json
{"status":"READY","daemonVersion":"0.1.0","protocolVersions":[1],"maxFrameBytes":1048576,"maxConcurrentTasks":4,"cgroupV2Ready":true}
```

연결 실패, timeout, 잘못된 응답 또는 `cgroupV2Ready=false`는 종료 코드가 `0`이 아니다. `status`는 실행
중인 daemon을 확인하고, `check-environment`는 현재 명령 process가 속한 별도 cgroup 위임 환경만 검사한다.

service 기본 log는 journal에 JSON으로 기록된다. `event`, `request_id`, `operation`, `task_id`, admission
결과와 종료 원인·cleanup 증거를 기준으로 검색할 수 있다. 기본 log에는 실행 argv, 환경 변수 값,
작업 디렉터리 또는 stdout/stderr tail을 기록하지 않는다.

```bash
sudo journalctl -u taskcaged.service -o cat --since today | \
  grep '"event":"task_finished"'
```

독립된 delegated preflight만 다시 실행하려면 service와 다른 transient unit을 사용한다.

```bash
sudo systemd-run \
  --wait --collect --pipe \
  --unit=taskcage-preflight \
  --property=Type=exec \
  --property=User=taskcage \
  --property=Group=taskcage \
  --property=Delegate=yes \
  /usr/local/bin/taskcaged check-environment
```

## 업그레이드와 rollback

새 binary와 되돌릴 이전 binary를 모두 신뢰할 수 있는 별도 경로에 보관한다. 같은 installer에 선택한
binary를 넘기면 원자적으로 경로를 교체하고 service를 restart한다.

```bash
sudo packaging/ubuntu/install-taskcaged.sh --binary /srv/taskcage/taskcaged-new --start

# rollback
sudo packaging/ubuntu/install-taskcaged.sh --binary /srv/taskcage/taskcaged-previous --start
```

binary provenance와 checksum은 배포 전에 별도로 검증해야 한다. 호환되지 않는 설정 변경이 있다면 이전
설정도 함께 복원한 뒤 restart한다.

## 제거

기본 제거는 service, unit과 binary만 제거하며 operator 설정과 `taskcage` account를 보존한다.

```bash
sudo packaging/ubuntu/uninstall-taskcaged.sh
```

설정도 제거하려면 명시적으로 요청한다. account와 group은 이 경우에도 자동 삭제하지 않는다.

```bash
sudo packaging/ubuntu/uninstall-taskcaged.sh --purge-config
```

TaskCage는 보안 sandbox가 아니다. 전용 account와 cgroup delegation은 운영·자원 경계를 제공하지만 신뢰할
수 없는 code의 filesystem, network 또는 syscall을 격리하지 않는다.
