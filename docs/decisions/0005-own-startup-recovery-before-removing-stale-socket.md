# 0005. 시작 복구 소유권을 획득한 뒤 stale socket을 제거한다

## 문제

비정상 종료 뒤에는 Unix domain socket 경로와 작업 cgroup이 남을 수 있다. 다음 데몬이 socket 경로가
존재한다는 사실이나 한 번의 연결 실패만으로 이를 stale이라고 판단하면 일반 파일, 다른 프로세스의
socket 또는 아직 시작 중인 socket을 삭제할 수 있다. 두 `taskcaged`가 동시에 복구를 수행하면 확인한
파일과 삭제하는 파일이 달라지는 경합도 생긴다.

ADR 0004는 기존 경로를 삭제하지 않는 안전한 bind를 먼저 정했고, ADR 0003은 요청 수락 전에 잔여
TaskCage cgroup을 정리하도록 요구한다. 이 ADR은 두 원칙을 유지하면서 crash 뒤 시작 복구에 필요한
소유권 경계, stale socket 증명과 실행 순서를 결정한다.

## 검토한 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| socket 경로가 있으면 항상 시작 거절 | 다른 파일을 삭제하지 않음 | crash 뒤 자동 복구가 불가능함 |
| `connect`가 실패하면 즉시 삭제 | 구현이 단순함 | 권한 오류, timeout, 시작 중인 listener와 일시 오류를 stale로 오판할 수 있음 |
| PID 파일만 확인 | 운영자가 이해하기 쉬움 | PID 재사용과 기록 유실 때문에 생존 여부를 증명하지 못함 |
| 생존 기간 lock을 먼저 잡고 파일 신원과 연결 결과를 함께 검증 | 동시에 복구하는 TaskCage 인스턴스를 막고 삭제 근거를 좁힐 수 있음 | 보호된 runtime 디렉터리와 Linux 파일 API가 필요함 |

## 결정

TaskCage MVP는 네 번째 선택지를 사용한다. 시작 복구를 구현하기 전까지 ADR 0004의 기존 동작처럼
어떤 기존 경로도 삭제하지 않고 시작을 거절한다.

### 인스턴스와 runtime 디렉터리

- 하나의 서비스 인스턴스는 명시한 socket과 그 부모 runtime 디렉터리, supervisor가 독점 위임한
  cgroup root를 함께 소유한다. 같은 위임 root를 서로 다른 runtime 디렉터리의 여러 데몬에 주는 설정은
  지원하지 않는다. 독점 소유를 확인할 수 없으면 cgroup 복구를 시작하지 않는다.
- socket 경로는 ADR 0004처럼 절대 경로여야 한다. 경로의 각 디렉터리는 directory file descriptor를
  기준으로 열고 `O_NOFOLLOW`를 사용해 symlink를 따라가지 않는다.
- root 디렉터리를 제외한 각 상위 디렉터리의 owner는 root 또는 daemon의 effective UID여야 하고,
  group·other 쓰기 권한이 없어야 한다. 최종 부모 디렉터리는 daemon의 effective UID가 소유하고
  group·other 쓰기 권한이 없어야 한다. 이 조건을 만족하지 않으면 시작에 실패한다.
- TaskCage는 시작 복구를 위해 UID 또는 GID를 바꾸거나 디렉터리 권한을 고치지 않는다. 다른 사용자가
  파일 이름을 교체할 수 있는 부모 디렉터리는 명시된 경로여도 신뢰하지 않는다.

### 단일 daemon lock

- lock 경로는 socket 부모 디렉터리의 `.taskcaged.lock`이다. 이 디렉터리는 한 서비스 인스턴스 전용이므로
  socket 파일명이 달라도 같은 인스턴스의 시작을 직렬화한다.
