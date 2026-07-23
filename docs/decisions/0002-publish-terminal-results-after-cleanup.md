# 0002. 정리가 확인된 뒤 하나의 종료 결과를 공개한다

## 결정

TaskCage MVP는 외부 명령의 시작 시도부터 작업 cgroup 전체와 출력 reader 정리가 끝날 때까지를
하나의 내부 lifecycle로 관리한다. 공개 `FINISHED` 결과는 정리가 확인된 뒤 정확히 한 번만 만든다.

실행 파일 또는 작업 디렉터리 문제로 `execve`가 시작되지 못하면 `taskAccepted` 대신 기존
`task`/`FINISHED` 응답과 `EXECUTION_FAILED`를 반환한다. 이 경우 실제 target process 결과가
없으므로 `process.exitCode`와 `process.signal`은 모두 `null`이다.

## 종료 원인 판정

- timeout과 cancel은 먼저 관찰한 terminal 원인 하나가 이긴다. 늦은 원인은 기록을 덮어쓰지 않는다.
- 메모리와 PID 사건이 함께 증가하면 `memory.oom_kill`, `pids.max`, `memory.oom` 순으로 판정한다.
- 데몬 내부 오류는 위의 명시적 제어 원인과 커널 사건이 없고 안전한 정리를 완료한 경우에만
  `DAEMON_ERROR`로 공개할 수 있다.
- exit code 137이나 `SIGKILL`만으로 timeout, 메모리 초과 또는 PID 초과를 추측하지 않는다.
- Linux 표준 signal은 정식 이름을 사용하고 realtime signal은 `SIGRTMIN` 또는 `SIGRTMIN+N`으로
  표현한다. 이름 규칙에 없는 번호를 임의 문자열로 만들지 않는다.

## 정리 실패

다음 조건을 모두 확인해야 정리 완료로 취급한다.

1. direct child의 종료 상태를 회수했다.
2. 작업 cgroup을 전체 종료하고 `cgroup.events`의 `populated 0`을 확인했다.
3. 작업 cgroup을 제거했다.
4. stdout과 stderr reader를 모두 회수했다.

하나라도 확인하지 못하면 `FINISHED`를 공개하지 않는다. 격리 상태가 불확실한 실행기는 이후 작업을
fail-closed로 거절하고 오류 반환 경로와 Drop 방어에서 안전한 정리를 다시 시도한다. 외부에는 기존 `ENVIRONMENT_UNAVAILABLE` 또는
`INTERNAL_ERROR`만 사용하며 cleanup field, cleanup state 또는 새 오류 코드를 추가하지 않는다.

## 영향

- Java SDK는 새 응답 타입을 추가하지 않는다. `submitTask`가 기존 `task`/`FINISHED` 응답을 직접
  받을 수 있고, `EXECUTION_FAILED`의 process 두 필드가 `null`일 수 있음을 처리한다.
- Rust 실행 코어는 정리 성공 뒤에만 만들 수 있는 내부 결과를 lifecycle에 넘긴다.
- 진단용 `run-once`와 protocol task 실행은 같은 atomic cgroup 실행 코어를 사용하지만 wire 결과와
  진단 JSON 타입은 합치지 않는다.
- UDS 경로, 인증, registry 보관 상한, capacity 정책과 cancel handler는 이 결정에서 확정하지 않는다.
