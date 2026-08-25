#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Manual local benchmark lab; intentionally dependency-free."""
from __future__ import annotations

import argparse
import datetime as dt
import html
import http.server
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


class State:
    def __init__(self, root: pathlib.Path):
        self.path, self.value = root / "live.json", {"phase": "preparing", "samples": []}
        self.lock = threading.Lock(); self.flush()

    def flush(self) -> None: write_json(self.path, self.value)
    def update(self, **changes: Any) -> None:
        with self.lock: self.value.update(changes); self.flush()
    def sample(self, sample: dict[str, Any]) -> None:
        with self.lock: self.value["samples"] = (self.value["samples"] + [sample])[-120:]; self.flush()


class SilentHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_: Any) -> None: pass


def start_dashboard(root: pathlib.Path, port: int) -> http.server.ThreadingHTTPServer:
    handler = lambda *args, **kwargs: SilentHandler(*args, directory=str(root), **kwargs)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


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


def collect_sample(state: State, output: pathlib.Path) -> dict[str, Any]:
    sample = {
        "at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "daemonMetrics": daemon_metrics(),
        "daemonContainer": container_stats("taskcaged"),
        "workerContainer": container_stats("benchmark-worker"),
    }
    with output.open("a", encoding="utf-8") as file:
        file.write(json.dumps(sample, ensure_ascii=False) + "\n")
    state.sample(sample)
    return sample


def collect(stop: threading.Event, state: State, output: pathlib.Path) -> None:
    while not stop.wait(.5):
        collect_sample(state, output)


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


def render(result: dict[str, Any], destination: pathlib.Path) -> None:
    rows = []
    for execution in result["executions"]:
        tasks = execution["workerResult"]["tasks"]
        resources = execution["workerResult"]["taskResources"]
        container = execution["workerResult"]["executorContainer"]
        cleanup = execution["workerResult"]["cleanup"]
        reasons = ", ".join(f"{name}: {count}" for name, count in tasks["terminationReasons"].items())
        rows.append(
            "<tr>"
            f"<td>{html.escape(execution['scenario'])}</td>"
            f"<td>{html.escape(execution['mode'])}</td>"
            f"<td>{tasks['submitted']}</td>"
            f"<td>{tasks['normalTasks']['latencyMs']['p50']} ms</td>"
            f"<td>{tasks['normalTasks']['latencyMs']['p95']} ms</td>"
            f"<td>{html.escape(reasons)}</td>"
            f"<td>{display_metric(resources['memoryPeakBytes'], 1024 * 1024, 'MiB')}</td>"
            f"<td>{display_metric(resources['cpuTimeMicros'], 1000, 'ms')}</td>"
            f"<td>{display_metric(container['memoryPeakBytes'], 1024 * 1024, 'MiB')}</td>"
            f"<td>{display_metric(container['cpuUsageMicros'], 1000, 'ms')}</td>"
            f"<td>{cleanup['residualProcesses']}</td>"
            f"<td>{str(cleanup['cleanupConfirmed']).lower()}</td>"
            "</tr>"
        )
    validation = result["validation"]
    validation_class = "passed" if validation["passed"] else "failed"
    validation_text = "검증 통과" if validation["passed"] else "검증 실패"
    error_list = "" if validation["passed"] else "<ul>" + "".join(
        f"<li>{html.escape(error)}</li>" for error in validation["errors"]) + "</ul>"
    destination.write_text(f'''<!doctype html><html lang="ko"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>TaskCage Benchmark Lab</title><style>body{{font-family:system-ui,sans-serif;max-width:1440px;margin:40px auto;padding:0 20px;color:#17212b}}table{{border-collapse:collapse;width:100%;font-size:14px}}td,th{{border:1px solid #d8dee4;padding:8px;text-align:left;vertical-align:top}}th{{background:#f4f7fa}}.status{{display:inline-block;padding:6px 10px;border-radius:999px;font-weight:700}}.passed{{background:#dcfce7;color:#166534}}.failed{{background:#fee2e2;color:#991b1b}}.table-wrap{{overflow:auto}}</style><h1>TaskCage Benchmark Lab 결과</h1><p>실행 ID: {html.escape(result['runId'])} · 환경: {html.escape(result['environment']['kind'])}</p><p><span class="status {validation_class}">{validation_text}</span></p>{error_list}<div class="table-wrap"><table><thead><tr><th>시나리오</th><th>실행기</th><th>Task</th><th>정상 p50</th><th>정상 p95</th><th>종료 원인</th><th>Task 메모리 peak</th><th>Task CPU</th><th>Worker 메모리 peak</th><th>Worker CPU</th><th>잔여 프로세스</th><th>정리 확인</th></tr></thead><tbody>{''.join(rows)}</tbody></table></div><h2>해석</h2><p>지연시간은 Worker 요청 제출부터 결과·정상 출력 검증까지이며 TaskCage SDK/UDS/daemon 비용을 포함합니다. 시작·build·warm-up은 제외합니다. ProcessBuilder에는 task 단위 자원 계측이 없어 해당 칸을 측정 안 됨으로 표시합니다.</p><p>이 보고서는 로컬 실험 증거입니다. Docker Desktop 결과만으로 공개 성능을 주장하지 않으며, native Linux 반복 실행으로 재검증해야 합니다.</p></html>''', encoding="utf-8")