- lock 파일은 symlink를 따라가지 않는 `O_NOFOLLOW`와 `O_CLOEXEC`으로 열고, 처음 만들 때는
  `O_CREAT | O_EXCL`과 mode `0600`을 사용한다. 기존 lock은 일반 파일, link count 1, effective UID
  소유와 정확한 mode `0600`을 모두 확인한다. 조건이 다르면 삭제, 교체, `chmod` 또는 `chown`하지 않고
  시작에 실패한다.
- daemon은 `flock(LOCK_EX | LOCK_NB)`로 non-blocking exclusive lock을 획득한다. 이미 잠겨 있으면 다른
  인스턴스가 살아 있거나 시작·종료 중인 것으로 보고 socket과 cgroup을 건드리지 않은 채 시작에
  실패한다.
- lock file descriptor는 시작 복구, 요청 처리와 정상 종료가 끝날 때까지 유지한다. 파일 안의 PID 같은
  값은 진단 정보일 뿐 소유권이나 생존 판정의 근거로 사용하지 않는다.
- lock 파일은 정상 종료에도 삭제하지 않는다. 정상 종료와 crash 모두 file descriptor가 닫히면 커널이
  lock을 해제하며, 다음 시작은 같은 파일의 신원과 권한을 다시 확인한다.

이 lock은 협력하는 TaskCage 인스턴스 사이의 소유권을 보장한다. 같은 UID의 다른 프로세스와 root는
MVP 신뢰 경계 안에 있으며, TaskCage는 이들을 격리하는 보안 sandbox가 아니다.

### stale socket 판정과 제거

exclusive lock을 유지한 상태에서 다음 절차를 순서대로 수행한다.

1. 부모 directory file descriptor를 기준으로 `fstatat(..., AT_SYMLINK_NOFOLLOW)`를 사용해 socket 이름을
   확인한다. 경로가 없으면 제거할 대상이 없다.
2. 일반 파일, 디렉터리, symlink 또는 socket이 아닌 객체는 TaskCage 소유로 간주하지 않는다. 삭제하지
   않고 시작에 실패한다.
3. socket이면 owner가 daemon effective UID인지, mode가 정확히 `0600`인지, link count가 1인지 확인하고
   device와 inode를 기록한다. 하나라도 다르거나 확인할 수 없으면 삭제하지 않고 시작에 실패한다.
4. 새 non-blocking `SOCK_STREAM` socket으로 제한된 내부 deadline 안에서 `connect`를 시도하고
   `poll`과 `SO_ERROR`로 결과를 확인한다. protocol 요청은 보내지 않는다.
5. 연결 성공은 활성 socket이다. 권한 오류, timeout과 분류할 수 없는 오류는 판정 불확실이다. 이 경우
   기존 socket을 삭제하지 않고 시작에 실패한다.
6. `ECONNREFUSED`만 stale 후보의 연결 근거로 인정한다. 단, 이 오류 하나만으로 삭제하지 않고 앞의
   lock, 부모 디렉터리, 객체 종류, owner와 mode 검증이 모두 성공해야 한다. 확인 중 `ENOENT`가 나오면
   이름을 다시 검사하고, 계속 없을 때만 제거 없이 다음 단계로 간다.
7. 삭제 직전에 같은 방식으로 다시 검사해 객체 종류, owner, mode, link count, device와 inode가 처음
   기록과 모두 같은지 확인한다. 다르면 삭제하지 않고 시작에 실패한다.
8. 같은 부모 directory file descriptor를 기준으로 `unlinkat`을 한 번 호출한다. 제거 뒤 이름이 다시
   생겼거나 상태를 확인할 수 없으면 시작에 실패한다.

명시적으로 받은 경로라는 사실은 삭제 권한이나 stale 증거가 아니다. 활성 socket, 연결 성공, 권한
오류, timeout 또는 판정 불확실 상태에서는 기존 경로를 삭제하지 않는다.

