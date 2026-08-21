#!/usr/bin/env python3
"""Render one local benchmark JSON result as a self-contained Korean HTML report."""

import html
import json
import sys
from pathlib import Path


def value(data, *path, default="-"):
    current = data
    for key in path:
        if not isinstance(current, dict) or key not in current:
            return default
        current = current[key]
    return current


def mib(number):
    if not isinstance(number, (int, float)) or number < 0:
        return "-"
    return f"{number / 1024 / 1024:.1f} MiB"


def ms(number):
    return f"{number} ms" if isinstance(number, (int, float)) else "-"


def cpu_time(micros):
    if not isinstance(micros, (int, float)) or micros < 0:
        return "-"
    return f"{micros / 1_000_000:.3f} s"


def cleanup(result):
    details = value(result, "cleanup", default={})
    return "confirmed" if details.get("cleanupConfirmed") else "not confirmed"


def row(name, result):
    if "workerResult" in result:
        result = result["workerResult"]
    tasks = value(result, "tasks", default={})
    normal_tasks = value(tasks, "normalTasks", default={})
    latency = value(normal_tasks, "latencyMs", default={})
    resources = value(result, "taskResources", default={})
    executor = value(result, "executorContainer", default={})
    if name == "TaskCage daemon":
        cleanup_text = "cgroup cleanup confirmed" if cleanup(result) == "confirmed" else "not confirmed"
    else:
        residual = value(result, "cleanup", "residualProcesses", default=0)
        cleanup_text = "residual observed: " + str(residual) if residual else "no descendant observed"
    return "".join((
        "<tr>",
        f"<td>{html.escape(name)}</td>",
        f"<td>{ms(latency.get('p50'))} / {ms(latency.get('p95'))} ({normal_tasks.get('submitted', 0)} normal)</td>",
        f"<td>{html.escape(str(tasks.get('terminationReasons', {})))}</td>",
        f"<td>{mib(resources.get('memoryPeakBytes'))}</td>",
        f"<td>{cpu_time(resources.get('cpuTimeMicros'))}</td>",
        f"<td>{mib(executor.get('memoryPeakBytes'))}</td>",
        f"<td>{cpu_time(executor.get('cpuUsageMicros'))}</td>",
        f"<td>{html.escape(cleanup_text)}</td>",
        "</tr>",
    ))


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: render-report.py RESULT.json REPORT.html")
    result = json.loads(Path(sys.argv[1]).read_text())
    environment = result["environment"]
    scenario_sections = []
    for scenario in result["scenarios"]:
        rows = "".join(row(label, scenario[key]) for key, label in (
            ("processBuilder", "Java ProcessBuilder"),
            ("taskCage", "TaskCage daemon"),
        ) if key in scenario)
        scenario_sections.append(f"""
          <h2>{html.escape(scenario['name'])}</h2>
          <table><thead><tr><th>경로</th><th>정상 Task p50 / p95</th><th>종료 이유</th>
          <th>Task peak memory</th><th>Task CPU time</th><th>실행 주체 peak memory</th><th>실행 주체 CPU time</th>
          <th>정리</th>
          </tr></thead><tbody>{rows}</tbody></table>""")
    body = "\n".join(scenario_sections)
    output = f"""<!doctype html>
<html lang=\"ko\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>TaskCage worker execution benchmark</title>
<style>
:root {{ font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif; color:#172033; background:#f6f8fc; }}
body {{ margin:0; padding:40px 20px; }} main {{ max-width:1120px; margin:auto; }} h1 {{ margin:0 0 8px; }} h2 {{ margin:34px 0 14px; }}
.muted {{ color:#667085; }} .note {{ background:#fff; border-left:4px solid #4c78ff; border-radius:10px; padding:18px; line-height:1.55; margin:24px 0; }}
table {{ width:100%; border-collapse:collapse; background:#fff; border:1px solid #e3e8f2; border-radius:12px; overflow:hidden; }}
th,td {{ padding:13px 15px; border-bottom:1px solid #e9edf5; text-align:left; vertical-align:top; }} th {{ background:#f8faff; color:#526078; font-size:13px; }} tr:last-child td {{ border-bottom:0; }} code {{ background:#f1f4f9; padding:2px 5px; border-radius:4px; }}
</style></head><body><main>
<h1>TaskCage Worker 실행 비교</h1>
<p class=\"muted\">{html.escape(environment['kind'])} · 동시성 {environment['concurrency']} · warm-up {environment['warmupBatches']} batches · 측정 {environment['measuredBatches']} batches</p>
<section class=\"note\"><strong>측정 범위:</strong> 작업 제출 직전부터 결과 파일 검증 또는 최종 실패까지입니다. TaskCage에는 SDK·UDS 요청이 포함됩니다. 이미지 빌드/pull, JVM·daemon·container 최초 기동과 warm-up은 제외됩니다.<br><br><strong>정리 해석:</strong> TaskCage는 cgroup cleanup 확인, ProcessBuilder는 root-only 종료 뒤 관찰된 descendant 수를 의미합니다. ProcessBuilder의 잔여 자식은 측정 후 harness가 개발 환경 보호를 위해 정리합니다.<br><br><strong>해석:</strong> Docker Desktop 기반 결과는 로컬 구조 검증입니다. 정상 지연시간만으로 일반적 성능 우위를 주장하지 않으며, timeout·limit 뒤 정리와 정상 작업의 영향 범위를 핵심 지표로 봅니다.</section>
{body}
</main></body></html>"""
    Path(sys.argv[2]).write_text(output)


if __name__ == "__main__":
    main()
