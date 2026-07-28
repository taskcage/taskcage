# 0006. cgroup read-back 불일치는 INTERNAL_ERROR로 거절한다

## 문제

TaskCage는 target을 실행하기 전에 `cpu.max`, `memory.max`와 `pids.max`에 제한값을 쓰고 다시 읽어
요청값과 정확히 같은지 확인한다. 기존 API 본문은 값이 다르면 `LIMIT_EXCEEDS_POLICY`로 거절한다고
설명하지만, 오류 표는 이 코드를 요청이 daemon 배포 정책을 벗어난 경우로 정의한다. 현재 Rust submit
경로는 cgroup read-back 불일치를 `INTERNAL_ERROR`로 변환한다.

배포 정책 검증 실패는 클라이언트가 요청을 바꾸면 해결할 수 있다. 반면 정책을 통과한 뒤 발생한
read-back 불일치는 커널 또는 daemon 환경에서 요청한 격리를 증명하지 못한 상태다. 두 실패의 원인과
호출자 책임이 다르므로 Protocol v1에서 같은 오류로 표현하면 안 된다.

## 검토한 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| `LIMIT_EXCEEDS_POLICY` | 기존 API 본문 일부와 일치함 | 커널 또는 daemon 환경의 적용 검증 실패를 클라이언트 정책 위반으로 잘못 분류하며, 요청 수정으로 해결되는 것처럼 보일 수 있음 |
| `INTERNAL_ERROR` | 현재 Rust 매핑과 일치하고 정책을 통과한 뒤 발생한 예상하지 못한 적용 검증 실패라는 의미가 분명함 | 이 원인의 재시도 가능 여부와 정리 실패 시 daemon 동작을 별도로 정해야 함 |
| `ENVIRONMENT_UNAVAILABLE` | 안전한 자원 격리를 증명할 수 없다는 의미를 강하게 전달함 | 한 요청의 적용 실패를 전역 readiness 문제로 확대하고 capability와 이후 요청의 의미까지 바꿀 수 있음 |

## 결정

TaskCage MVP는 `INTERNAL_ERROR`를 사용한다.

- `LIMIT_EXCEEDS_POLICY`는 cgroup을 만들기 전에 요청 제한이 명시적인 daemon 배포 정책을 벗어났다고
  판정한 경우에만 사용한다. API 예시나 커널 표현 범위를 임의의 배포 정책으로 해석하지 않는다.
- 정책 검증을 통과했지만 cgroup 제어 파일에 쓴 값을 다시 읽었을 때 예상값과 다르면 해당
  `submitTask`를 `INTERNAL_ERROR`로 실패시킨다.
- 이 원인의 `retryable`은 `false`다. 같은 daemon에 즉시 재시도해도 요청한 제한을 안전하게 적용할 수
  있다는 근거가 없기 때문이다.
- target 프로세스는 시작하지 않는다. `taskAccepted`와 공개 `taskId`도 반환하지 않는다.
- 임시 cgroup과 Registry·idempotency·capacity 예약은 전체 정리를 확인한 뒤 원상 복구한다. 완전히
  정리된 실패는 `getTask`로 조회할 공개 작업을 남기지 않는다.
- 임시 cgroup 정리를 증명할 수 없으면 일반 `INTERNAL_ERROR` 응답으로 끝내지 않는다. ADR 0003의
  process-wide fail-stop 경로로 전환하고 신규 실행을 차단한다.
- read-back 불일치만으로 capability, readiness, 전역 상태, wire field 또는 새 오류 코드를 추가하지
  않는다.
- 시작 전 cgroup 환경과 필수 기능을 사용할 수 없을 때의 기존 `ENVIRONMENT_UNAVAILABLE` 계약은
  변경하지 않는다.

오류 응답은 기존 Protocol v1의 `error` 형식과 `INTERNAL_ERROR`만 사용한다. 내부 cgroup 경로,
controller 이름, 제어 파일, 예상값과 실제값은 구조화된 새 wire field로 공개하지 않는다. SDK는 사람이
읽는 `message`가 아니라 `code`와 `retryable`을 사용한다.

## effectiveLimits 의미

- `effectiveLimits`는 검증 전 요청을 단순히 복사했다는 뜻이 아니라, cgroup에 적용하고 다시 읽어
  요청값과 정확히 같음을 확인한 값이다.
- `taskAccepted`는 모든 제한값의 write와 read-back 검증이 성공하고 target 시작이 확인된 뒤에만
  반환한다.
- MVP에서는 커널이 요청값을 다른 값으로 정규화한 결과를 조용히 허용하지 않는다. 예상값과 실제값이
  다르면 작업을 시작하지 않는다.
- 향후 정규화된 값을 허용하려면 허용 범위, 비교 방법, `effectiveLimits`의 의미와 Java 호환성 시험을
  별도 계약으로 결정해야 한다.

exact-match 규칙 때문에 성공 응답의 `effectiveLimits` 값은 요청값과 같다. 그러나 그 값의 의미는
검증되지 않은 echo가 아니라 적용 성공을 증명한 결과다.

