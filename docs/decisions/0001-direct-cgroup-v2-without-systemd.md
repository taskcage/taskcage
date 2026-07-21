# 0001. systemd 없이 직접 cgroup v2를 제어한다

## 결정

TaskCage MVP의 Rust 데몬은 systemd transient unit, `systemd-run`, DBus를 사용하지 않는다. 데몬이 cgroup v2 파일 인터페이스와 Linux 프로세스 API를 직접 사용해 작업별 cgroup을 생성·제한·관찰·정리한다.

## 배경

TaskCage의 핵심 가치는 외부 명령이 제한된 cgroup 안에서만 실행되도록 보장하고, timeout·취소·자원 초과 시 작업 전체를 정리한 뒤 원인을 반환하는 것이다. Ubuntu에서는 systemd가 일반적이지만, 제품의 실행·권한·장애 모델을 systemd unit과 DBus에 결합하지 않기로 했다.

## 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| systemd transient unit 사용 | unit lifecycle과 resource-control 기능을 재사용 | systemd·DBus 의존, unit 모델과 TaskCage API 모델을 함께 관리해야 함 |
| 직접 cgroup v2 제어 | 제품 계약에 맞는 cgroup lifecycle·통계·종료 원인을 직접 정의 가능 | cgroup 생성·정리·권한·경쟁 조건을 데몬이 책임져야 함 |

## 이유

- TaskCage는 systemd 관리 API의 래퍼가 아니라 Java SDK를 위한 작업 실행 계약을 제공한다.
- cgroup 이벤트와 프로세스 상태를 함께 사용해 종료 원인을 판정해야 한다.
- 작업 시작 시점부터 cgroup에 들어가도록 보장하고, 보장할 수 없는 환경에서는 실행을 거절해야 한다.
- 향후 systemd가 없는 cgroup v2 환경을 지원할 선택지를 유지한다.

## 구현 원칙

- 실행 전 작업 cgroup을 만들고 `cpu.max`, `memory.max`, `pids.max`를 설정한다.
- 원자적 cgroup 진입을 보장할 수 없으면 외부 명령을 실행하지 않는다.
- 종료·취소·timeout·오류 시 `cgroup.kill`로 작업 cgroup 전체를 정리한다.
- `cgroup.events`의 `populated 0`을 확인한 뒤 cgroup을 제거한다.
- 시작 시 cgroup v2 controller, `cgroup.kill`, 필요한 쓰기 권한을 점검하고, 충족하지 못하면 명시적으로 실패한다.

## 결과와 남은 위험

데몬 구현과 Linux 통합 테스트의 범위가 커진다. timeout, 메모리 제한, PID 제한, 자식 프로세스 정리, 데몬 재시작 뒤 잔여 cgroup 정리 시나리오를 실제 Linux 환경에서 검증해야 한다.
