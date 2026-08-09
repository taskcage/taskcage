# 0010. 배포 최대값과 SDK 안전 기본값을 분리한다

- Status: Proposed
- Date: 2026-08-10
- Related issue: [#102](https://github.com/taskcage/taskcage/issues/102)

## 문제

Protocol v1은 모든 Task 자원 예산을 요구하지만 daemon 배포가 허용하는 Task별 최대값과 SDK가 간단한
호출에 사용할 요청 기본값은 정의하지 않았다. 호출자가 임의로 큰 값을 보내도 구조 검증만 통과하면 task
cgroup 생성 단계까지 진행할 수 있었고, 운영자 정책과 SDK 편의값을 같은 의미로 오해할 여지도 있었다.

## 결정

daemon은 시작할 때 Task 하나의 CPU·memory·PID·벽시계 시간·stdout/stderr tail 최대값을 모두
명시적으로 받아야 한다. 누락되거나 0인 정책은 시작을 거절한다. 정책은 daemon 수명 동안 불변이며 변경은
restart로 적용한다.

구조적으로 유효한 `submitTask`의 요청 예산을 정책과 비교한다. 초과하면 Registry record, capacity permit,
task cgroup과 target process를 만들기 전에 기존 Protocol v1 `LIMIT_EXCEEDS_POLICY`, `retryable: false`로
거절한다. CPU는 부동소수점 없이 `requested quota * maximum period <= maximum quota * requested period`로
비율을 비교한다.

Ubuntu 설치 자산은 다음 명시적 최대값을 제공한다.

| 자원 | 배포 최대값 |
|---|---:|
| CPU | `200000/100000` (2 CPUs) |
| memory | `2147483648` (2 GiB) |
| PID | `128` |
| wall time | `900000ms` (15분) |
| stdout tail | `65536 bytes` |
| stderr tail | `65536 bytes` |

Java SDK의 `ResourceBudget.safeDefaults()`는 CPU `100000/100000`, memory 512 MiB, PID 32, 벽시계
2분, 출력 tail 각각 65,536 bytes를 반환한다. `TaskSpec(command)`가 이를 사용하고 기존 명시적 생성자는
override로 유지한다. 이 값은 daemon capability나 협상 결과가 아니며 더 낮은 배포 정책에서는 거절된다.
Protocol v1과 `getCapabilities`에 새 정책 field를 추가하지 않는다.

## 선택 이유

- 운영자 최대값과 호출자 편의값의 소유권을 분리한다.
- 제한을 적용할 수 없는 요청은 실행 side effect 전에 fail closed한다.
- CPU period가 다른 동등한 요청을 잘못 거절하지 않는다.
- Protocol v1 wire 호환성을 유지한다.
- 합리적인 SDK 기본값으로 첫 실행의 보일러플레이트를 줄이면서 명시적 override를 보존한다.

## 기각한 선택지

- **daemon 최대값을 SDK 기본값으로 사용:** Local transport에 policy discovery와 협상 의미를 추가하고 배포
  변경에 따라 SDK 동작이 암묵적으로 바뀐다.
- **초과값을 최대값으로 clamp:** 호출자가 요청한 계약과 실제 제한이 달라진다. TaskCage는 조용히 완화하거나
  변경하지 않고 명시적으로 거절한다.
- **합계 resource pool과 fairness까지 함께 구현:** A3는 Task별 admission 계약이다. 전체 CPU/memory 예약,
  overcommit, queue와 공정성은 별도 측정과 설계가 필요하다.
- **동적 reload:** RUNNING Task와 새 Task 사이 정책 버전 의미, 원자성, audit 계약이 먼저 필요하다.

## 검증

- 정책과 같은 값, period가 다른 같은 CPU 비율은 수락한다.
- 각 자원 최대값을 하나씩 넘긴 요청은 `LIMIT_EXCEEDS_POLICY`로 거절한다.
- 거절 시 core submit 호출과 요청 context side effect가 0임을 단위 테스트한다.
- 공유 Protocol fixture가 오류 코드와 `retryable: false`를 고정한다.
- Ubuntu systemd smoke가 설치 기본값보다 큰 요청을 보내 오류와 task cgroup 미생성을 확인한다.
- Java 테스트가 SDK 안전 기본값과 `TaskSpec(command)` 위임을 고정한다.

## 영향과 남은 위험

- 이 정책은 Task별 최대값만 보장한다. 동시 Task의 합계가 host 용량을 넘는 것을 방지하지 않는다.
- `getCapabilities`로 정책을 조회할 수 없으므로 호출자는 배포 문서와 오류를 통해 호환성을 확인한다.
- 설치 기본값은 대표 workload 측정 전의 Alpha 값이며 실제 사용 증거에 따라 새 ADR로 조정할 수 있다.
- systemd의 service-level 제한과 host의 다른 workload는 별도 운영 경계다.