Linux에는 확인한 device·inode만 조건부로 unlink하는 단일 syscall이 없다. exclusive lock과 교체할 수
없는 부모 디렉터리로 판정과 삭제 사이의 경합을 제한하고, 같은 UID 또는 root가 그 짧은 사이에 경로를
바꾸는 공격은 MVP 신뢰 경계 밖으로 둔다.

### 시작 순서와 실패 처리

daemon 시작은 다음 순서를 지킨다.

1. 보호된 runtime 디렉터리를 확인하고 단일 daemon lock을 획득한다.
2. 위 절차로 기존 socket을 확인하고, TaskCage 소유의 stale socket임을 모두 증명한 경우에만 제거한다.
3. 독점 위임된 cgroup root에서 남아 있는 TaskCage 작업 cgroup을 whole-cgroup 방식으로 종료하고
   `populated 0`과 제거를 확인한다.
4. 기존 cgroup 사전 검사를 새로 실행해 target 실행에 사용할 `VerifiedEnvironment`를 만든다.
5. socket을 bind하고 mode `0600`, owner와 device·inode를 확인한다.
6. 앞 단계가 모두 성공한 뒤 UDS 요청을 수락한다.

3단계는 잔여 cgroup을 식별하고 정리하는 데 필요한 cgroup v2 mount, 현재 membership, 위임 root와
필수 제어 파일을 fail-closed로 먼저 확인한다. 이 제한된 확인은 target 실행 readiness를 뜻하지 않으며,
잔여 정리 뒤 4단계의 전체 사전 검사를 대체하지 않는다.

crash 뒤 위임 root에 `manager`와 `jobs`가 남으면 cgroup v2의 no-internal-process 제약 때문에 supervisor가
새 daemon을 비어 있지 않은 위임 root로 바로 옮기지 못할 수 있다. 이때는 설정으로 부모 위임 root를
명시했고 새 daemon의 실제 membership이 정확히 그 root의 바로 아래 `manager`인 경우만 시작 복구를
허용한다. 복구는 `manager/cgroup.procs`에 현재 daemon 하나만 있는지 확인하고 manager의 신원과 leaf
구조를 다시 검증한 뒤, 형제 `jobs` 아래의 잔여 작업을 정리한다. 검증된 기존 manager는 제거하지 않고
4단계의 전체 사전 검사와 이후 daemon 격리에 재사용한다.

부모 위임 root가 명시되지 않았거나, 실제 membership이 다른 하위 cgroup이거나, manager에 다른 직접
프로세스가 있거나, manager 신원과 구조를 확인할 수 없으면 재사용하지 않고 startup 오류로 종료한다.
이 예외는 기존 manager 하나에만 적용되며 임의의 조상·후손 경로를 위임 root로 추론하지 않는다.

어느 단계에서든 소유권, 판정 또는 복구가 실패하면 socket을 bind하거나 요청을 받지 않고 0이 아닌
startup 오류로 종료한다. 이 실패는 protocol 응답이 아니므로 새 wire 오류 코드를 추가하지 않는다.

### 정상 종료와 crash

- 정상 종료는 신규 연결 수락과 기존 handler를 정리한 뒤, 현재 경로가 자신이 bind할 때 기록한 동일한
  socket device·inode인 경우에만 socket을 제거한다. lock file descriptor는 socket 처리 뒤 마지막에
  닫고 lock 파일은 남긴다.
- 정상 종료 중 socket 경로가 교체되면 교체된 객체를 삭제하지 않는다.
- crash에서는 커널이 listener와 lock file descriptor를 닫지만 socket 이름과 lock 파일은 남을 수 있다.
  다음 시작은 이 ADR의 전체 판정을 처음부터 다시 수행한다.
- 시작 중 bind까지 성공한 뒤 실패하면 자신이 기록한 동일 device·inode의 socket만 제거하고 lock을
  해제한다. 시작 전에 발견한 불명확한 객체는 어떤 실패 경로에서도 삭제하지 않는다.

## 선택 이유

