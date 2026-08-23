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

기본값은 `normal`, `timeout_child`, `memory_limit`을 각각 `processbuilder`, `taskcage`로 실행한다. 실행 중 표시되는 `http://127.0.0.1:8765`에서 현재 단계와 TaskCage metrics를 확인할 수 있다.

```bash
python3 dev/benchmark/lab.py run --concurrency 8 --warmup 2 --iterations 30
python3 dev/benchmark/lab.py run --scenarios normal --concurrency 16
```

## 측정 경계

작업 지연시간은 Worker가 실행 요청을 제출하기 직전부터 정상 출력 검증 또는 terminal result 수신까지다. 따라서 TaskCage의 SDK/UDS 요청과 daemon 처리 비용은 포함한다. 이미지 build/pull, JVM·container·daemon 시작, warm-up은 제외한다.

실시간 수집은 TaskCage daemon의 opt-in Prometheus `/metrics`와 Docker container 사용량을 결합한다. ProcessBuilder 측에는 daemon metrics가 없으므로 Worker container 표본과 terminal result만 수집한다. 짧은 작업의 메모리 peak은 표본 주기에 따라 낮게 잡힐 수 있으므로, TaskCage Task가 반환하는 `memoryPeakBytes`를 우선 해석한다.

## 해석 원칙

- Docker Desktop/macOS 결과는 구조·정리 검증용이다. 공개 성능 주장은 native Linux VM/host 반복 결과로만 한다.
- `timeout_child`는 ProcessBuilder가 root만 종료했을 때의 잔여 descendant를 보이는 안정성 대비다. harness가 이후 정리한 사실도 결과에 명시한다.
- `memory_limit`은 ProcessBuilder에 동등한 task별 cgroup 경계가 없으므로, 동일 처리량 비교가 아니라 blast radius 대비다.
