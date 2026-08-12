# TaskCage

TaskCage는 신뢰된 외부 프로세스를 작업 단위로 실행하고, Linux cgroup v2로 자원과 수명주기를 관리하는 경량 런타임이다.

> **상태:** daemon과 Java SDK `0.1.0` Local Public Alpha prerelease가 공개됐다. daemon은
> [GitHub Release](https://github.com/taskcage/taskcage/releases/tag/taskcaged-v0.1.0), Java SDK는
> [Maven Central](https://repo1.maven.org/maven2/org/taskcage/taskcage-java-sdk/0.1.0/)에서 설치할 수 있다.
> `0.x`는 초기 개발 버전이며 공개 API와 운영 계약이 이후 minor 버전에서 변경될 수 있다.

제품의 장기 방향과 표준 용어는 [제품 철학과 용어](docs/product-philosophy.md)에서 정의한다. 이 README는
현재 구현하고 공개한 Local Public Alpha 범위를 설명한다.

## 해결하려는 문제

PDF·OCR·이미지·영상 변환, 브라우저 자동화, 컴파일 같은 외부 프로그램은 호출은 간단하지만 독립된 자원과 프로세스 트리를 가진다.

- 한 작업이 CPU·메모리·PID를 과도하게 사용한다.
- timeout, 취소 또는 호출 애플리케이션 종료 뒤 자식 프로세스가 남는다.
- 여러 작업이 겹치면 정상 요청까지 영향을 받는다.
- exit code만으로는 timeout, OOM, PID 제한 같은 종료 원인을 구분하기 어렵다.

TaskCage는 외부 프로세스를 단일 PID가 아니라 제한과 결과를 가진 **Task**로 다룬다.

> Task는 특정 실행 계약을 입력과 자원 정책으로 수행하는 일회성 작업이며, cgroup v2 실행 경계·프로세스 트리·상태·결과를 포함한다.

공개 계약에서 Task 하나는 task cgroup root 하나를 소유한다. 내부 하위 cgroup은 별도 공개 실행 단위가
아니며 daemon 구현 세부사항이다.

## 현재 Public Alpha

```text
Java application
    │
    ▼
TaskCage Java SDK
    │ Unix domain socket / Protocol v1
    ▼
taskcaged (Rust)
    │
    ├─ task cgroup: CPU · memory · PID · wall time
    ├─ external process tree
    └─ result: reason · usage · output tail
```

현재 범위는 다음과 같다.

- 단일 Linux 호스트의 `taskcaged`와 Java 17+ 애플리케이션
- cgroup v2의 `cpu`, `memory`, `pids` controller
- 작업별 CPU·메모리·PID·벽시계 시간 제한
- 호스트 단위 동시 실행 수 제한과 즉시 거절
- timeout·취소·오류 시 작업 cgroup 전체 정리
- 종료 원인, exit code/signal, 사용량, 제한된 stdout/stderr tail 반환
- `submitTask`, `getTask`, `cancelTask` 비동기 API
- 요청 ID 기반의 데몬 생존 기간 내 멱등 제출
- Ubuntu FFmpeg package를 사용하는 Local Raw Command 정상·timeout reference workflow

## 안전 보장

TaskCage는 다음 조건을 실행 계약으로 취급한다.

1. cgroup과 모든 제한을 적용하고 확인한 뒤에만 외부 프로그램을 시작한다.
2. 원자적인 cgroup 진입을 사용할 수 없으면 제한 없는 상태로 실행하지 않는다.
3. timeout·취소·오류 시 루트 PID가 아니라 작업 cgroup 전체를 종료한다.
4. 프로세스, cgroup, 출력 reader 정리를 확인한 뒤에만 `FINISHED` 결과를 공개한다.
5. 정리를 확인할 수 없으면 새 작업을 받지 않고 fail-stop 절차로 전환한다.

종료 원인은 단일 exit code로 추측하지 않는다. 데몬 제어 상태와 `memory.events.local`, `pids.events`, `cpu.stat` 같은 cgroup 통계를 함께 사용한다.

## 지원 환경

- Linux cgroup v2
- Rust 1.88 이상
- Java 17 이상
- PoC 검증 환경: Ubuntu 24.04

TaskCage는 신뢰할 수 없는 코드를 격리하는 보안 sandbox가 아니다. 파일시스템, 네트워크, syscall, 사용자 권한 격리는 컨테이너나 별도 보안 정책의 책임이다.

## 데몬 실행

Ubuntu 24.04 x86-64 host에서는 버전이 고정된 GitHub Release의 bootstrap installer로 daemon을 설치하고
바로 시작할 수 있다. 내려받은 스크립트의 내용을 확인한 뒤 root로 실행한다.

```bash
VERSION=0.1.0
RELEASE_URL="https://github.com/taskcage/taskcage/releases/download/taskcaged-v${VERSION}"

curl --fail --location --output install-taskcaged.sh \
  "${RELEASE_URL}/install-taskcaged.sh"
less install-taskcaged.sh
sudo bash install-taskcaged.sh --version "${VERSION}"
```

재현 가능한 설치는 버전을 명시하고, 설정을 검토하기 전 service를 시작하지 않으려면
`--no-autostart`를 추가한다.

```bash
sudo bash install-taskcaged.sh --version "${VERSION}" --no-autostart
sudoedit /etc/taskcage/taskcaged.env
sudo systemctl enable --now taskcaged.service
```

bootstrap installer는 선택한 GitHub Release의 archive와 SHA-256 checksum을 받아 검증한 뒤 packaged
installer를 실행한다. 상세 설치·재설치·제거 절차는 [Ubuntu daemon 설치](docs/install-ubuntu.md)를 따른다.

source checkout에서 실행할 때는 실제 cgroup v2 위임이 준비된 Linux 환경에서 먼저 사전 조건을 검사한다.

```bash
cargo build --workspace
cargo run -p taskcaged -- check-environment
```

서비스 모드는 소켓 경로와 내부 상한을 모두 명시한다.

```bash
target/debug/taskcaged serve \
  --socket /run/taskcage/taskcaged.sock \
  --max-concurrent-tasks 4 \
  --max-registry-tasks 1000 \
  --max-concurrent-connections 32 \
  --cleanup-timeout-ms 5000 \
  --fail-stop-timeout-ms 10000 \
  --max-task-cpu-quota-us 200000 \
  --max-task-cpu-period-us 100000 \
  --max-task-memory-bytes 2147483648 \
  --max-task-pids 128 \
  --max-task-timeout-ms 900000 \
  --max-task-stdout-tail-bytes 65536 \
  --max-task-stderr-tail-bytes 65536 \
  --profile-artifact-root /var/lib/taskcage/artifacts \
  --profile-artifact-max-bytes 104857600
```

상위 디렉터리와 서비스 계정은 배포 환경이 준비한다. 데몬은 소켓을 owner-only `0600`으로 생성한다. 위 값은 예시이며 프로토콜 기본값이 아니다.

### Opt-in Local Profile daemon 설정

v0.2의 Local Profile은 기본적으로 꺼져 있다. 위의 두 Profile 옵션을 **함께** 지정한 daemon만 정적
`file-copy@1.0.0` Profile과 Protocol v2 capability를 공개한다. 이 Profile은 Runtime Package, Bundle,
임의 executable 또는 caller-provided argv를 허용하지 않는다.

Artifact root는 daemon service UID 소유의 기존 absolute directory여야 하며, symlink가 아니고
group/other writable이면 안 된다. daemon은 시작 시 이 조건과 descriptor-relative staging/publish 권한을
검증한다. 두 옵션 중 하나가 빠지거나 검증에 실패하면 Profile capability를 광고하지 않고 daemon 시작을
거부한다. wire 계약은 [Local Profile Core API v2](docs/api-profile-v2.md), Artifact의 경계는
[Local Artifact 계약](docs/local-artifact-contract.md)을 따른다.

`check-environment`는 현재 process의 cgroup 실행 조건을 검사한다. 실행 중인 daemon 자체의 준비 상태는
socket owner와 같은 UID에서 live status로 확인한다. 기본 timeout은 2초다.

```bash
sudo -u taskcage target/debug/taskcaged status \
  --socket /run/taskcage/taskcaged.sock \
  --timeout-ms 2000
```

준비된 경우 `status=READY`와 Protocol v1 capabilities를 한 줄 JSON으로 반환한다. 연결 실패, timeout 또는
cgroup fail-stop으로 `UNREADY`인 경우 종료 코드는 `0`이 아니다. Ubuntu service는 구조화 JSON log를
사용하며, 기본 log에는 raw argv·환경 변수 값·작업 디렉터리·출력 tail을 남기지 않는다.

## Java SDK 사용

Java SDK `0.1.0`은 Maven Central에 공개됐다. Gradle 프로젝트에서는 다음 좌표를 추가한다.

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.1.0")
}
```

현재 공개 API는 동기 실행, 비동기 제출·조회, bounded 완료 대기와 취소를 제공한다. 실행 파일과 작업
디렉터리는 절대 경로여야 한다.
`TaskSpec(command)`는 유한한 SDK 안전 기본 자원 예산을 사용하며, 필요하면 기존 명시적 생성자로 override한다.

```java
TaskSpec spec = new TaskSpec(
    new ExternalCommand(
        Path.of("/usr/bin/pdftotext"),
        List.of("input.pdf", "output.txt"),
        Path.of("/srv/taskcage/jobs/42"),
        Map.of("LANG", "C.UTF-8")));

try (TaskCageClient client = TaskCageClient.connect(
        TaskCageClientConfig.builder()
            .socketPath(Path.of("/run/taskcage/taskcaged.sock"))
            .build())) {
    FinishedTaskSnapshot finished = client.run(spec, Duration.ofMinutes(3));
    ExecutionResult result = finished.result();
}
```

`run()`의 wait timeout은 제출 응답 뒤 완료 대기를 제한하며 Task의 cgroup wall-time 제한과 별개다. wait
timeout은 Task를 자동 취소하지 않는다. `TaskHandle.get()`은 현재 snapshot을 조회하고, bounded
`await()`는 비동기 완료 대기에 사용한다.
`TaskHandle.cancel()`은 daemon이 whole-task cleanup을 확인한 뒤 반환한다. 저수준 `submit()`,
`getTask()`, `cancelTask()`도 유지한다.

SDK 안전 기본값은 CPU 1개, memory 512 MiB, PID 32, 벽시계 2분, stdout/stderr tail 각각 65,536
bytes다. 이는 daemon capability 협상이 아니며 배포 최대값을 넘으면 `LIMIT_EXCEEDS_POLICY`로 거절된다.
더 큰 값이 필요하면 `new TaskSpec(command, new ResourceBudget(...))`로 명시한다.

SDK의 상세 사용법은 [Java SDK README](java-sdk/README.md)를 참고한다.

## 검증

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test

cd java-sdk
./gradlew test
```

실제 cgroup 동작은 전용 Linux VM에서 검증한다.

```bash
bash integration-tests/preflight-fail-closed.sh
bash integration-tests/cgroup-runner-smoke.sh
bash integration-tests/ffmpeg-reference-workflow.sh
bash integration-tests/release-artifact-smoke.sh \
  0.1.0 path/to/taskcage-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  path/to/taskcage-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256 \
  path/to/install-taskcaged.sh
```

환경 요구사항과 E2E 실행법은 [Linux 통합 시험](integration-tests/README.md)에 정리되어 있다.

## 문서

- [제품 철학과 용어](docs/product-philosophy.md)
- [Protocol v1 API 명세](docs/api-mvp.md)
- [Java SDK](java-sdk/README.md)
- [Ubuntu daemon 설치](docs/install-ubuntu.md)
- [릴리스 및 버전 정책](docs/release-policy.md)
- [릴리스 운영](docs/releasing.md)
- [FFmpeg Local Raw Command reference](docs/reference-ffmpeg.md)
- [Linux 통합 시험](integration-tests/README.md)
- [Protocol fixture](protocol-fixtures/v1/README.md)
- [기여 가이드](CONTRIBUTING.md)

## 단계별 제품 방향

`0.1.0`은 Local UDS와 Raw Command를 실제 Ubuntu 호스트에서 사용할 수 있는 Local Public Alpha 기준선이다.
다음 목표는 외부 사용자의 설치 시간, 실제 workload와 운영 피드백을 수집하고 호환되는 결함은 patch
버전으로 수정하는 것이다.

v0.2는 opt-in `file-copy@1.0.0` Local Profile과 Artifact 계약을 제한적으로 검증한다. Profile·Package·Artifact를
일반화하거나 Profile을 늘리기 전에는 이 경로를 Public Alpha의 외부 사용자와 반복 요구로 검증한다. 재현 가능한
실행은 버전 관리되는 Execution Profile과 digest로 고정한 Runtime Package를 사용한다.
Bundle은 Package binary가 아니라 Profile, Runtime Package ref + digest, 플랫폼·정책·무결성 정보를
담으며, 여러 Bundle이 같은 Package digest를 공유할 수 있다.

Remote는 Local Public Alpha나 Local Product Alpha의 선행 조건이 아니다. Local 계약과 실제 원격 수요가
검증된 뒤 topology·wire·인증·권한·Artifact 전달·backpressure·응답 유실 의미를 별도 설계와 API 계약으로
결정한다. 중앙 Hub server도 이 단계들의 구성요소가 아니며, 임의 URL에서 임의 binary를 받아 실행하는
기능을 제공하지 않는다.

## 기여

문제 사례와 기능 제안은 [GitHub Issues](https://github.com/taskcage/taskcage/issues)에서 공유해 주세요.
