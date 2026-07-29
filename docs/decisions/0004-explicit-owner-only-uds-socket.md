# 0004. 명시적 절대 경로에 owner-only UDS를 연다

## 문제

TaskCage protocol v1은 Unix domain socket의 `SOCK_STREAM`과 length-prefixed JSON을 사용하지만,
데몬이 socket 파일을 어디에 만들고 어떤 권한으로 관리하는지는 정하지 않았다. 잘못된 기본 경로,
기존 파일의 무조건 삭제 또는 임의의 UID/GID 변경은 운영자의 파일을 손상하거나 예상하지 못한
호출자에게 데몬을 노출할 수 있다.

## 검토한 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| 고정된 기본 경로와 계정 정책 사용 | 실행 인자가 단순함 | 배포 환경마다 다른 경로·서비스 계정을 protocol 구현이 임의로 결정함 |
| 기존 경로를 지우고 다시 bind | crash 뒤 자동 복구가 쉬움 | 일반 파일, symlink, 활성 socket 또는 다른 프로세스의 socket을 삭제할 수 있음 |
| 절대 경로를 명시적으로 받고 기존 경로가 있으면 거절 | 파일 소유권과 배포 설정의 책임이 분명함 | crash 뒤 stale socket은 운영자나 후속 복구 절차가 처리해야 함 |

## 결정

TaskCage MVP는 세 번째 선택지를 사용한다.

- 서비스 시작 시 daemon socket 경로를 절대 경로로 명시적으로 받는다. 기본 경로는 정하지 않는다.
- socket 파일 mode를 owner-only `0600`으로 설정하고 실제 mode를 다시 확인한 뒤 요청을 받는다.
- daemon은 socket을 위해 UID 또는 GID를 변경하지 않는다. 서비스 계정과 상위 디렉터리 권한은 배포
  설정의 책임이다.
- 일반 bind 전에 경로가 일반 파일, 디렉터리, symlink, 활성 socket 또는 stale socket으로 존재하면
  시작을 거절한다. 어떤 기존 경로도 임의로 삭제하거나 덮어쓰지 않는다.
- 정상적으로 bind한 socket의 device와 inode를 기억한다. 종료 시 같은 socket인 경우에만 제거하며,
  경로가 바뀌었거나 다른 파일로 교체됐으면 삭제하지 않는다.
- crash 뒤 남은 stale socket은 [ADR 0005](0005-own-startup-recovery-before-removing-stale-socket.md)의
  시작 소유권과 신원 검증을 모두 통과한 경우에만 startup recovery가 제거할 수 있다. 이 절차는 일반
  bind가 기존 경로를 삭제하도록 허용하지 않는다.

이 결정은 peer credential 인증이나 사용자 인증 protocol을 추가하지 않는다. MVP의 접근 경계는 서비스
계정, 상위 디렉터리 권한과 socket mode `0600`이다.

## 선택 이유

- **파일 안전성:** TaskCage가 만들지 않은 경로를 삭제하지 않는다.
- **최소 권한:** owner 이외의 접근을 기본적으로 허용하지 않으면서 배포 계정을 daemon이 바꾸지 않는다.
- **명시적 운영 설정:** 배포 환경마다 다른 `/run` 하위 경로를 protocol 계약에 고정하지 않는다.
- **좁은 MVP 범위:** 인증 protocol을 이번 서버 구현에 섞지 않는다. stale socket 복구는 별도 ADR의
  fail-closed 경계를 먼저 확정한 뒤 구현한다.

## 검증 방법

- 상대 경로를 거절한다.
- bind 뒤 socket mode가 `0600`인지 확인한다.
- startup recovery 소유권을 얻지 않은 일반 bind는 기존 일반 파일, symlink와 socket을 삭제하지 않고
  시작을 거절한다.
- 정상 shutdown 뒤 자신이 bind한 동일 device·inode의 socket만 제거한다.
- bind 뒤 경로가 교체되면 교체된 파일을 삭제하지 않는다.
- 실제 Linux Unix domain socket에서 연결과 frame 처리를 검증한다.

## 영향과 남은 위험

- 배포 설정은 socket의 절대 경로, 상위 디렉터리와 서비스 계정을 준비해야 한다.
- 현재 구현에서는 crash 뒤 stale socket이 남으면 다음 시작을 fail-closed로 거절한다. 후속 startup
  recovery 구현은 ADR 0005가 정한 lock, 부모 디렉터리, socket 신원과 연결 결과를 모두 검증해야 한다.
- owner 계정 안의 세부 호출자 구분은 하지 않는다. peer credential authorization이 필요하면 별도 ADR과
  protocol·운영 영향을 검토한다.
- 원격 TCP, TLS와 네트워크 인증은 MVP 범위 밖이다.

## 관련 작업

- 구현 Issue: [#40](https://github.com/taskcage/taskcage/issues/40)
- startup recovery 결정: [ADR 0005](0005-own-startup-recovery-before-removing-stale-socket.md)
- protocol 계약: [MVP API 명세](../api-mvp.md)