- **파일 안전성:** 경로 문자열이나 단일 연결 오류를 파일 소유권 증거로 사용하지 않는다.
- **동시 시작 방지:** socket과 cgroup 복구 전에 생존 기간 lock을 잡아 두 TaskCage가 같은 인스턴스를
  동시에 복구하지 못하게 한다.
- **fail-closed 실행:** stale socket과 잔여 cgroup을 모두 안전하게 처리한 뒤에만 readiness를 검증하고
  요청을 받는다.
- **protocol 호환성:** 시작 실패는 연결 전 운영 오류이므로 protocol v1 field, 상태와 오류 코드를
  변경하지 않는다.

## 검증 방법

- 두 daemon을 동시에 시작해 하나만 lock을 획득하고 다른 하나는 어떤 파일과 cgroup도 변경하지 않는지
  실제 Linux에서 확인한다.
- 일반 파일, 디렉터리, symlink, 다른 UID의 socket, 잘못된 mode와 link count를 삭제하지 않는지 확인한다.
- 연결 성공, 권한 오류, timeout과 불명확한 연결 오류에서 socket을 삭제하지 않는지 확인한다.
- 조건을 모두 만족하고 `ECONNREFUSED`인 socket만 제거하는지 확인한다.
- 판정과 삭제 사이에 device·inode 또는 파일 종류를 바꾸면 제거하지 않는지 fault injection으로
  확인한다.
- group·other가 쓸 수 있거나 신뢰하지 않는 owner의 부모 디렉터리에서 시작을 거절하는지 확인한다.
- socket 복구 뒤 잔여 cgroup의 `populated 0`과 제거를 확인하고, 전체 preflight와 bind가 그 뒤에만
  실행되는지 확인한다.
- crash 뒤 새 daemon을 명시된 위임 root의 기존 `manager`에 시작해 manager는 보존하고 형제 `jobs`의
  잔여 작업만 정리한 뒤 전체 preflight와 bind가 진행되는지 확인한다. manager에 다른 프로세스가 있거나
  다른 하위 cgroup에서 시작하면 요청을 받지 않는지도 확인한다.
- 정상 종료는 자신이 bind한 socket만 제거하고 lock 파일은 남기며, crash 뒤 다음 시작이 복구하는지
  실제 Ubuntu 24.04 cgroup v2와 UDS 환경에서 확인한다.

## 영향과 남은 위험

- 배포 설정은 인스턴스 전용 runtime 디렉터리를 daemon effective UID 소유로 만들고 group·other 쓰기를
  금지해야 한다. `/tmp` 같은 공유 쓰기 디렉터리는 지원하지 않는다.
- 한 runtime 디렉터리에는 한 TaskCage 서비스 인스턴스만 둘 수 있다. 여러 인스턴스는 서로 다른 보호된
  runtime 디렉터리와 서로 다른 위임 cgroup root를 사용해야 한다.
- 같은 UID의 다른 프로세스 또는 root가 의도적으로 경로를 바꾸는 공격은 막지 않는다. 더 강한 호출자·
  파일 격리가 필요하면 별도 sandbox와 서비스 정책이 필요하다.
- stale socket과 잔여 cgroup 복구는 구현되어 있다. 실제 daemon crash 뒤 기존 manager 재사용과 잔여
  실행 제거는 Ubuntu 24.04 재시작 E2E에서 계속 검증한다.
- Registry는 메모리 기반이므로 시작 복구가 이전 snapshot이나 idempotency mapping을 되살리지 않는다.

## 관련 작업

- 계약 Issue: [#44](https://github.com/taskcage/taskcage/issues/44)
- 선행 socket 결정: [ADR 0004](0004-explicit-owner-only-uds-socket.md)
- fail-stop과 재시작 결정: [ADR 0003](0003-fail-stop-on-uncertain-cleanup-after-running.md)
- protocol 계약: [MVP API 명세](../api-mvp.md)