## 선택 이유

- **원인 구분:** 클라이언트의 배포 정책 위반과 daemon·커널의 적용 검증 실패를 분리한다.
- **fail-closed 실행:** 요청한 격리를 증명할 수 없으면 target을 시작하지 않는다.
- **좁은 실패 범위:** 정리가 확인된 불일치는 해당 요청만 실패시키며 capability를 임의로 바꾸지 않는다.
- **기존 안전 계약 유지:** 정리를 증명하지 못한 경우에만 기존 fail-stop 계약으로 확대한다.
- **Protocol v1 호환성:** 기존 `INTERNAL_ERROR`와 `retryable` field만 사용하므로 새 wire 값을 만들지
  않는다.

`LIMIT_EXCEEDS_POLICY`를 선택하지 않은 이유는 적용 실패를 호출자 책임으로 잘못 돌리기 때문이다.
`ENVIRONMENT_UNAVAILABLE`을 선택하지 않은 이유는 한 요청의 실패만으로 daemon 전체 readiness와 이후
요청의 동작을 바꾸는 별도 운영 계약이 필요하기 때문이다.

## 현재 구현과 후속 작업

현재 Rust 구현은 다음 부분에서 이 결정을 따른다.

- `CgroupError::ValueMismatch`가 read-back 불일치를 구분한다.
- cgroup 생성 단계의 실패는 target 시작 전에 발생하며, 정리가 성공하면 임시 작업 cgroup을 제거한다.
- submit 경로는 Runner 실패를 `INTERNAL_ERROR`로 변환하고 handler는 `CAPACITY_EXHAUSTED` 이외 오류의
  `retryable`을 `false`로 만든다.
- cgroup rollback까지 실패한 `CleanupCombined`는 capacity를 재사용하지 않고 fail-stop 방어 경계를
  유지한다.

후속 Rust 작업에서는 공개 계약을 바꾸지 않고 다음을 타입과 시험으로 더 직접적으로 고정해야 한다.

- `CgroupError::ValueMismatch`를 주입한 실제 submit handler 시험에서 `INTERNAL_ERROR`와
  `retryable: false`를 확인한다.
- 같은 시험에서 target 시작, `taskAccepted`, 공개 task snapshot이 0건이고 Registry·idempotency·capacity
  예약이 안전하게 되돌아가는지 확인한다.
- 성공한 read-back 뒤에만 만들 수 있는 내부 검증 완료 값으로 `effectiveLimits` 생성 경계를 제한한다.
  현재 구현은 검증 성공을 실행 전제조건으로 두면서 요청값 복사본을 응답에 사용하므로 exact-match
  결과는 같지만, 이 provenance가 타입으로 강제되지는 않는다.
- rollback 실패를 주입해 일반 오류 응답으로 복귀하지 않고 fail-stop으로 전환하는지 확인한다.

이 ADR은 위 Rust 변경을 구현하지 않는다.

## 검증 방법

- 저장소 전체에서 `LIMIT_EXCEEDS_POLICY`, `INTERNAL_ERROR`, `effectiveLimits`와 read-back 설명을 검색해
  원인이 뒤바뀐 문장이 없는지 확인한다.
- 기존 Protocol v1 JSON fixture를 파싱하고 Rust fixture round-trip 시험을 실행한다.
- 오류 코드 enum, 응답 field와 fixture 파일 집합이 늘지 않았는지 확인한다.
- 문서 diff에 Rust 제품 코드와 Java SDK 변경이 없는지 확인한다.
- 후속 Rust 시험에서 read-back 불일치의 target 시작과 공개 작업 생성이 0건인지 검증한다.

## 영향과 남은 위험

- Java SDK의 wire 모델과 공개 API는 바뀌지 않는다. 기존 `INTERNAL_ERROR`와 `retryable: false`를
  처리하면 된다.
- 이 결정은 daemon이 read-back 불일치 뒤에도 정상 서비스를 계속할 수 있다고 보장하지 않는다.
  임시 자원의 정리를 증명한 경우에만 해당 요청으로 실패 범위를 제한한다.
- 반복되는 read-back 불일치를 운영자가 어떻게 경보로 처리할지는 배포 운영 정책이며 Protocol v1에
  추가하지 않는다.
- 정규화된 cgroup 값을 향후 지원할 경우에는 별도 ADR, fixture와 Rust·Java 호환성 검증이 필요하다.

## 관련 작업

- 계약 Issue: [#66](https://github.com/taskcage/taskcage/issues/66)
- 직접 cgroup 제어: [ADR 0001](0001-direct-cgroup-v2-without-systemd.md)
- 정리 완료 뒤 결과 공개: [ADR 0002](0002-publish-terminal-results-after-cleanup.md)
- 정리 불확실 fail-stop: [ADR 0003](0003-fail-stop-on-uncertain-cleanup-after-running.md)
- protocol 계약: [MVP API 명세](../api-mvp.md)
