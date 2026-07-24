# 0003. RUNNING 이후 정리가 불확실하면 fail-stop으로 종료한다

## 문제

TaskCage는 작업 cgroup 안에서 target 시작을 확인한 뒤 `RUNNING`을 공개한다. 그 뒤 direct child
회수, 작업 cgroup 전체 종료, `cgroup.events`의 `populated 0`, cgroup 제거 또는 stdout·stderr
reader 회수 중 하나를 확인하지 못할 수 있다.

이 상태에서는 작업이 끝났거나 격리가 정리됐다고 증명할 수 없다. exit code, signal 또는 timeout만
보고 `FINISHED`를 만들면 실제 프로세스나 출력 reader가 남아 있는데도 완료로 보이는 모순이 생긴다.
반대로 아무 정책 없이 `RUNNING`을 유지하면 신규 요청, 동일 요청 재전송, 운영 복구와 데몬 재시작의
동작이 불명확해진다.

ADR 0002는 정리가 확인되기 전에는 `FINISHED`를 공개하지 않는다고 결정했다. 이 ADR은 그 원칙을
바꾸지 않고, 정리를 증명하지 못했을 때 데몬이 어떻게 멈추고 복구하는지 결정한다.

## 검토한 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| 정리 완료까지 `RUNNING`을 유지하며 무기한 degraded 상태로 둠 | 공개 상태가 거짓 `FINISHED`가 되지 않음 | 복구되지 않는 데몬이 영구적으로 남고 운영자가 장애 경계를 알기 어려움 |
| 정리 확인 없이 `DAEMON_ERROR` 또는 다른 `FINISHED` 결과를 만듦 | 클라이언트가 terminal 결과를 받음 | 정리와 필수 결과가 확인되지 않았는데 완료로 위장해 `FINISHED`의 의미를 깨뜨림 |
| protocol v1에 cleanup 상태나 오류 코드를 추가함 | 불확실성을 wire에서 직접 표현할 수 있음 | Rust와 Java 계약, fixture와 상태 전이가 함께 늘어나며 MVP 범위를 확장함 |
| 제한된 복구를 시도하고 실패하면 fail-stop으로 종료함 | 새 실행을 막고 기존 wire 계약과 `FINISHED`의 진실성을 함께 지킴 | 데몬 재시작이 필요하고 메모리 Registry 상태를 잃음 |

## 결정

TaskCage MVP는 네 번째 선택지를 사용한다. `RUNNING` 공개 이후 안전한 최종 결과를 만들 수 없는
오류가 발생하면 process-wide 정리 불확실 상태로 전환한다. 같은 프로세스가 살아 있는 동안 이 상태는
정상 상태로 되돌아가지 않는 단방향 상태다. 제한된 복구의 성공 여부와 관계없이 이 프로세스는 정해진
종료 기한에 0이 아닌 종료 코드로 종료한다.

### 공개 상태의 진실성

- 정리 완료를 증명하지 못한 작업은 `FINISHED`로 전환하지 않는다.
- exit code, signal 또는 timeout만으로 정리 완료를 추측하지 않는다.
- 필수 `process`, `timing`, `usage`, `output` 근거가 없으면 빈 값이나 추정값으로 채우지 않는다.
- 정리 불확실 상태를 `EXITED`나 `DAEMON_ERROR`로 위장하지 않는다.
- `DAEMON_ERROR`는 안전한 정리와 필수 결과 근거를 모두 확인한 경우에만 사용할 수 있다.
- protocol v1에 `CLEANUP_FAILED`, `UNKNOWN`, `DEGRADED` 같은 상태나 오류 코드를 추가하지 않는다.

### 신규 실행 차단과 기존 조회

정리 불확실 상태에서는 다음을 지킨다.

- 새로운 `clientRequestId`의 `submitTask`를 시작하지 않는다.
- task ID, Registry 항목, cgroup과 process를 새로 만들지 않는다.
- 신규 제출에는 기존 `ENVIRONMENT_UNAVAILABLE`을 반환한다.
- `getCapabilities.cgroupV2Ready`는 `false`를 반환한다.
- `getTask`는 이미 저장된 snapshot을 그대로 반환한다. 안전한 `FINISHED`를 저장하기 전까지 불확실한
  작업은 `RUNNING`으로 남는다.
- 같은 `clientRequestId`와 같은 본문의 재전송은 새 실행 없이 기존 `taskId`와 현재 저장된 snapshot을
  반환한다.
- 같은 `clientRequestId`와 다른 본문은 기존처럼 `IDEMPOTENCY_CONFLICT`를 반환한다.
- 향후 capacity가 추가돼도 불확실한 작업의 실행 슬롯을 정상 반환한 것으로 처리하지 않는다.

### 하나의 종료 기한과 제한된 내부 복구

