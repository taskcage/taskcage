#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Manual local benchmark lab; intentionally dependency-free."""
from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import os
import pathlib
import shutil
import subprocess
import threading
import urllib.request
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parent
REPO = ROOT.parent.parent
COMPOSE = ["docker", "compose", "--file", str(ROOT / "compose.yml"), "--profile", "benchmark"]


def write_json(path: pathlib.Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def append(path: pathlib.Path, value: dict[str, Any]) -> None:
    value["at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(value, ensure_ascii=False) + "\n")


def command(
    args: list[str],
    *,
    capture: bool = False,
    check: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=REPO, check=check, text=True, capture_output=capture, env=env)


def parse_prometheus(text: str) -> dict[str, float]:
    values = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        try:
            key, value = line.rsplit(None, 1)
            values[key] = float(value)
        except (ValueError, TypeError):
            continue
    return values


def daemon_metrics() -> dict[str, float]:
    try:
        with urllib.request.urlopen("http://127.0.0.1:19098/metrics", timeout=.4) as response:
            text = response.read().decode()
    except OSError:
        return {}
    return parse_prometheus(text)


def container_stats(service: str) -> dict[str, Any]:
    """Best-effort Docker sample; task-level usage remains the authoritative peak."""
    try:
        container = command(["docker", "ps", "--filter", f"label=com.docker.compose.service={service}", "--format", "{{.ID}}"], capture=True).stdout.strip().splitlines()
        if not container:
            return {}
        output = command(["docker", "stats", "--no-stream", "--format", "{{json .}}", container[0]], capture=True).stdout
        return json.loads(output)
    except (subprocess.CalledProcessError, json.JSONDecodeError):
        return {}


def collect_sample(output: pathlib.Path) -> dict[str, Any]:
    sample = {
        "at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "daemonMetrics": daemon_metrics(),
        "daemonContainer": container_stats("taskcaged"),
        "workerContainer": container_stats("benchmark-worker"),
    }
    with output.open("a", encoding="utf-8") as file:
        file.write(json.dumps(sample, ensure_ascii=False) + "\n")
    return sample


def collect(stop: threading.Event, output: pathlib.Path) -> None:
    while not stop.wait(.5):
        collect_sample(output)


def display_metric(value: Any, divisor: float, suffix: str) -> str:
    if not isinstance(value, (int, float)) or value < 0:
        return "측정 안 됨"
    return f"{value / divisor:,.1f} {suffix}"


def result_validation(executions: list[dict[str, Any]]) -> dict[str, Any]:
    errors = []
    for execution in executions:
        label = f"{execution['scenario']}/{execution['mode']}"
        worker_validation = execution["workerResult"].get("validation", {})
        worker_errors = worker_validation.get("errors", [])
        if execution["workerExitCode"] != 0 or not worker_validation.get("passed", False):
            if worker_errors:
                errors.extend(f"{label}: {error}" for error in worker_errors)
            else:
                errors.append(f"{label}: worker exited {execution['workerExitCode']} without passing validation")
    return {"passed": not errors, "errors": errors}


SCENARIO_LABELS = {
    "normal": "정상 변환",
    "timeout_child": "Timeout + 자식 프로세스",
    "memory_limit": "메모리 제한",
}
MODE_LABELS = {
    "processbuilder": "ProcessBuilder",
    "docker_per_task": "Docker per task",
    "taskcage": "TaskCage",
}
MODE_COLORS = {"processbuilder": "#64748b", "docker_per_task": "#d97706", "taskcage": "#2563eb"}


def metric(execution: dict[str, Any], name: str) -> tuple[float, str]:
    worker = execution["workerResult"]
    if name == "latency":
        return float(worker["tasks"]["latencyMs"]["p50"]), "ms"
    resources = worker["taskResources"]
    container = worker["executorContainer"]
    if name == "memory":
        value = resources["memoryPeakBytes"]
        return float(value if value >= 0 else container["memoryPeakBytes"]) / 1024 / 1024, "MiB"
    value = resources["cpuTimeMicros"]
    return float(value if value >= 0 else container["cpuUsageMicros"]) / 1000, "ms"


def chart(title: str, description: str, executions: list[dict[str, Any]], name: str) -> str:
    maximum = max((metric(execution, name)[0] for execution in executions), default=1) or 1
    scenarios = []
    for scenario in SCENARIO_LABELS:
        rows = []
        for execution in (item for item in executions if item["scenario"] == scenario):
            value, unit = metric(execution, name)
            tasks = execution["workerResult"]["tasks"]
            p95 = tasks["latencyMs"]["p95"] if name == "latency" else None
            detail = f"p50 {value:,.0f} {unit} · p95 {p95:,.0f} ms" if p95 is not None else f"{value:,.1f} {unit}"
            rows.append(
                f'<div class="bar-row"><span>{MODE_LABELS[execution["mode"]]}</span>'
                f'<div class="bar-track"><i style="width:{value / maximum * 100:.1f}%;background:{MODE_COLORS[execution["mode"]]}"></i></div>'
                f'<strong>{detail}</strong></div>')
        if rows:
            scenarios.append(f'<section class="chart-group"><h3>{SCENARIO_LABELS.get(scenario, scenario)}</h3>{"".join(rows)}</section>')
    return f'<section class="card"><h2>{title}</h2><p class="muted">{description}</p>{"".join(scenarios)}</section>'


def cleanup_table(executions: list[dict[str, Any]]) -> str:
    rows = []
    for execution in executions:
        worker = execution["workerResult"]
        cleanup = worker["cleanup"]
        reasons = ", ".join(worker["tasks"]["terminationReasons"])
        confirmed = "O" if cleanup["cleanupConfirmed"] else "X"
        rows.append(
            f'<tr><td>{SCENARIO_LABELS.get(execution["scenario"], execution["scenario"])}</td>'
            f'<td>{MODE_LABELS[execution["mode"]]}</td><td>{html.escape(reasons)}</td>'
            f'<td>{cleanup["residualProcesses"]}</td><td class="cleanup-{confirmed.lower()}">{confirmed}</td></tr>')
    return '<section class="card"><h2>프로세스 정리 결과</h2><table><thead><tr><th>시나리오</th><th>실행기</th><th>종료 원인</th><th>잔여 프로세스</th><th>전체 정리</th></tr></thead><tbody>' + "".join(rows) + "</tbody></table></section>"


def analysis(executions: list[dict[str, Any]]) -> str:
    normal = {item["mode"]: item for item in executions if item["scenario"] == "normal"}
    notes = []
    if {"processbuilder", "taskcage"} <= normal.keys():
        direct, managed = metric(normal["processbuilder"], "latency")[0], metric(normal["taskcage"], "latency")[0]
        delta = managed - direct
        notes.append(f"정상 변환에서 TaskCage의 p50은 ProcessBuilder 대비 {delta:,.0f} ms ({delta / direct * 100:.1f}%) 차이입니다.")
    timeout = {item["mode"]: item for item in executions if item["scenario"] == "timeout_child"}
    if {"processbuilder", "taskcage"} <= timeout.keys():
        residual = timeout["processbuilder"]["workerResult"]["cleanup"]["residualProcesses"]
        notes.append(f"Timeout 시나리오에서 ProcessBuilder는 {residual}개의 descendant가 남았고, TaskCage는 cgroup 정리를 확인했습니다.")
    memory = {item["mode"]: item for item in executions if item["scenario"] == "memory_limit"}
    if "taskcage" in memory:
        notes.append("메모리 제한 시나리오에서 TaskCage는 작업별 memory limit 종료 원인과 peak 사용량을 함께 반환했습니다.")
    return '<section class="card"><h2>자동 해석</h2><ul>' + "".join(f"<li>{html.escape(note)}</li>" for note in notes) + "</ul></section>"


def render(result: dict[str, Any], destination: pathlib.Path) -> None:
    executions = result["executions"]
    validation = result["validation"]
    status = "검증 통과" if validation["passed"] else "검증 실패"
    status_class = "passed" if validation["passed"] else "failed"
    destination.write_text(f'''<!doctype html><html lang="ko"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>TaskCage Benchmark Report</title><style>
body{{font-family:system-ui,sans-serif;max-width:1100px;margin:40px auto;padding:0 20px;color:#17212b;background:#f8fafc}}.card{{background:#fff;border:1px solid #d8dee4;border-radius:12px;padding:20px;margin:16px 0}}h1{{margin-bottom:4px}}h2{{margin:0 0 6px}}h3{{font-size:14px;margin:20px 0 8px}}.muted{{color:#64748b;margin:0}}.status{{display:inline-block;border-radius:999px;padding:6px 10px;font-weight:700}}.passed{{background:#dcfce7;color:#166534}}.failed{{background:#fee2e2;color:#991b1b}}.bar-row{{display:grid;grid-template-columns:120px 1fr 170px;gap:10px;align-items:center;margin:7px 0;font-size:13px}}.bar-track{{height:16px;background:#e2e8f0;border-radius:999px;overflow:hidden}}.bar-track i{{display:block;height:100%;border-radius:999px}}table{{border-collapse:collapse;width:100%;font-size:14px}}td,th{{border:1px solid #d8dee4;padding:8px;text-align:left}}th{{background:#f4f7fa}}.cleanup-o{{color:#166534;font-weight:800}}.cleanup-x{{color:#b91c1c;font-weight:800}}li{{margin:7px 0}}code{{background:#e2e8f0;padding:2px 4px;border-radius:4px}}@media(max-width:650px){{.bar-row{{grid-template-columns:1fr;gap:4px}}}}
</style><h1>TaskCage Benchmark Report</h1><p class="muted">실행 ID: {html.escape(result["runId"])} · {html.escape(result["environment"]["kind"])}</p><p><span class="status {status_class}">{status}</span></p>{analysis(executions)}{chart("작업 지연시간", "막대는 p50이며 p95를 함께 표기합니다. timeout·memory는 terminal result까지의 시간입니다.", executions, "latency")}{chart("메모리 peak", "TaskCage는 task cgroup peak, ProcessBuilder는 Worker container 관측 peak입니다.", executions, "memory")}{chart("CPU 사용 시간", "TaskCage는 task CPU time, ProcessBuilder는 Worker container CPU time입니다.", executions, "cpu")}{cleanup_table(executions)}<section class="card"><h2>측정 경계</h2><p>지연시간에는 요청 제출부터 terminal result와 정상 출력 검증까지 포함하며, 이미지 build/pull·입력 준비·daemon 시작·warm-up은 제외합니다. 이 보고서는 로컬 Docker 실험 결과이므로 공개 성능 주장은 native Linux 반복 측정으로 재검증해야 합니다.</p></section></html>''', encoding="utf-8")


def execute(args: argparse.Namespace) -> None:
    if not shutil.which("docker"): raise SystemExit("docker is required")
    if args.comparator_cpus <= 0: raise SystemExit("--comparator-cpus must be positive")
    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    root = ROOT / "results" / "runs" / run_id; root.mkdir(parents=True)
    events, samples = root / "events.ndjson", root / "samples.ndjson"
    input_file = args.input.resolve() if args.input else None
    if input_file and not input_file.is_file():
        raise SystemExit(f"--input must point to a regular file: {input_file}")
    input_manifest = None if input_file is None else {"name": input_file.name, "bytes": input_file.stat().st_size}
    write_json(root / "manifest.json", {"runId": run_id, "environment": {"kind": "local-docker", "hostArchitecture": os.uname().machine}, "comparators": ["processbuilder", "taskcage"], "scenarios": args.scenarios, "normalWorkload": args.normal_workload, "comparatorCpuLimit": args.comparator_cpus, "concurrency": args.concurrency, "warmup": args.warmup, "iterations": args.iterations, "input": input_manifest, "measurementBoundary": "worker submission through terminal result and normal output validation; excludes input preparation, build, service start and warm-up"})
    env = os.environ | {"BENCHMARK_MAX_CONCURRENT_TASKS": str(args.concurrency), "BENCHMARK_WORKER_CPUS": str(args.comparator_cpus)}
    if input_file:
        env["BENCHMARK_INPUT_HOST_PATH"] = str(input_file)
    executions: list[dict[str, Any]] = []
    validation = {"passed": False, "errors": ["run did not complete"]}
    try:
        append(events, {"type": "build_started"})
        command(COMPOSE + ["build", "--quiet", "taskcaged", "benchmark-worker"], env=env)
        append(events, {"type": "build_finished"})
        for scenario in args.scenarios:
            for mode in ("processbuilder", "taskcage"):
                append(events, {"type": "execution_started", "scenario": scenario, "mode": mode})
                if mode == "taskcage": command(COMPOSE + ["up", "--detach", "--wait", "taskcaged"], env=env)
                worker_command = COMPOSE + ["run", "--rm", "--no-deps", "-e", f"BENCHMARK_MODE={mode}", "-e", f"BENCHMARK_SCENARIO={scenario}", "-e", f"BENCHMARK_NORMAL_WORKLOAD={args.normal_workload}", "-e", f"BENCHMARK_CONCURRENCY={args.concurrency}", "-e", f"BENCHMARK_WARMUP={args.warmup}", "-e", f"BENCHMARK_ITERATIONS={args.iterations}"]
                if input_file:
                    worker_command += ["-e", "BENCHMARK_INPUT_FILE=/benchmark-input/input"]
                worker_command += ["benchmark-worker"]
                stop = threading.Event(); collector = threading.Thread(target=collect, args=(stop, samples), daemon=True); collector.start()
                try:
                    worker_process = command(worker_command, capture=True, check=False, env=env)
                    worker = json.loads(worker_process.stdout)
                finally:
                    stop.set(); collector.join(timeout=2)
                    # worker가 terminal result를 반환한 직후 daemon의 running_tasks=0을 보존한다.
                    collect_sample(samples)
                item = {"scenario": scenario, "mode": mode, "workerExitCode": worker_process.returncode,
                        "workerResult": worker}
                if mode == "taskcage":
                    command(COMPOSE + ["exec", "--no-TTY", "taskcaged", "taskcage-container-verify-cleanup"], env=env)
                    item["daemonCleanupVerified"] = True
                executions.append(item); append(events, {"type": "execution_finished", **item})
                command(COMPOSE + ["down", "--volumes", "--remove-orphans"], env=env)
        validation = result_validation(executions)
        result = {"runId": run_id, "environment": {"kind": "local-docker"},
                  "validation": validation, "executions": executions}
        write_json(root / "result.json", result); render(result, root / "report.html")
        print(f"Result JSON: {(root / 'result.json').resolve()}\nHTML report: {(root / 'report.html').resolve()}")
    finally:
        command(COMPOSE + ["down", "--volumes", "--remove-orphans"], env=env)
    if not validation["passed"]:
        raise SystemExit("benchmark intent validation failed")


def main() -> None:
    parser = argparse.ArgumentParser(description="TaskCage benchmark lab")
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run")
    run_parser.add_argument("--concurrency", type=int, default=2); run_parser.add_argument("--warmup", type=int, default=0); run_parser.add_argument("--iterations", type=int, default=1)
    run_parser.add_argument("--scenarios", nargs="+", default=["normal", "timeout_child", "memory_limit"]); run_parser.add_argument("--input", type=pathlib.Path, help="optional host media file for normal Capsule and ProcessBuilder runs"); run_parser.add_argument("--normal-workload", choices=["audio_to_wav", "video_transcode"], default="audio_to_wav"); run_parser.add_argument("--comparator-cpus", type=float, default=1.0, help="CPU limit applied to both benchmark worker containers; default matches the Capsule CPU budget")
    args = parser.parse_args()
    if args.concurrency < 1 or args.warmup < 0 or args.iterations < 1: parser.error("invalid run sizes")
    execute(args)


if __name__ == "__main__": main()
