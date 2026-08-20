# 컨테이너 기반 로컬 개발 환경

이 환경은 Linux cgroup v2 VM을 직접 준비하지 않고 실제 `taskcaged`와 Java SDK E2E를 실행하기 위한
개발 전용 구성이다. 운영용 TaskCage 배포나 보안 sandbox를 제공하지 않는다.

## Capsule MVP에서의 역할

Capsule-first MVP의 권장 개발 경험은 Docker Compose daemon에 Java `ExternalRunner`가 연결해 Capsule을
실행하는 것이다. Docker Desktop 사용자도 macOS/Windows가 아니라 그 안의 Linux VM에서 cgroup 제한과
process tree cleanup을 확인할 수 있다.

현재 기본 `taskcaged` service는 같은 Compose volume을 공유하는 Local UDS E2E를 제공한다. `remote-taskcaged`
profile은 별도 Compose network에서 개발용 CA·TLS를 사용하는 Remote Protocol E2E를 제공한다. 다음 MVP 변경은
이 TLS 경로를 FFmpeg Capsule 개발 sample과 Java ExternalRunner의 기본 검증 경로로 완결하는 것이다. 그 전까지
기본 UDS Compose를 TLS Capsule 개발 경험으로 설명하지 않는다.

개발용 CA는 SDK가 명시적으로 신뢰해야 하며 hostname 검증을 끄거나 모든 인증서를 신뢰해서는 안 된다.
이 CA, 인증서, service credential은 test fixture 전용이고 운영에서 재사용하면 안 된다.

## 요구 사항

- Linux containers를 실행하는 Docker Engine 또는 Docker Desktop
- cgroup v2를 사용하는 Docker host
- Docker Compose 2.15 이상

Docker Desktop에서는 컨테이너가 Desktop의 Linux VM 안에서 실행된다. 따라서 여기서 검증하는 cgroup은
macOS나 Windows host가 아니라 해당 Linux VM의 cgroup이다.

## 현재 Local UDS daemon 시작

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
Protocol v2도 활성화한다. 컨테이너의 Ubuntu FFmpeg는 시작할 때 검증 가능한 Runtime Package로 import되고
그 Package를 참조하는 서명된 `ffmpeg-audio-to-wav@1.0.0` Bundle도 import된다. daemon은 Bundle catalog에서
Profile을 찾아 실행한다. 이 Bundle과 서명 키는 개발 E2E 전용으로 컨테이너 기동 때 생성되며, 공개 배포 artifact가
아니다.

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

## 현재 Remote TLS Java E2E

Remote Protocol v1의 실제 TCP/TLS 경로는 별도 daemon과 Java test runner 컨테이너로 검증한다.

```bash
bash dev/container/run-remote-e2e.sh
```

이 구성은 daemon에만 `privileged: true`와 private cgroup namespace를 부여한다. Java runner는 일반 Compose
network에서 TLS 1.3과 ALPN `taskcage/remote/1`로 daemon DNS에 연결하고, test-only CA·service account로 인증한다.
`file-copy@1.0.0`와 Compose가 import한 `ffmpeg-audio-to-wav@1.0.0` Capsule을 통해 Artifact upload → Capsule
실행 → output download를 검증한다. FFmpeg Capsule은 정상 실행·timeout·cancel 뒤 cleanup을 포함한다.
인증서와 secret은 test fixture 전용이며 운영 credential이나 인증서로 재사용하면 안 된다.

## FFmpeg Binding 예제

다음 명령은 실제 FFmpeg Binding 예제 하나만 실행하고 결과 Artifact 경로를 출력한다.

```bash
bash dev/container/run-ffmpeg-example.sh
```

예제 코드는 [`examples/ffmpeg-java`](../../examples/ffmpeg-java/README.md)에 있다. 애플리케이션은 FFmpeg
실행 파일이나 argv를 전달하지 않고 typed request만 Binding에 전달한다.

## 권한과 제한

Compose는 host architecture에 맞는 native image를 build한다. Runtime Package는 `linux/x86_64/gnu`와
`linux/aarch64/gnu`를 지원한다. ARM Mac에서는 `linux/amd64` emulation을 강제하지 말고 native ARM64
Docker Desktop Linux VM에서 실행해야 한다.

`taskcaged` 컨테이너는 자신의 private cgroup namespace 안에서 cgroup을 생성하고 프로세스를 이동해야
하므로 `privileged: true`를 사용한다. 후손 PID 정리를 Java E2E에서 직접 검증하기 위해 daemon과 Java
테스트 컨테이너에는 `pid: host`도 적용한다. privileged 컨테이너와 host PID namespace 공유는 host에
광범위한 접근을 허용하므로 신뢰할 수 있는 개발 장비에서만 이 Compose 파일을 실행해야 한다.

- 운영 환경에서 사용하지 않는다.
- Local UDS daemon에는 network namespace를 제공하지 않으며, Remote test daemon도 host port를 publish하지 않는다.
- host cgroup tree를 별도 bind mount하지 않는다.
- 인증 없는 TCP proxy를 UDS 앞에 추가하지 않는다.
- 격리와 권한 동작의 최종 근거로 사용하지 않고 Ubuntu 통합 테스트를 유지한다.

종료만 하려면 다음을 사용한다.

```bash
docker compose -f dev/container/compose.yml down --volumes
```