- 작업이 최초 cleanup 단계에 들어가는 단조시간에 그 작업의 내부 cleanup timeout을 한 번만 더해
  절대 cleanup deadline을 만든다. 정리 불확실성을 관찰하면 새 시간을 더하지 않고 이 deadline을
  process-wide 종료 deadline으로 승계한다.
- 최초 실패 처리부터 같은 작업의 재시도, 다른 활성 작업 정리, 결과 저장, 제한된 조회 유예와 마지막
  종료 방어까지 모두 같은 deadline을 사용한다.
- 단계마다 timeout을 새로 시작하거나 deadline을 연장하지 않는다. 한 단계가 시간을 많이 사용하면
  다음 단계는 남은 시간만 사용할 수 있다.
- 공개 cleanup timeout 설정과 별도 공개 시간 예산은 추가하지 않는다.
- 복구 중에도 신규 작업을 시작하지 않는다.
- direct child 회수, 작업 cgroup 전체 종료, `populated 0`, cgroup 제거와 출력 reader 회수를 모두
  확인해야 정리 성공이다.
- 모든 최종 결과 근거를 보존한 채 복구에 성공한 경우에만 기존 lifecycle을 통해 `FINISHED`를
  정확히 한 번 저장한다.
- 정리만 성공하고 process, usage 또는 output 근거를 잃었다면 임의의 결과를 만들지 않는다.

### 다른 활성 작업의 처리

한 작업이 process-wide 정리 불확실 상태를 만들면 데몬은 다른 `RUNNING` 작업도 방치하지 않는다.

- 새로운 UDS 연결과 신규 작업 수락을 중단한다.
- 이미 활성 상태인 모든 작업에 whole-cgroup 종료와 정리를 시작한다. 개별 PID 종료 fallback은 사용하지
  않는다.
- 작업별로 새 timeout을 부여하지 않고 같은 process-wide deadline 안에서 가능한 한 동시에 정리를
  시작한다.
- direct child, cgroup과 output reader 정리 및 필수 결과 근거를 모두 확인한 작업만 기존 lifecycle로
  `FINISHED`를 정확히 한 번 저장한다.
- fail-stop 때문에 종료한 다른 작업은 안전한 정리와 결과 근거를 확인한 경우 기존 `DAEMON_ERROR`로
  분류한다. 이미 먼저 기록된 timeout, cancel 또는 커널 사건이 있다면 ADR 0002의 우선순위를 유지한다.
- 안전한 결과를 만들지 못한 작업은 `RUNNING` snapshot을 추정 결과로 덮어쓰지 않는다.
- capacity 슬롯은 정상 서비스에 반환하지 않는다. 프로세스 종료가 모든 내부 실행 상태를 폐기한다.

### 제한된 조회 유예와 제어된 종료

데몬은 복구 성공 여부와 관계없이 같은 process-wide deadline에 fail-stop 종료한다.

1. 신규 UDS 연결과 신규 작업은 받지 않는다.
2. deadline이 남아 있는 동안 이미 연결된 클라이언트의 `getCapabilities`, `getTask`와 idempotency
   재전송을 처리한다. 새로운 `clientRequestId`의 제출에는 `ENVIRONMENT_UNAVAILABLE`을 반환하고,
   같은 ID의 같은 본문에는 현재 snapshot을, 다른 본문에는 `IDEMPOTENCY_CONFLICT`를 반환한다. 이
   응답들은 task ID, Registry 항목, cgroup과 process를 만들지 않으며 deadline을 연장하지 않는다.
3. 안전한 결과를 만든 작업은 유예 중 `FINISHED`로 조회할 수 있다. 결과를 만들지 못한 작업은
   `RUNNING`으로 남는다.
4. 치명적 수준의 구조화 로그에 관련 `taskId`, 실패 단계, 재시도 결과와 정리하지 못한 항목을 남긴다.
5. 명령 인자 전체, 환경 변수 값, stdout과 stderr 원문처럼 민감할 수 있는 값은 기록하지 않는다.
6. deadline의 남은 범위에서 Drop 방어와 종료 경로의 마지막 안전한 정리를 시도한다.
7. deadline에 도달하거나 모든 활성 작업 정리가 끝나고 기존 연결이 모두 닫히면 0이 아닌 종료 코드로
   종료한다.

연결 중인 클라이언트는 응답을 받지 못하고 연결이 종료되면 기존 `DAEMON_UNAVAILABLE`로 처리한다.
이는 데몬이 보내는 새 JSON 오류가 아니다.

### 재시작 복구와 idempotency 한계

- 데몬은 UDS 요청을 받기 전에 남아 있는 TaskCage cgroup을 검색하고 정리한다.
- 잔여 cgroup의 `populated 0`과 제거를 확인하기 전에는 요청을 수락하지 않는다.
- 시작 복구에 실패하면 준비됐다고 보고하지 않고 신규 작업을 시작하지 않는다.
- MVP Registry는 메모리 기반이므로 재시작 전 snapshot과 idempotency mapping을 복구하지 않는다.
- 재시작 뒤 이전 `taskId` 조회는 `TASK_NOT_FOUND`가 될 수 있다.
- 같은 `clientRequestId` 재제출은 새 작업을 만들 수 있지만, 이전 잔여 cgroup 정리가 끝난 뒤에만
  허용한다.
