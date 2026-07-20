# Linux 통합 시험

이 폴더의 시험은 실제 Linux cgroup v2와 `Delegate=yes`가 설정된 systemd 서비스 또는
scope가 필요하다. 일반 디렉터리에 제어 파일 이름만 만들어 둔 시험은 Linux 동작의
근거로 사용하지 않는다.

## 사전 검사와 실패 시 실행 차단

`preflight-fail-closed.sh`는 다음 내용을 확인한다.

1. 실제 cgroup v2 위임 경로를 찾는다.
2. `cpu`, `memory`, `pids` 제어기와 필수 사건·통계 파일을 확인한다.
3. 데몬을 `manager`로 옮긴 뒤 `/proc/self/cgroup`에서 이동 결과를 다시 확인한다.
4. 사용자 프로그램을 실행하지 않는 내부 `clone3` 검사로 원자적 cgroup 진입을 확인한다.
5. 일반 디렉터리를 위임 경로로 주면 아무 파일도 만들지 않고 실패하는지 확인한다.
6. controller 누락, 쓰기 권한 부족, 원자 진입 미지원은 가짜 검사 결과를 사용해
   외부 target 호출이 0건인지 확인한다.

4번의 내부 검사 프로세스는 사용자 target이 아니다. `clone3` 반환 직후 메모리 할당,
잠금, 비동기 실행기와 로그를 사용하지 않고 `_exit`만 호출한다. 이 과정으로 짧게 생기는
내부 자식 프로세스는 부모가 즉시 종료 상태를 회수한다.

실행 방법:

```bash
bash integration-tests/preflight-fail-closed.sh
```

Linux, systemd, cgroup v2 또는 일회성 위임 서비스를 만들 권한이 없으면 종료 코드 77로
건너뛴다. GitHub Actions의 Ubuntu 24.04 작업에서는 건너뛰지 않고 통과해야 한다.

## 작업별 제한과 원자적 실행

`cgroup-runner-smoke.sh`는 실제 위임 서비스 안에서 다음 내용을 확인한다.

1. 메모리, 프로세스 수와 CPU 상한을 작업 cgroup에 쓰고 다시 읽는다.
2. target을 `clone3(CLONE_INTO_CGROUP)`로 생성 순간부터 작업 cgroup 안에 둔다.
3. 정상 종료와 0이 아닌 종료 상태를 결과에 남긴다.
4. 대표 프로세스가 먼저 끝난 뒤 남은 자식과 손자를 `cgroup.kill`로 정리한다.
5. 벽시계 제한을 넘긴 작업을 전체 종료한다.
6. 없는 실행 파일과 작업 디렉터리 오류가 제한 없는 실행으로 이어지지 않는다.
7. `populated 0` 확인 뒤에만 통계와 정리 완료 결과를 반환한다.

실행 방법:

```bash
bash integration-tests/cgroup-runner-smoke.sh
```
