# 컨테이너 기반 로컬 개발 환경

이 환경은 Linux cgroup v2 VM을 직접 준비하지 않고 실제 `taskcaged`와 Java SDK E2E를 실행하기 위한
개발 전용 구성이다. 운영용 TaskCage 배포나 보안 sandbox를 제공하지 않는다.

## 요구 사항

- Linux containers를 실행하는 Docker Engine 또는 Docker Desktop
- cgroup v2를 사용하는 Docker host
- Docker Compose 2.15 이상

Docker Desktop에서는 컨테이너가 Desktop의 Linux VM 안에서 실행된다. 따라서 여기서 검증하는 cgroup은
macOS나 Windows host가 아니라 해당 Linux VM의 cgroup이다.

## daemon 시작

저장소 루트에서 다음을 실행한다.

```bash
docker compose -f dev/container/compose.yml up --build --detach --wait taskcaged
```

상태와 로그는 다음처럼 확인한다.

```bash
docker compose -f dev/container/compose.yml exec taskcaged \
  taskcaged status --socket /run/taskcage/taskcaged.sock
docker compose -f dev/container/compose.yml logs taskcaged
```

daemon은 외부 TCP port를 열지 않는다. owner-only UDS는 Compose volume의
`/run/taskcage/taskcaged.sock`에 있으며 같은 구성의 테스트 컨테이너만 사용한다. 개발 구성은
`/taskcage-work/artifacts`를 owner-only Local Artifact root로 준비하고 opt-in `file-copy@1.0.0` Profile과
Protocol v2도 활성화한다.

## Java SDK E2E

다음 한 명령이 daemon을 build·기동하고 Java 단위 테스트와 실제 daemon E2E를 실행한 뒤 volume까지
정리한다.

```bash
bash dev/container/run-e2e.sh
```

Java 테스트 컨테이너는 daemon과 UDS·작업 volume 및 Docker Linux host의 PID namespace를 공유한다.
따라서 기존 Java E2E가 후손 PID 소멸까지 확인하고, Profile E2E가 입력 Artifact를 준비해 실제 daemon의
snapshot·실행·결과 publish를 검증할 수 있다. 테스트가 끝나면 daemon 컨테이너 내부에서 잔여 job
cgroup이 없는지도 검사한다. 최종 cgroup·systemd·릴리스 검증은 계속 Ubuntu 24.04 VM 또는 host 통합
테스트가 담당한다.

## 권한과 제한

`taskcaged` 컨테이너는 자신의 private cgroup namespace 안에서 cgroup을 생성하고 프로세스를 이동해야
하므로 `privileged: true`를 사용한다. 후손 PID 정리를 Java E2E에서 직접 검증하기 위해 daemon과 Java
테스트 컨테이너에는 `pid: host`도 적용한다. privileged 컨테이너와 host PID namespace 공유는 host에
광범위한 접근을 허용하므로 신뢰할 수 있는 개발 장비에서만 이 Compose 파일을 실행해야 한다.

- 운영 환경에서 사용하지 않는다.
- daemon에는 network namespace를 제공하지 않으며 port를 publish하지 않는다.
- host cgroup tree를 별도 bind mount하지 않는다.
- 인증 없는 TCP proxy를 UDS 앞에 추가하지 않는다.
- 격리와 권한 동작의 최종 근거로 사용하지 않고 Ubuntu 통합 테스트를 유지한다.

종료만 하려면 다음을 사용한다.

```bash
docker compose -f dev/container/compose.yml down --volumes
```
