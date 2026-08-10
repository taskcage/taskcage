# Ubuntu 24.04 daemon 설치

이 문서는 검증한 `taskcaged` Public Alpha archive를 Ubuntu 24.04의 전용 service account와 delegated
systemd service로 설치하는 경로를 설명한다. release가 아직 공개되지 않았거나 source에서 개발 중이면
같은 installer에 직접 빌드한 binary를 전달할 수 있다.

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
- GitHub Release에서 받은 Linux x86-64 archive와 SHA-256 sidecar 또는 신뢰할 수 있는 source checkout

## Release archive 준비

공개된 버전을 설치할 때는 archive와 checksum을 같은 GitHub Release에서 받고, 압축을 풀기 전에 검증한다.
아래의 `VERSION`은 설치하려는 실제 release로 바꾼다. `0.1.0-alpha.1`은 pipeline의 첫 후보 버전이며 release가
공개되기 전에는 URL이 존재하지 않는다.

```bash
VERSION=0.1.0-alpha.1
ASSET="taskcage-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
RELEASE_URL="https://github.com/taskcage/taskcage/releases/download/v${VERSION}"

curl --fail --location --remote-name "${RELEASE_URL}/${ASSET}"
curl --fail --location --remote-name "${RELEASE_URL}/${ASSET}.sha256"
sha256sum --check "${ASSET}.sha256"
tar --extract --gzip --file "${ASSET}"
```

archive는 `bin/taskcaged`, Ubuntu installer·unit·기본 설정, README와 Apache-2.0 LICENSE를 하나의
versioned top-level directory 아래에 담는다. checksum이 일치하지 않거나 예상하지 않은 파일·symlink가
있으면 설치하지 않는다.

저장소에서 직접 준비할 때는 checkout의 version과 lockfile을 사용해 release binary를 빌드한다.

```bash
cargo build --workspace --release
```

이 경우 아래 release 경로 대신 checkout의 `packaging/ubuntu/install-taskcaged.sh`와
`target/release/taskcaged`를 사용한다.

## 설치

installer는 기존 `/etc/taskcage/taskcaged.env`를 덮어쓰지 않는다. 처음 설치할 때 생성된 값을 검토한 뒤
service를 시작하는 경로가 기본이다.

```bash
sudo "taskcage-v${VERSION}-x86_64-unknown-linux-gnu/packaging/ubuntu/install-taskcaged.sh" \
  --binary "taskcage-v${VERSION}-x86_64-unknown-linux-gnu/bin/taskcaged"

sudoedit /etc/taskcage/taskcaged.env
sudo systemctl enable --now taskcaged.service
```

검토 없이 저장소의 명시적 기본값으로 바로 시작하는 smoke 환경에서는 `--start`를 사용할 수 있다.

```bash
sudo "taskcage-v${VERSION}-x86_64-unknown-linux-gnu/packaging/ubuntu/install-taskcaged.sh" \
  --binary "taskcage-v${VERSION}-x86_64-unknown-linux-gnu/bin/taskcaged" \
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
| `TASKCAGE_MAX_TASK_CPU_QUOTA_US` | `200000` | Task 하나의 CPU quota 최대값 |
| `TASKCAGE_MAX_TASK_CPU_PERIOD_US` | `100000` | CPU 최대 비율의 period |
| `TASKCAGE_MAX_TASK_MEMORY_BYTES` | `2147483648` | Task 하나의 memory 최대값 (2 GiB) |
| `TASKCAGE_MAX_TASK_PIDS` | `128` | Task 하나의 PID 최대값 |
| `TASKCAGE_MAX_TASK_TIMEOUT_MS` | `900000` | Task 하나의 벽시계 시간 최대값 (15분) |
| `TASKCAGE_MAX_TASK_STDOUT_TAIL_BYTES` | `65536` | stdout tail 최대값 |
| `TASKCAGE_MAX_TASK_STDERR_TAIL_BYTES` | `65536` | stderr tail 최대값 |
| `TASKCAGE_LOG_FORMAT` | `json` | `json` 또는 개발용 `compact` log 형식 |
| `RUST_LOG` | `taskcaged=info` | daemon log filter |

`TASKCAGE_MAX_TASK_*` 값은 Task 하나가 요청할 수 있는 배포 최대값이다. Java SDK의
`ResourceBudget.safeDefaults()`는 CPU `100000/100000`, memory 512 MiB, PID 32, 2분, 출력 tail 각각
65,536 bytes를 요청하는 편의값이며 daemon과 협상하지 않는다. 운영자가 최대값을 이보다 낮추면 SDK
기본값도 `LIMIT_EXCEEDS_POLICY`로 거절된다. CPU는 quota/period 비율로 비교한다.

이 정책은 실행 중 동적으로 reload하지 않는다. 설정을 바꾼 뒤 service를 restart해야 하며 기존 RUNNING
Task의 제한은 바뀌지 않는다. 이 최대값은 Task별 admission만 담당하고 전체 host resource pool, 공정성이나
overcommit을 관리하지 않는다. cleanup과 fail-stop 예산의 합이 unit의 `TimeoutStopSec=30s`에 가까워지면
stop 정책도 함께 검토한다.

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
{"status":"READY","daemonVersion":"0.1.0-alpha.1","protocolVersions":[1],"maxFrameBytes":1048576,"maxConcurrentTasks":4,"cgroupV2Ready":true}
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

각 archive의 SHA-256 sidecar를 보관하고 설치 전에 다시 검증한다. Maven Central과 공개된 GitHub Release
자산은 덮어쓸 수 없으므로 수정 release는 새 버전으로 발행한다. 호환되지 않는 설정 변경이 있다면 이전
archive와 이전 설정을 함께 복원한 뒤 restart한다.

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
