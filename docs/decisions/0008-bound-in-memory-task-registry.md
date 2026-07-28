# 0008. in-memory Task Registry의 작업 수를 명시적으로 제한한다

## 문제

TaskCage는 실행 중인 작업과 완료 결과, `clientRequestId` 비교 본문을 메모리에 보관한다. 완료 결과는
최소 10분 보존하지만 서로 다른 요청 수에는 상한이 없어서, 요청이 계속 들어오면 실행 동시성 한도와
무관하게 Registry 메모리가 증가할 수 있었다. UDS 연결 상한도 transport handler만 제한하므로 보존
결과의 메모리를 보호하지 않는다.

## 검토한 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| `maxConcurrentTasks`를 Registry 상한으로 재사용 | 설정이 하나임 | 실행이 끝난 결과를 10분 보존할 공간이 없어 두 계약이 충돌함 |
| byte 단위 메모리 예산을 계산 | 실제 메모리 양에 더 가까움 | allocator와 자료구조 overhead를 정확히 계산하기 어렵고 구현이 복잡함 |
| 별도 작업 수 상한을 명시 | 논리 작업 수와 보존 계약을 단순하고 결정적으로 제한함 | 요청별 크기가 달라 정확한 byte 상한은 아님 |

## 결정

MVP는 0보다 큰 `max-registry-tasks`를 `taskcaged serve`의 필수 내부 설정으로 사용한다. 공개 기본값은
두지 않으며 이 값은 `maxConcurrentTasks` 이상이어야 한다. `getCapabilities`와 Protocol v1에는 새 field를
추가하지 않는다.

Registry의 `clientRequestId` 요청 인덱스를 논리 작업 수의 권위 있는 컬렉션으로 사용한다. 이 인덱스는
다음 상태를 작업마다 정확히 한 번 포함한다.

- RUNNING 공개 전 예약
- RUNNING 작업
- 최소 보존 기간 안의 FINISHED 결과
- RUNNING 공개 뒤 cleanup을 증명하지 못한 작업

별도 원자 카운터를 두지 않는다. 예약, 상태 전환, rollback과 만료가 사용하는 같은 Registry 잠금 안에서
컬렉션 길이를 확인해 카운터와 인덱스가 어긋날 가능성을 없앤다.

## submit 판정 순서

검증된 요청은 같은 잠금 구간에서 다음 순서로 판정한다.

1. 최소 보존 기간을 지난 FINISHED 항목을 정리한다.
2. 같은 `clientRequestId`가 있는지 확인한다.
3. 같은 본문이면 Registry가 가득 차도 기존 작업을 반환한다.
4. 다른 본문이면 `IDEMPOTENCY_CONFLICT`를 반환한다.
5. 새로운 ID일 때만 Registry 상한을 확인한다.
6. 공간이 있을 때만 task ID와 reservation을 만든다.

상한인 새 요청은 기존 `CAPACITY_EXHAUSTED`, `retryable: true`를 사용한다. message는
`task registry retention capacity is exhausted`로 구분할 수 있지만 SDK 분기 기준은 error code다. 이
거절에서는 task ID 생성, Registry 두 인덱스, 실행 capacity permit, Runner, cgroup과 process side effect가
모두 0이어야 한다. 대기열이나 조기 FINISHED eviction은 만들지 않는다.

## 슬롯 회수와 보존

- RUNNING 공개 전 실행 슬롯 획득 실패, 실행 준비 실패와 read-back 불일치가 안전하게 rollback되면
  예약과 두 인덱스를 함께 제거해 Registry 슬롯을 되돌린다.
- FINISHED는 완료 시점부터 최소 10분 보존한다. 정확히 10분 경계에서는 유지하고 그보다 지난 항목만
  다음 Registry 접근에서 만료시킨다.
- RUNNING 작업, 예약과 cleanup 불확실 작업은 공간 확보를 위해 제거하지 않는다.
- RUNNING 이후 cleanup 불확실성은 기존 fail-stop 계약을 따르며 Registry 슬롯을 정상 서비스에
  반환하지 않는다.

## 메모리 경계

Protocol frame은 최대 1 MiB이고 출력 tail도 각 요청의 검증된 상한 안에 있다. 따라서 보존 작업 수를
제한하면 Registry가 소유하는 요청 본문, snapshot, 출력과 인덱스 메모리는 설정값에 비례해 제한된다.
MVP는 allocator overhead를 포함한 정확한 byte accounting이나 byte 기반 eviction을 보장하지 않는다.

## 선택 이유

- 10분 결과 보존과 멱등성 판정을 유지하면서 새 요청 admission만 닫는다.
- 하나의 잠금과 권위 있는 컬렉션으로 마지막 슬롯 경쟁을 결정할 수 있다.
- 실행 capacity와 UDS 연결 capacity의 의미를 바꾸지 않는다.
- 새 wire field와 오류 코드 없이 기존 SDK 계약으로 표현한다.

## 영향과 남은 위험

- 배포자는 실행 동시성, 완료율과 10분 동안 예상되는 결과 수를 고려해 Registry 상한을 정해야 한다.
- 작업별 요청 크기가 다르므로 process RSS의 정확한 byte 상한은 아니다.
- 상한에 오래 도달하면 새 ID는 완료 결과가 만료되거나 안전한 예약이 rollback될 때까지 거절된다.
- Java SDK wire 모델과 공개 API는 바뀌지 않는다.

## 검증 방법

- 예약, RUNNING, FINISHED와 cleanup 불확실 작업이 각각 한 슬롯인지 확인한다.
- 마지막 한 슬롯의 동시 요청에서 owner가 하나만 생기는지 확인한다.
- 가득 찬 Registry에서 기존 멱등 요청과 conflict가 새 ID 거절보다 먼저 판정되는지 확인한다.
- 정확히 10분과 그 직후의 만료 경계를 확인한다.
- 반복 rollback 뒤 슬롯과 두 인덱스가 누적되지 않는지 확인한다.
- 실제 Linux `taskcaged serve`에서 FINISHED 두 건을 보존한 뒤 셋째 요청이 target과 cgroup 없이
  `CAPACITY_EXHAUSTED`가 되는지 확인한다.

## 관련 작업

- 구현 Issue: [#74](https://github.com/taskcage/taskcage/issues/74)
- UDS 연결 상한: [ADR 0007](0007-bound-concurrent-uds-connections.md)
- cleanup 불확실 fail-stop: [ADR 0003](0003-fail-stop-on-uncertain-cleanup-after-running.md)
- protocol 계약: [MVP API 명세](../api-mvp.md)
