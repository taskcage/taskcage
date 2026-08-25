# TaskCage Benchmark Lab

TaskCage의 외부 CLI 작업 경계를 반복 검증하는 로컬 실험실이다. CI 성능 게이트나 공개 성능 수치의 근거가 아니다.

## 비교 대상

| 실행기 | 의미 | 주 비교 목적 |
| --- | --- | --- |
| `processbuilder` | 장기 실행 Java Worker가 직접 CLI를 시작 | 일반적인 기존 도입 경로 |
| `taskcage` | 같은 Java Worker가 UDS로 daemon에 Capsule Task를 제출 | 작업별 제한·정리와 추가 비용 |
| `docker-per-task` | 이후 추가할 보조 시나리오 | Job/컨테이너 per-task의 패키징·격리 비용 |

`docker-per-task`는 흔한 Job/격리 패턴이지만 TaskCage의 주 고객 경로는 아니다. 따라서 첫 두 실행기를 기본 비교로 유지하고, 세 번째는 별도 보조 실험으로 다룬다.

## 계층

```text
scenario definition
  -> Java worker execution
  -> collector (daemon Prometheus metrics + container samples + terminal results)
  -> live local dashboard
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

대표 미디어 파일을 사용해 정상 FFmpeg 변환만 반복 측정하려면 다음처럼 실행한다. 입력 파일의
복사와 digest 계산은 측정 시작 전에 끝나므로, 지연시간에는 포함되지 않는다.

```bash
python3 dev/benchmark/lab.py run \
  --input /absolute/path/to/input.mp4 \
  --scenarios normal \
  --concurrency 1 --warmup 1 --iterations 5
```

`audio_to_wav`가 너무 빨리 끝나 고정 관리 비용의 비중이 과도하게 보이면, 같은 입력을 H.264로
재인코딩하는 benchmark 전용 Capsule을 사용한다.

```bash
python3 dev/benchmark/lab.py run \
  --input /absolute/path/to/input.mp4 \
  --scenarios normal --normal-workload video_transcode \
  --concurrency 1 --warmup 1 --iterations 3
```

기본적으로 두 비교 Worker container에 `CPU 1.0`을 적용한다. 이는 Capsule의 기본 `CPU 1` 예산과
동일한 조건에서 비교하기 위한 설정이며, 필요하면 `--comparator-cpus`로 변경할 수 있다.

기본값은 `normal`, `timeout_child`, `memory_limit`을 각각 `processbuilder`, `taskcage`로 한 번씩 측정하며
warm-up은 수행하지 않는다(`warmup=0`, `iterations=1`). 실행 중 표시되는
`http://127.0.0.1:8765`에서 현재 단계, 종료 원인별 TaskCage metric과 컨테이너 자원 그래프를 확인할 수 있다.
실행이 끝나면 컨테이너는 정리하지만 대시보드는 유지하며, 확인을 마친 뒤 `Ctrl-C`로 닫는다.

```bash
python3 dev/benchmark/lab.py run --concurrency 8 --warmup 2 --iterations 30
python3 dev/benchmark/lab.py run --scenarios normal --concurrency 16
python3 dev/benchmark/lab.py run --no-keep-dashboard # 자동화에서 보고서 작성 후 바로 종료
```

Worker는 정상 output, 시나리오별 terminal reason, TaskCage 사용량과 cleanup evidence를 자동 검증한다.
하나라도 의도와 다르면 원시 결과와 HTML 보고서를 남긴 뒤 benchmark process가 0이 아닌 상태로 종료한다.

## 측정 경계

작업 지연시간은 Worker가 실행 요청을 제출하기 직전부터 정상 출력 검증 또는 terminal result 수신까지다. 따라서 TaskCage의 SDK/UDS 요청, daemon, 사전 설치된 Capsule과 cgroup 처리 비용은 포함한다. 이미지 build/pull, JVM·container·daemon 시작, warm-up은 제외한다. `normal`은 `ffmpeg-audio-to-wav@1.0.0` Capsule을, 실패 시나리오는 이미지에만 포함된 검증용 Capsule을 실행하므로 benchmark runner는 Raw Command API를 사용하지 않는다.

실시간 수집은 TaskCage daemon의 opt-in Prometheus `/metrics`와 Docker container 사용량을 결합한다.
종료 원인처럼 label이 있는 Prometheus metric도 label set을 포함한 이름으로 보존한다. ProcessBuilder 측에는
daemon metrics가 없으므로 Worker container 표본과 terminal result만 수집한다. 각 실행이 terminal result를
반환한 직후 `running_tasks=0`인 마지막 daemon 표본을 한 번 더 기록한다. 짧은 작업의 메모리 peak은 표본
주기에 따라 낮게 잡힐 수 있으므로, TaskCage Task가 반환하는 `memoryPeakBytes`를 우선 해석한다.

## 해석 원칙

- Docker Desktop/macOS 결과는 구조·정리 검증용이다. 공개 성능 주장은 native Linux VM/host 반복 결과로만 한다.
- `timeout_child`는 ProcessBuilder가 root만 종료했을 때의 잔여 descendant를 보이는 안정성 대비다. harness가 이후 정리한 사실도 결과에 명시한다.
- `memory_limit`은 ProcessBuilder에 동등한 task별 cgroup 경계가 없으므로, 동일 처리량 비교가 아니라 blast radius 대비다.
