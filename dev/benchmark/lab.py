#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Manual local benchmark lab; intentionally dependency-free."""
from __future__ import annotations

import argparse
import datetime as dt
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


def command(args: list[str], *, capture: bool = False, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=REPO, check=True, text=True, capture_output=capture, env=env)


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


def daemon_metrics() -> dict[str, float]:
    try:
        with urllib.request.urlopen("http://127.0.0.1:19098/metrics", timeout=.4) as response:
            text = response.read().decode()
    except OSError:
        return {}
    values = {}
    for line in text.splitlines():
        if line and not line.startswith("#") and "{" not in line:
            key, value = line.rsplit(" ", 1); values[key] = float(value)
    return values


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


def collect(stop: threading.Event, state: State, output: pathlib.Path) -> None:
    while not stop.wait(.5):
        sample = {
            "at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "daemonMetrics": daemon_metrics(),
            "daemonContainer": container_stats("taskcaged"),
            "workerContainer": container_stats("benchmark-worker"),
        }
        with output.open("a", encoding="utf-8") as file: file.write(json.dumps(sample) + "\n")
        state.sample(sample)


def render(result: dict[str, Any], destination: pathlib.Path) -> None:
    rows = []
    for execution in result["executions"]:
        tasks = execution["workerResult"]["tasks"]
        cleanup = execution["workerResult"]["cleanup"]
        rows.append(f"<tr><td>{execution['scenario']}</td><td>{execution['mode']}</td><td>{tasks['submitted']}</td><td>{tasks['normalTasks']['latencyMs']['p50']} ms</td><td>{tasks['normalTasks']['latencyMs']['p95']} ms</td><td>{cleanup['residualProcesses']}</td><td>{str(cleanup['cleanupConfirmed']).lower()}</td></tr>")
    destination.write_text(f'''<!doctype html><html lang="ko"><meta charset="utf-8"><title>TaskCage Benchmark Lab</title><style>body{{font-family:system-ui,sans-serif;max-width:960px;margin:40px auto;color:#17212b}}table{{border-collapse:collapse;width:100%}}td,th{{border:1px solid #d8dee4;padding:8px;text-align:left}}th{{background:#f4f7fa}}</style><h1>TaskCage Benchmark Lab 결과</h1><p>실행 ID: {result['runId']} · 환경: {result['environment']['kind']}</p><table><thead><tr><th>시나리오</th><th>실행기</th><th>Task</th><th>정상 p50</th><th>정상 p95</th><th>잔여 프로세스</th><th>정리 확인</th></tr></thead><tbody>{''.join(rows)}</tbody></table><h2>해석</h2><p>지연시간은 Worker 요청 제출부터 결과·정상 출력 검증까지이며 TaskCage SDK/UDS/daemon 비용을 포함합니다. 시작·build·warm-up은 제외합니다.</p><p>이 보고서는 로컬 실험 증거입니다. Docker Desktop 결과만으로 공개 성능을 주장하지 않으며, native Linux 반복 실행으로 재검증해야 합니다.</p></html>''', encoding="utf-8")


def execute(args: argparse.Namespace) -> None:
    if not shutil.which("docker"): raise SystemExit("docker is required")
    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    root = ROOT / "results" / "runs" / run_id; root.mkdir(parents=True)
    shutil.copy2(ROOT / "dashboard.html", root / "dashboard.html")
    state, events, samples = State(root), root / "events.ndjson", root / "samples.ndjson"
    write_json(root / "manifest.json", {"runId": run_id, "environment": {"kind": "local-docker", "hostArchitecture": os.uname().machine}, "comparators": ["processbuilder", "taskcage"], "scenarios": args.scenarios, "concurrency": args.concurrency, "measurementBoundary": "worker submission through terminal result and normal output validation; excludes build, service start and warm-up"})
    dashboard = start_dashboard(root, args.port)
    print(f"Live dashboard: http://127.0.0.1:{args.port}/dashboard.html")
    env = os.environ | {"BENCHMARK_MAX_CONCURRENT_TASKS": str(args.concurrency)}
    executions: list[dict[str, Any]] = []
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
                try: worker = json.loads(command(worker_command, capture=True, env=env).stdout)
                finally: stop.set(); collector.join(timeout=2)
                item = {"scenario": scenario, "mode": mode, "workerResult": worker}
                if mode == "taskcage":
                    command(COMPOSE + ["exec", "--no-TTY", "taskcaged", "taskcage-container-verify-cleanup"], env=env)
                    item["daemonCleanupVerified"] = True
                executions.append(item); append(events, {"type": "execution_finished", **item})
                command(COMPOSE + ["down", "--volumes", "--remove-orphans"], env=env)
        result = {"runId": run_id, "environment": {"kind": "local-docker"}, "executions": executions}
        write_json(root / "result.json", result); render(result, root / "report.html")
        state.update(phase="completed", completed=True, report="report.html")
        print(f"Result JSON: {root / 'result.json'}\nReport: {root / 'report.html'}")
    finally:
        command(COMPOSE + ["down", "--volumes", "--remove-orphans"], env=env); dashboard.shutdown()


def main() -> None:
    parser = argparse.ArgumentParser(description="TaskCage benchmark lab")
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run")
    run_parser.add_argument("--concurrency", type=int, default=2); run_parser.add_argument("--warmup", type=int, default=1); run_parser.add_argument("--iterations", type=int, default=3)
    run_parser.add_argument("--scenarios", nargs="+", default=["normal", "timeout_child", "memory_limit"]); run_parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    if args.concurrency < 1 or args.warmup < 0 or args.iterations < 1: parser.error("invalid run sizes")
    execute(args)


if __name__ == "__main__": main()
