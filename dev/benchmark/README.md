# TaskCage Benchmark Lab

TaskCage의 외부 CLI 작업 경계를 반복 검증하는 로컬 실험실이다. CI 성능 게이트나 공개 성능 수치의 근거가 아니다.

## 비교 대상

| 실행기 | 의미 | 주 비교 목적 |
| --- | --- | --- |
| `processbuilder` | 장기 실행 Java Worker가 직접 CLI를 시작 | 일반적인 기존 도입 경로 |
| `taskcage` | 같은 Java Worker가 UDS로 daemon에 Capsule Task를 제출 | 작업별 제한·정리와 추가 비용 |

이 실험은 동일한 Java 프로그램에서 `ProcessBuilder` 호출을 TaskCage Capsule 호출로 바꿨을 때의 사용자 경험을 비교한다. 두 실행기는 동일한 CPU 1.0 예산에서 실행한다. 작업별 Docker 컨테이너 실행은 별도 lifecycle 실험으로 다루며, 이 성능 비교의 대조군에는 넣지 않는다.

## 계층

```text
scenario definition
  -> Java worker execution
  -> collector (daemon Prometheus metrics + container samples + terminal results)
  -> raw JSON + self-contained HTML report
```

각 실행은 `results/runs/<timestamp>/`에 독립적으로 저장된다.

- `manifest.json`: 실행 환경과 측정 경계
- `events.ndjson`: 실행 단계 및 최종 worker 결과
- `samples.ndjson`: daemon Prometheus/cgroup 샘플
- `result.json`: 비교 가능한 최종 수치
- `report.html`: 공유 가능한 정적 한국어 보고서

## 실행

Linux cgroup v2를 제공하는 신뢰된 Docker 환경에서 저장소 루트 기준으로 실행한다.

```bash
python3 dev/benchmark/lab.py run
```

대표 미디어 파일을 사용해 정상 FFmpeg 변환을 반복 측정하려면 다음처럼 실행한다. 입력 파일의 복사와 digest 계산은 측정 시작 전에 끝나므로, 지연시간에는 포함되지 않는다.

```bash
python3 dev/benchmark/lab.py run \
  --input /absolute/path/to/input.mp4 \
  --concurrency 1 --warmup 2 --iterations 100 \
  --control-iterations 1
```

`audio_to_wav`가 너무 빨리 끝나 고정 관리 비용의 비중이 과도하게 보이면, 같은 입력을 H.264로 재인코딩하는 benchmark 전용 Capsule을 사용한다.

```bash
python3 dev/benchmark/lab.py run \
  --input /absolute/path/to/input.mp4 \
  --scenarios normal --normal-workload video_transcode \
  --concurrency 1 --warmup 2 --iterations 30
```

기본적으로 두 비교 Worker container에 `CPU 1.0`을 적용한다. 이는 Capsule의 기본 `CPU 1` 예산과 동일한 조건에서 비교하기 위한 설정이며, 필요하면 `--comparator-cpus`로 변경할 수 있다.

기본값은 `normal`, `timeout_child`, `memory_limit`을 각각 `processbuilder`, `taskcage`로 한 번씩 측정하며 warm-up은 수행하지 않는다(`warmup=0`, `iterations=1`). `--iterations`와 `--warmup`은 정상 변환에만 적용되며, 제어 시나리오는 기본적으로 `--control-iterations 1`로 실행된다. 실행이 끝나면 컨테이너를 정리하고, 터미널에 원시 JSON과 정적 HTML 보고서의 절대 경로를 출력한다. HTML 보고서는 정상 변환에서만 지연시간과 메모리 peak를 비교하고, timeout·memory 제한은 각각 전체 정리와 작업별 제한을 O/X로 표시한다.

정상 변환의 메모리 그래프는 전체 실행 footprint를 보수적으로 나타낸다. `processbuilder`는 Java Worker와 그 자식 프로세스 트리를, `taskcage`는 Java Worker·`taskcaged` 프로세스·Task cgroup을 포함한다. privileged daemon container의 root cgroup은 호스트 범위까지 보일 수 있어 사용하지 않는다. 서로 다른 구성 요소의 peak 시점은 일치하지 않을 수 있으므로, 값은 같은 시점의 시스템 peak가 아닌 보수적 상한이다.

```bash
python3 dev/benchmark/lab.py run --concurrency 8 --warmup 2 --iterations 30
python3 dev/benchmark/lab.py run --scenarios normal --concurrency 16
```

Worker는 정상 output, 시나리오별 terminal reason, TaskCage 사용량과 cleanup evidence를 자동 검증한다. 하나라도 의도와 다르면 원시 결과와 HTML 보고서를 남긴 뒤 benchmark process가 0이 아닌 상태로 종료한다.

## 측정 경계

작업 지연시간은 Java Worker가 실행 요청을 시작하기 직전부터 정상 출력 검증 또는 terminal result 수신까지다. 따라서 TaskCage의 SDK/UDS 요청, daemon, 사전 설치된 Capsule과 cgroup 처리 비용은 포함한다. 이미지 build/pull, JVM·container·daemon 시작, warm-up은 제외한다. `normal`은 `ffmpeg-audio-to-wav@1.0.0` Capsule을, 실패 시나리오는 이미지에만 포함된 검증용 Capsule을 실행하므로 benchmark runner는 Raw Command API를 사용하지 않는다.

실시간 수집은 TaskCage daemon의 opt-in Prometheus `/metrics`와 Docker container 사용량을 결합한다. 종료 원인처럼 label이 있는 Prometheus metric도 label set을 포함한 이름으로 보존한다. ProcessBuilder 측에는 daemon metrics가 없으므로 Worker container 표본과 terminal result를 수집한다. 정상 FFmpeg 비교에서는 Worker와 외부 작업을 함께 해석하기 위해 Worker cgroup 사용량을 기록한다. TaskCage 실행 후에는 daemon 프로세스의 `VmHWM`과 Task cgroup 결과를 각각 기록한다. 각 실행이 terminal result를 반환한 직후 `running_tasks=0`인 마지막 daemon 표본을 한 번 더 기록한다. 짧은 작업의 메모리 peak은 표본 주기에 따라 낮게 잡힐 수 있으므로, TaskCage Task가 반환하는 `memoryPeakBytes`를 우선 해석한다.

## 해석 원칙

- Docker Desktop/macOS 결과는 구조·정리 검증용이다. 공개 성능 주장은 native Linux VM/host 반복 결과로만 한다.
- `timeout_child`는 ProcessBuilder가 root만 종료했을 때의 잔여 descendant를 보이는 안정성 대비다. harness가 이후 정리한 사실도 결과에 명시한다. TaskCage는 작업 cgroup을 제거한 뒤 잔여 프로세스가 없는지 검증한다.
- `memory_limit`은 ProcessBuilder에 동등한 task별 cgroup 경계가 없으므로, 동일 처리량 비교가 아니라 작업별 memory limit 적용 여부를 검증한다.
