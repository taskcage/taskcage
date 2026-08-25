import importlib.util
import pathlib
import tempfile
import unittest


LAB_PATH = pathlib.Path(__file__).with_name("lab.py")
SPEC = importlib.util.spec_from_file_location("benchmark_lab", LAB_PATH)
assert SPEC is not None and SPEC.loader is not None
lab = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lab)


class BenchmarkLabTest(unittest.TestCase):
    def test_prometheus_parser_keeps_labeled_metrics(self):
        parsed = lab.parse_prometheus(
            '# TYPE taskcage_tasks_finished_total counter\n'
            'taskcage_running_tasks 0\n'
            'taskcage_tasks_finished_total{outcome="memory_limit_exceeded"} 2\n'
        )

        self.assertEqual(parsed["taskcage_running_tasks"], 0)
        self.assertEqual(parsed['taskcage_tasks_finished_total{outcome="memory_limit_exceeded"}'], 2)

    def test_result_validation_propagates_worker_intent_failures(self):
        result = lab.result_validation([{
            "scenario": "memory_limit",
            "mode": "taskcage",
            "workerExitCode": 2,
            "workerResult": {"validation": {
                "passed": False,
                "errors": ["expected MEMORY_LIMIT_EXCEEDED but got TIMED_OUT"],
            }},
        }])

        self.assertFalse(result["passed"])
        self.assertIn("memory_limit/taskcage", result["errors"][0])

    def test_report_contains_terminal_and_resource_metrics(self):
        result = {
            "runId": "fixture",
            "environment": {"kind": "test"},
            "validation": {"passed": True, "errors": []},
            "executions": [{
                "scenario": "normal",
                "mode": "processbuilder",
                "workerExitCode": 0,
                "workerResult": {
                    "tasks": {"submitted": 1, "latencyMs": {"p50": 20, "p95": 25},
                              "normalTasks": {"latencyMs": {"p50": 20, "p95": 25}},
                              "terminationReasons": {"EXITED": 1}},
                    "taskResources": {"memoryPeakBytes": 16 * 1024 * 1024, "cpuTimeMicros": 1_000},
                    "executorContainer": {"memoryPeakBytes": 32 * 1024 * 1024, "cpuUsageMicros": 2_000},
                    "cleanup": {"residualProcesses": 0, "cleanupConfirmed": True},
                },
                "executionMemoryUpperBound": {"memoryPeakBytes": 32 * 1024 * 1024, "components": []},
            }],
        }
        with tempfile.TemporaryDirectory() as directory:
            report = pathlib.Path(directory) / "report.html"
            lab.render(result, report)
            contents = report.read_text()

        self.assertIn("정상 변환: Java 호출 기준 비교", contents)
        self.assertIn("32.0 MiB", contents)
        self.assertIn("p50 20 ms", contents)


if __name__ == "__main__":
    unittest.main()
