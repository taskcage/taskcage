# TaskCage

TaskCage는 신뢰된 외부 프로세스를 작업 단위로 실행하고, Linux cgroup v2로 자원과 수명주기를 관리하는 경량 런타임이다.

> **상태:** `0.x` PoC. 다음 목표는 설치 가능한 Local Public Alpha이며, 아직 Maven Central 배포나 운영 호환성을 보장하지 않는다.

제품의 장기 방향과 표준 용어는 [제품 철학과 용어](docs/product-philosophy.md)에서 정의한다. 이 README는
현재 구현하고 검증한 PoC 범위를 설명한다.

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

## 현재 PoC

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
- Rust 1.85 이상
- Java 17 이상
- PoC 검증 환경: Ubuntu 24.04

TaskCage는 신뢰할 수 없는 코드를 격리하는 보안 sandbox가 아니다. 파일시스템, 네트워크, syscall, 사용자 권한 격리는 컨테이너나 별도 보안 정책의 책임이다.

## 데몬 실행

실제 cgroup v2 위임이 준비된 Linux 환경에서 먼저 사전 조건을 검사한다.

Ubuntu 24.04의 반복 가능한 service 설치는 [Ubuntu daemon 설치](docs/install-ubuntu.md)를 따른다.

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
  --fail-stop-timeout-ms 10000
```

상위 디렉터리와 서비스 계정은 배포 환경이 준비한다. 데몬은 소켓을 owner-only `0600`으로 생성한다. 위 값은 예시이며 프로토콜 기본값이 아니다.

## Java SDK 사용

SDK는 아직 Maven Central에 배포되지 않았다. 현재는 `java-sdk/`에서 직접 빌드해 사용한다.

```bash
cd java-sdk
./gradlew build
```

현재 공개 API는 제출, 조회, 취소를 제공한다. 모든 자원 예산과 실행 파일·작업 디렉터리의 절대 경로를 명시해야 한다.

```java
TaskSpec spec = new TaskSpec(
    new ExternalCommand(
        Path.of("/usr/bin/pdftotext"),
        List.of("input.pdf", "output.txt"),
        Path.of("/srv/taskcage/jobs/42"),
        Map.of("LANG", "C.UTF-8")),
    new ResourceBudget(
        new CpuQuota(100_000, 100_000),
        512L * 1024 * 1024,
        32,
        Duration.ofMinutes(2),
        65_536,
        65_536));

try (TaskCageClient client = TaskCageClient.connect(
        TaskCageClientConfig.builder()
            .socketPath(Path.of("/run/taskcage/taskcaged.sock"))
            .build())) {
    TaskSubmission submission = client.submit(spec);

    if (submission instanceof Task task) {
        TaskSnapshot snapshot = client.getTask(task.taskId());
        // RUNNING 또는 FINISHED 처리
    } else if (submission instanceof FinishedTaskSnapshot finished) {
        // exec 시작 실패처럼 즉시 정리가 끝난 결과
        TerminationReason reason = finished.result().terminationReason();
    }
}
```

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
```

환경 요구사항과 E2E 실행법은 [Linux 통합 시험](integration-tests/README.md)에 정리되어 있다.

## 문서

- [제품 철학과 용어](docs/product-philosophy.md)
- [Protocol v1 API 명세](docs/api-mvp.md)
- [Java SDK](java-sdk/README.md)
- [Ubuntu daemon 설치](docs/install-ubuntu.md)
- [Linux 통합 시험](integration-tests/README.md)
- [Protocol fixture](protocol-fixtures/v1/README.md)
- [기여 가이드](CONTRIBUTING.md)

## 단계별 제품 방향

다음 목표는 Local UDS와 Raw Command를 실제 Ubuntu 호스트에 설치해 반복 사용할 수 있는 Local Public
Alpha다. 서비스 계정과 systemd cgroup 위임, readiness와 구조화된 log, 배포 정책과 기본 자원 계약,
하나의 대표 workload, release artifact와 Java 배포 경로를 먼저 검증한다.

Execution Profile과 Artifact 계약은 Public Alpha를 최소 3명의 외부 사용자가 사용하고,
Profile·Package·Artifact로 일반화할 반복 요구가 2개 이상 확인되면 Local Product Alpha에서 도입한다.
재현 가능한 실행은 버전 관리되는 Execution Profile과 digest로 고정한 Runtime Package를 사용한다.
Bundle은 Package binary가 아니라 Profile, Runtime Package ref + digest, 플랫폼·정책·무결성 정보를
담으며, 여러 Bundle이 같은 Package digest를 공유할 수 있다.

Remote는 Local Public Alpha나 Local Product Alpha의 선행 조건이 아니다. Local 계약과 실제 원격 수요가
검증된 뒤 topology·wire·인증·권한·Artifact 전달·backpressure·응답 유실 의미를 ADR과 API 계약으로
결정한다. 중앙 Hub server도 이 단계들의 구성요소가 아니며, 임의 URL에서 임의 binary를 받아 실행하는
기능을 제공하지 않는다.

## 기여

문제 사례와 기능 제안은 [GitHub Issues](https://github.com/taskcage/taskcage/issues)에서 공유해 주세요.