- 잔여 작업과 새 작업의 동시 실행은 막지만, 장애와 재시작을 가로지르는 정확히 한 번 실행은
  보장하지 않는다.

## 선택 이유

- **안전성:** 격리 상태를 모르는 동안 보호되지 않은 새 작업을 실행하지 않는다.
- **상태 진실성:** `RUNNING`은 정리가 확인되지 않았음을 숨기지 않고, `FINISHED`는 전체 정리가
  확인됐다는 의미를 유지한다.
- **protocol v1 호환성:** 기존 상태, 응답과 오류 코드만 사용하므로 fixture와 Java wire 모델을
  변경하지 않는다.
- **운영 복구성:** 복구 성공 여부와 관계없이 절대 deadline에 비정상 종료해 supervisor가 재시작
  복구를 수행하게 하며, 영구적으로 신규 작업을 거절하는 프로세스를 남기지 않는다.
- **MVP 범위:** Registry 영속화와 분산 exactly-once 없이도 재시작 전 잔여 cgroup 정리로 동시 중복
  실행을 막는다.

## 영향과 한계

- 정리 불확실 상태에 들어간 데몬은 복구가 성공해 정확한 결과를 저장하더라도 정상 상태로 돌아가지
  않고 같은 절대 deadline에 종료한다.
- 복구가 일찍 끝난 경우 기존 연결은 남은 deadline 범위에서 최종 snapshot을 조회할 수 있다. 별도의
  조회 유예시간을 더해 전체 종료 시간을 늘리지 않는다.
- 한 작업의 불확실성이 다른 활성 작업의 whole-cgroup 종료를 일으킬 수 있다. 안전한 결과를 만든
  작업만 `FINISHED`로 공개한다.
- 클라이언트는 새 wire 타입을 구현할 필요가 없지만, 데몬 연결 종료 뒤 재연결과
  `DAEMON_UNAVAILABLE`을 기존 규칙대로 처리해야 한다.
- 재시작 뒤 이전 작업 조회와 idempotency mapping은 사라질 수 있다.
- supervisor의 재시작 정책과 운영 경보 전달 방식은 배포 설정의 책임이며 protocol v1에 넣지 않는다.

## 검증 방법

- ADR 0002와 API 명세에서 정리 확인 전 `FINISHED`를 허용하는 문장이 없는지 검토한다.
- protocol v1의 field, state, response type과 error code 목록이 늘지 않았는지 확인한다.
- 불확실 상태에서 신규 submit, 기존 조회, 동일·충돌 재전송의 동작을 각각 검토한다.
- 최초 cleanup 시작부터 마지막 종료 방어까지 하나의 단조시간 deadline만 사용하는지 검토한다.
- 동시에 실행 중인 다른 작업이 whole-cgroup 정리되고, 근거가 완전한 결과만 공개되는지 검토한다.
- 복구 성공 뒤에도 deadline에 비정상 종료해 영구 거절 상태가 남지 않는지 검토한다.
- 재시작 전후 Registry와 idempotency 한계가 문서에 드러나는지 확인한다.
- 로그 요구사항에 명령 인자, 환경 변수 값과 출력 원문이 포함되지 않는지 확인한다.

## 후속 구현 작업

- process-wide 정리 불확실 상태를 capability와 submit gate에 연결한다.
- 최초 cleanup 시작 시 하나의 단조시간 절대 deadline을 만들고 모든 정리 단계에 전달한다.
- 기존 cleanup timeout 안에서 같은 작업과 모든 활성 작업을 함께 정리하는 fail-stop coordinator를
  구현한다.
- UDS 신규 연결·작업 수락 중단, 기존 연결의 제한된 조회와 0이 아닌 제어 종료 경로를 구현한다.
- 데몬 시작 시 잔여 TaskCage cgroup을 검색·정리하고 완료 전에는 UDS를 열지 않는다.
- capacity가 추가될 때 불확실한 작업의 슬롯을 반환하지 않는지 검증한다.
- 실제 Linux cgroup v2 환경에서 복구 성공, 복구 실패, 재시작 정리와 잔존 0건을 검증한다.

이번 ADR에서는 Runner, Registry, UDS 서버, cleanup retry, 시작 복구, capacity와 cancelTask를
구현하지 않는다. Registry 영속화와 재시작을 가로지르는 정확히 한 번 실행도 MVP 범위 밖이다.

## 관련 작업

- 계약 Issue: [#30](https://github.com/taskcage/taskcage/issues/30)
- 선행 결정: [ADR 0002](0002-publish-terminal-results-after-cleanup.md)