def execute(args: argparse.Namespace) -> None:
    if not shutil.which("docker"): raise SystemExit("docker is required")
    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    root = ROOT / "results" / "runs" / run_id; root.mkdir(parents=True)
    shutil.copy2(ROOT / "dashboard.html", root / "dashboard.html")
    state, events, samples = State(root), root / "events.ndjson", root / "samples.ndjson"
    write_json(root / "manifest.json", {"runId": run_id, "environment": {"kind": "local-docker", "hostArchitecture": os.uname().machine}, "comparators": ["processbuilder", "taskcage"], "scenarios": args.scenarios, "concurrency": args.concurrency, "warmup": args.warmup, "iterations": args.iterations, "measurementBoundary": "worker submission through terminal result and normal output validation; excludes build, service start and warm-up"})
    dashboard = start_dashboard(root, args.port)
    print(f"Live dashboard: http://127.0.0.1:{args.port}/dashboard.html")
    env = os.environ | {"BENCHMARK_MAX_CONCURRENT_TASKS": str(args.concurrency)}
    executions: list[dict[str, Any]] = []
    completed = False
    validation = {"passed": False, "errors": ["run did not complete"]}
    try:
        state.update(phase="building"); append(events, {"type": "build_started"})
        command(COMPOSE + ["build", "--quiet", "taskcaged", "benchmark-worker"], env=env)
        append(events, {"type": "build_finished"})
        for scenario in args.scenarios:
            for mode in ("processbuilder", "taskcage"):
                state.update(phase="running", scenario=scenario, mode=mode); append(events, {"type": "execution_started", "scenario": scenario, "mode": mode})
                if mode == "taskcage": command(COMPOSE + ["up", "--detach", "--wait", "taskcaged"], env=env)
                worker_command = COMPOSE + ["run", "--rm", "--no-deps", "-e", f"BENCHMARK_MODE={mode}", "-e", f"BENCHMARK_SCENARIO={scenario}", "-e", f"BENCHMARK_CONCURRENCY={args.concurrency}", "-e", f"BENCHMARK_WARMUP={args.warmup}", "-e", f"BENCHMARK_ITERATIONS={args.iterations}", "benchmark-worker"]
                stop = threading.Event(); collector = threading.Thread(target=collect, args=(stop, state, samples), daemon=True); collector.start()
                try:
                    worker_process = command(worker_command, capture=True, check=False, env=env)
                    worker = json.loads(worker_process.stdout)
                finally:
                    stop.set(); collector.join(timeout=2)
                    # worker가 terminal result를 반환한 직후 daemon의 running_tasks=0을 보존한다.
                    collect_sample(state, samples)
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
        state.update(phase="completed", completed=True, report="report.html", validation=validation)
        completed = True
        print(f"Result JSON: {root / 'result.json'}\nReport: {root / 'report.html'}")
    finally:
        command(COMPOSE + ["down", "--volumes", "--remove-orphans"], env=env)
        if completed and args.keep_dashboard:
            print("Dashboard remains available after cleanup; press Ctrl-C to close it.")
            try:
                threading.Event().wait()
            except KeyboardInterrupt:
                pass
        dashboard.shutdown()
    if not validation["passed"]:
        raise SystemExit("benchmark intent validation failed")


def main() -> None:
    parser = argparse.ArgumentParser(description="TaskCage benchmark lab")
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run")
    run_parser.add_argument("--concurrency", type=int, default=2); run_parser.add_argument("--warmup", type=int, default=0); run_parser.add_argument("--iterations", type=int, default=1)
    run_parser.add_argument("--scenarios", nargs="+", default=["normal", "timeout_child", "memory_limit"]); run_parser.add_argument("--port", type=int, default=8765)
    run_parser.add_argument("--no-keep-dashboard", action="store_false", dest="keep_dashboard",
                            help="close the dashboard server immediately after writing the report")
    args = parser.parse_args()
    if args.concurrency < 1 or args.warmup < 0 or args.iterations < 1: parser.error("invalid run sizes")
    execute(args)


if __name__ == "__main__": main()
