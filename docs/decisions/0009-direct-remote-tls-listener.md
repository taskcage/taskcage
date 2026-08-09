# 0009. Remote MVP는 taskcaged가 TLS endpoint를 직접 제공한다

- Status: Proposed
- Date: 2026-08-10
- Related issue: [#90](https://github.com/taskcage/taskcage/issues/90)

## 문제

Remote 연결은 TaskCage 제품 MVP의 아키텍처 계약이지만 현재 구현은 owner-only `0600` Local UDS와
Protocol v1만 제공한다. Remote 구현에 앞서 network endpoint, caller identity, authorization,
backpressure, 장애와 audit 책임을 어느 process가 소유하는지 결정해야 한다.

검토 대상은 다음 두 topology다.

1. `taskcaged`가 TCP + TLS/mTLS listener를 직접 제공한다.
2. 별도 Gateway가 Remote TLS와 caller authentication을 종료하고 Local daemon core로 전달한다.

이 ADR은 topology와 보안 책임의 경계를 결정한다. Remote wire schema, Artifact 전송 protocol,
SDK retry API와 구체적인 설정 field는 후속 계약에서 다룬다. 이 ADR만으로 현재 Local Protocol v1
framing을 network에 노출할 수 없다.

## 결정 기준

| 기준 | 직접 listener | 별도 Gateway |
|---|---|---|
| 배포와 경량성 | 하나의 daemon과 하나의 health/restart 경계 | process, hop, 설정과 health 경계가 하나씩 늘어남 |
| caller identity | TLS에서 얻은 identity를 authorization까지 end-to-end로 유지 | Gateway가 daemon에 identity를 다시 증명하는 별도 신뢰 protocol 필요 |
| Local UDS `0600` 보존 | 기존 Local endpoint를 그대로 유지할 수 있음 | Gateway가 같은 UID면 privilege 분리가 약하고, 다른 UID면 UDS 권한 계약 변경 필요 |
| attack surface | network/TLS parser와 cgroup 권한을 가진 core가 같은 process | edge parser를 core와 분리할 수 있음 |
| 장애 범위 | listener와 core가 같은 crash/restart 경계를 공유 | Gateway 장애와 daemon 장애를 분리할 수 있으나 상태 조합이 늘어남 |
| backpressure | endpoint와 core의 한도를 한 daemon에서 계층별로 적용 | 두 process 사이 queue와 중복 한도 정책 필요 |
| 성능 | 추가 hop과 identity 전달 encoding이 없음 | forwarding과 buffering 비용이 추가됨 |
| 향후 확장 | multi-tenant edge와 독립 scaling에는 불리함 | 독립 scaling, edge patch cadence와 trust zone 분리에 유리함 |

별도 Gateway의 격리 이점은 중요하지만 현재 owner-only UDS를 그대로 사용하려면 Gateway도 보통 daemon과
같은 UID로 실행해야 한다. 이 경우 process crash 경계는 나뉘어도 의미 있는 privilege boundary는 생기지
않는다. 다른 credential로 분리하려면 UDS 권한, Gateway-to-daemon 인증, caller identity assertion과 replay
방지 protocol을 먼저 추가해야 한다. 이는 Remote MVP의 경량성과 단일 runtime 운영 목표에 비해 큰 선행
비용이다.

## 제안 결정

Remote MVP는 `taskcaged` 안의 선택적 직접 TCP endpoint로 제공한다. endpoint는 TLS 1.3과 필수 mTLS로
server와 caller를 상호 인증한 뒤, 같은 daemon core의 Task 계약으로 요청을 전달한다.

이 결정은 `Status: Proposed`인 동안 구현 승인이 아니다. review에서 trust root 소유자, caller identity
형식과 fail-closed 조건을 승인하고 상태를 `Accepted`로 바꾸기 전에는 Remote protocol 또는 listener
구현을 시작하지 않는다.

## 배포와 privilege 경계

- Local UDS는 기존과 같이 owner-only `0600`이며 Remote endpoint와 별도 listener로 유지한다.
- Remote는 기본적으로 비활성이다. 암시적 network bind와 공개 bind 기본값을 두지 않는다.
- 운영자가 bind address, server certificate와 private key, client CA trust bundle, authorization policy,
  connection·in-flight·byte 상한을 모두 명시하고 검증을 통과해야 Remote readiness가 true가 된다.
- `0.0.0.0` 또는 `[::]` bind는 명시적으로 설정한 경우에만 가능하다.
- Remote endpoint와 daemon core는 같은 process와 OS credential을 사용한다. 이 topology는 network parser와
  cgroup 관리 권한 사이의 privilege separation을 제공하지 않는다.
- Local readiness와 Remote readiness는 구분한다. Remote 설정이나 credential이 안전하지 않으면 새 Remote
  연결은 거부하되, Local UDS의 기존 안전 조건이 충족되면 Local runtime까지 제한 없이 우회시키지 않는다.

## TLS와 caller identity

- Remote MVP의 최소 및 최대 protocol family는 TLS 1.3이다. TLS 1.2 이하는 거부한다.
- client는 운영자가 구성한 trust root와 기대한 server name을 사용해 server certificate의 DNS/IP SAN과
  `serverAuth` 용도를 검증한다. SAN 검증을 끄는 옵션을 제공하지 않는다.
- Remote caller는 client certificate를 반드시 제시한다. certificate chain은 운영자가 구성한 client CA와
  `clientAuth` 용도를 통과해야 한다.
- caller identity는 leaf client certificate의 단일 exact URI SAN이다. CN fallback, wildcard identity,
  부분 문자열과 대소문자 보정 matching을 허용하지 않는다.
- URI SAN이 없거나 둘 이상이라 identity가 모호하면 handshake 뒤 application request를 수락하지 않는다.
- deployment operator가 server key/certificate, client CA와 authorization policy의 trust owner다. private
  key는 daemon owner만 읽을 수 있어야 하며 secret과 certificate 원문을 log에 남기지 않는다.

TLS는 caller를 인증할 뿐 Task 실행 권한을 부여하지 않는다. handshake 성공과 authorization 성공은 서로
다른 판정이며, 인증된 caller에도 allow-all 기본 정책을 적용하지 않는다.

## authorization 경계

daemon의 authorization policy는 exact caller URI identity를 operation capability에 mapping한다. 최소
capability 경계는 다음과 같다.

- Task 제출·조회·취소
- Execution Profile 또는 Bundle 참조 사용
- Artifact 입력 업로드·조회·결과 다운로드
- Raw Command 제출
- 관리 operation과 credential reload

authorization은 요청의 모든 identity, operation, Profile/Bundle 참조와 Artifact metadata를 검증한 뒤,
다음 side effect보다 먼저 완료해야 한다.

- Task ID 또는 Registry record 생성
- task cgroup 생성과 실행 capacity 획득
- Artifact 임시 파일, target 파일 또는 cache entry 생성
- target process 시작

거부된 요청은 위 side effect가 0임을 검증할 수 있어야 한다. Remote Raw Command는 기본 거부하며 운영자가
caller별 capability로 명시적으로 허용한 경우에만 가능하다. 허용된 Raw Command도 argv 검증, Task 자원
정책, task cgroup 진입과 whole-task cleanup을 우회하지 않는다.

## credential rotation과 revocation

- credential과 authorization policy는 명시적인 관리 reload에서만 교체한다.
- 새 server certificate/key pair, client CA, denylist와 policy 전체를 먼저 검증한 뒤 하나의 snapshot으로
  원자적으로 교체한다. 부분 적용은 허용하지 않는다.
- reload가 실패하면 이전 snapshot을 메모리에 보존하더라도 Remote readiness를 false로 만들고 새 Remote
  handshake를 거부한다. 운영자가 유효한 전체 snapshot으로 다시 성공시켜야 readiness를 복구한다.
- MVP는 online OCSP 의존성을 두지 않는다. operator가 certificate fingerprint 또는 exact caller identity
  denylist를 관리하고 CA·policy snapshot과 함께 reload한다.
- revocation reload가 성공하면 해당 identity의 새 요청을 거부하고 활성 Remote connection도 종료한다.
  connection에는 명시적인 최대 수명을 두어 오래된 인증 상태가 무기한 유지되지 않게 한다.
- Local UDS caller는 기존 OS socket ownership 계약을 따르며 Remote certificate policy로 가장하지 않는다.

## backpressure 책임

Remote endpoint와 daemon core는 서로 다른 한도를 소유한다.

Remote endpoint가 소유하는 한도:

- 동시 TLS handshake 수와 handshake timeout
- 전체 및 caller별 connection 수와 connection 최대 수명
- connection별 및 전체 in-flight request 수
- frame, request, response, output stream과 Artifact 전송 byte 한도
- 느린 reader/writer와 미완료 upload timeout

daemon core가 소유하는 한도:

- Task 실행 동시성
- in-memory Task Registry 보존 capacity
- Task별 CPU·memory·PID·wall-clock 정책
- 향후 Profile, Package cache와 Artifact 저장 capacity

Remote connection 상한이 남아 있어도 core capacity가 없으면 새 Task를 만들지 않고 명시적인 retryable
거절을 반환한다. 반대로 Task capacity가 남아 있어도 handshake, connection, in-flight 또는 byte 상한을
초과한 Remote 요청은 core에 전달하지 않는다. 이 ADR은 구체적인 숫자나 공개 기본값을 승인하지 않는다.

## 연결 단절, crash와 restart

- Remote connection은 Task의 수명주기 owner가 아니다. 연결 단절만으로 이미 수락한 Task를 취소하지 않는다.
- caller가 응답을 잃었을 때 같은 요청을 안전하게 조회·재시도할 수 있도록 후속 Remote 계약은 caller-owned
  idempotency key와 request body identity를 wire에 포함해야 한다.
- authorization과 admission 전에 연결이 끊기면 Task side effect는 0이어야 한다.
- RUNNING 공개 뒤 연결이 끊기거나 daemon이 restart되면 기존 startup recovery, task cgroup ownership,
  whole-task cleanup과 결과 공개 순서를 유지한다.
- 직접 listener는 TLS endpoint와 core가 같은 crash domain임을 인정한다. listener failure가 안전한 Remote
  admission을 보장하지 못하면 Remote readiness를 닫고 새 Remote 요청을 거부한다.
- restart 뒤 response-loss의 정확한 상태와 retention 의미는 후속 idempotency API 계약에서 확정한다.

## audit 책임

daemon은 최소한 다음 보안 결정을 구조화해 기록한다.

- exact caller URI identity와 leaf certificate fingerprint
- 인증·authorization 결과와 거부 reason code
- request ID와 Task ID가 생성된 경우 그 Task ID
- operation 종류, listener와 연결 metadata
- credential/policy snapshot version과 reload 결과

command argv, Profile input value, Artifact byte, private key, bearer secret과 certificate 원문은 기본 audit
record에 남기지 않는다. audit 기록 실패가 admission을 닫아야 하는 배포인지 여부는 후속 운영 정책에서
명시해야 하며 조용히 log를 유실하는 기본값은 두지 않는다.

## 선택 이유

- TaskCage의 경량 단일 daemon 운영 목표를 유지한다.
- TLS에서 얻은 caller identity를 별도 forwarding trust protocol 없이 authorization까지 보존한다.
- Local UDS `0600` 계약을 변경하지 않는다.
- 기존 Task Registry, admission, fail-stop과 startup recovery를 하나의 core 책임으로 유지한다.
- Gateway의 실질적인 privilege separation을 위해 필요한 추가 local 인증 protocol을 성급하게 고정하지
  않는다.

## 기각한 선택지: 별도 Gateway

별도 Gateway는 Remote MVP 기본 topology로 선택하지 않는다. 같은 UID로 UDS에 연결하면 privilege 분리가
약하고, 다른 UID로 실행하면 기존 owner-only `0600` 계약을 바꾸면서 caller identity 전달과 인증 protocol을
새로 설계해야 한다. 또한 두 process의 capacity, buffering, readiness, restart와 audit 상관관계를 운영해야
한다.

다음 조건 중 하나가 현실 요구로 확인되면 Gateway 결정을 다시 검토한다.

- network edge와 cgroup-capable runtime이 서로 다른 trust zone에 있어야 한다.
- edge security patch cadence 또는 release ownership을 daemon과 분리해야 한다.
- multi-tenant/shared edge, 독립 scaling 또는 protocol translation이 필요하다.
- 독립 crash boundary의 이점이 추가 hop과 운영 비용보다 크다는 실측 근거가 있다.
- same-UID가 아닌 local channel과 검증 가능한 identity assertion protocol이 승인되었다.

## 검증 방법

후속 구현은 최소한 다음 증거를 제공해야 한다.

- TLS 1.2, unknown CA, 잘못된 server name, client certificate 누락, 잘못된 EKU, 만료 certificate와
  denylist 대상 certificate를 각각 거부한다.
- 모호한 client URI SAN, CN-only identity, wildcard authorization과 allow-all fallback을 거부한다.
- 모든 authentication·authorization 거부에서 Task record, task cgroup, capacity permit, Artifact 임시 파일과
  target side effect가 0이다.
- Remote를 명시하지 않은 시작에서 network listener가 없고 Local UDS가 계속 owner-only `0600`이다.
- handshake, connection, caller별/전체 in-flight와 byte 상한을 동시성·slow-client·flood test로 검증한다.
- credential/policy reload의 원자성, 실패 시 Remote readiness 차단, revocation 뒤 활성 연결 종료를 검증한다.
- daemon crash/restart와 응답 유실 뒤 같은 idempotency key가 중복 Task를 만들지 않음을 검증한다.
- 외부 Linux host에서 실제 encrypted Remote 연결을 검증한다. loopback test는 외부 host 증거를 대체하지
  않는다.

## 영향과 남은 위험

- public network parser와 cgroup 관리 권한이 같은 process와 crash domain에 남는다.
- operator가 certificate 발급, key 보호, CA rotation, denylist와 authorization policy를 운영해야 한다.
- online revocation을 제공하지 않으므로 operator reload 시간과 connection 최대 수명이 revocation 지연의
  상한이 된다.
- TLS와 mTLS는 filesystem, network, syscall 또는 악의적인 target code를 sandbox하지 않는다.
- Artifact integrity, lost-response retry, wire framing과 구체적 capacity 값은 아직 후속 계약이 필요하다.

## 후속 작업

이 ADR이 `Accepted`가 된 뒤에만 다음 계약을 순서대로 정의한다.

1. Remote Task와 authorization wire schema
2. Remote Artifact transfer, digest, staging과 cleanup
3. idempotency, disconnect와 response-loss 의미
4. Raw Command capability와 audit contract
5. direct TLS listener 구현과 외부 Linux host 검증

## 관련 문서

- [TaskCage 제품 철학과 용어](../product-philosophy.md)
- [Local Protocol v1 API 명세](../api-mvp.md)
- [Issue #90](https://github.com/taskcage/taskcage/issues/90)
