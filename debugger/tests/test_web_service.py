from __future__ import annotations

import json
import queue
import subprocess
import sys
import threading
from pathlib import Path

import pytest
from test_fork_native import synthetic_reference

from chasm_debugger.fork import save_capture
from chasm_debugger.web.protocol import MAX_REQUEST_BYTES, WebError, json_value
from chasm_debugger.web.service import WebService

ROOT = Path(__file__).resolve().parents[1]
DEMO = ROOT / "examples" / "web-debugger"


class Worker:
    def __init__(self, command: list[str] | None = None) -> None:
        self.process = subprocess.Popen(
            command or [sys.executable, "-m", "chasm_debugger.web_service"],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        self.sequence = 0
        self.lines: queue.Queue[str] = queue.Queue()
        self.errors: list[str] = []

        def read_stdout():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put("")

        self.reader = threading.Thread(target=read_stdout, daemon=True)
        self.reader.start()

        def read_stderr():
            for line in self.process.stderr:
                self.errors.append(line)

        self.error_reader = threading.Thread(target=read_stderr, daemon=True)
        self.error_reader.start()

    def call(self, method: str, params: dict | None = None) -> dict:
        self.sequence += 1
        request = {"id": str(self.sequence), "method": method, "params": params or {}}
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        try:
            line = self.lines.get(timeout=30)
        except queue.Empty:
            self.process.kill()
            self.process.wait(timeout=10)
            pytest.fail("worker response timed out: " + "".join(self.errors))
        assert line, "worker stopped responding: " + "".join(self.errors)
        response = json.loads(line)
        assert response["id"] == str(self.sequence)
        return response

    def result(self, method: str, params: dict | None = None) -> dict:
        response = self.call(method, params)
        assert "error" not in response, response
        return response["result"]

    def close(self) -> None:
        if self.process.poll() is None:
            self.result("shutdown")
        self.process.wait(timeout=10)
        self.reader.join(timeout=5)
        self.error_reader.join(timeout=5)
        assert self.process.returncode == 0, "".join(self.errors)
        assert self.lines.get(timeout=5) == ""
        self.process.stdin.close()
        self.process.stdout.close()
        self.process.stderr.close()


@pytest.fixture
def worker():
    instance = Worker()
    try:
        yield instance
    finally:
        instance.close()


@pytest.fixture
def capture(tmp_path):
    plan, _ = synthetic_reference()
    plan.saved_witness["storage"].setdefault(plan.target["to"], {})["0x0"] = "0x0"
    baseline = plan.run_baseline()
    path = tmp_path / "capture.json"
    save_capture(path, plan, baseline)
    return path


def start_raw(worker, capture):
    return worker.result("start", {"mode": "offline", "capture_path": str(capture)})


def test_hello_and_fixed_dispatch(worker):
    hello = worker.result("hello")
    assert hello["protocol"] == "chasm-web-debugger/1"
    assert hello["engine_version"] == "43.0.2"
    assert not hello["capabilities"]["pause"]
    assert worker.call("__dict__")["error"]["code"] == "unknown_method"
    assert worker.call("snapshot")["error"]["code"] == "no_session"
    assert worker.call("hello", {"revision": 1})["error"]["code"] == "invalid_params"


def test_actual_offline_snapshot_mutation_restart_and_export(worker, capture, tmp_path):
    first = start_raw(worker, capture)
    assert first["status"]["pc"] == 0
    assert first["fork"]["baseline"]["validated"]
    assert first["capabilities"] == {
        "source": False,
        "solidity": False,
        "locals": False,
        "mutate": True,
        "restart": True,
        "reason": "raw bytecode; matching build-info is required for Solidity evaluation",
    }
    assert not first["panes"]["source"]["available"]
    assert first["panes"]["disassembly"]["rows"][0]["text"] == "STOP"
    assert len(first["panes"]["memory"]["items"]) == 64
    assert first["panes"]["storage"]["items"][0]["hex"] == "0x" + "0" * 64
    changed = worker.result(
        "mutate", {"target": "storage", "key": "0", "value": str((1 << 255) + 7)}
    )
    assert changed["snapshot"]["fork"]["diverged"]
    assert changed["mutation"]["value"] == hex((1 << 255) + 7)
    inspected = worker.result("snapshot", {"memoryOffset": "32", "storageSlots": ["0"]})
    assert inspected["panes"]["memory"]["offset"] == 32
    assert int(inspected["panes"]["storage"]["items"][0]["hex"], 16) == (1 << 255) + 7
    bundle = worker.result("export_capture")
    assert bundle["baseline"] == json.loads(capture.read_text())["baseline"]
    assert bundle["format"] == "chasm-fork-capture"
    exported = tmp_path / "exported.json"
    exported.write_text(json.dumps(bundle))
    replayed = worker.result("start", {"mode": "offline", "capture_path": str(exported)})
    assert replayed["fork"]["baseline"]["validated"]
    assert not replayed["fork"]["diverged"]
    restarted = worker.result("navigate", {"action": "restart"})
    assert restarted["status"]["pc"] == 0
    assert not restarted["fork"]["diverged"]
    finished = worker.result("navigate", {"action": "continue"})
    assert finished["status"]["finished"]
    assert finished["status"]["ok"]
    assert finished["status"]["execution"]["success"]
    assert "error" in finished["panes"]["stack"]


def test_breakpoint_and_per_pane_witness_error(worker, capture):
    start_raw(worker, capture)
    added = worker.result("breakpoint", {"action": "opcode", "opcode": "STOP"})
    assert added["breakpoints"]["breakpoints"]
    assert added["snapshot"]["breakpoints"] == added["breakpoints"]
    navigated = worker.result("navigate", {"action": "restart"})
    assert (
        navigated["breakpoints"]["breakpoints"][0]["id"]
        == added["breakpoints"]["breakpoints"][0]["id"]
    )
    assert worker.result("snapshot")["breakpoints"] == navigated["breakpoints"]
    listed = worker.result("breakpoint", {"action": "list"})
    number = listed["breakpoints"]["breakpoints"][0]["id"]
    deleted = worker.result("breakpoint", {"action": "delete", "id": number})
    assert deleted["snapshot"]["breakpoints"] == {"breakpoints": []}
    snapshot = worker.result("snapshot", {"storageSlots": ["999999"]})
    assert snapshot["panes"]["storage"]["error"]["code"] == "pane_unavailable"
    assert snapshot["panes"]["disassembly"]["rows"]
    assert snapshot["status"]["stopped"]


@pytest.mark.parametrize(
    "command",
    [
        "run @secret.txt",
        "source commands.txt",
        "shell whoami",
        "python print(1)",
        "q",
        "restart",
        "continue",
        'vm.envString("KEY")',
        "vm.sign(1, bytes32(0))",
        'vm.readFile("secret.txt")',
        'vm.ffi("whoami")',
        "export_trace p.json",
    ],
)
def test_reject_commands_before_processor(worker, capture, command):
    start_raw(worker, capture)
    assert worker.call("command", {"text": command})["error"]["code"] == "unsupported"
    assert not worker.result("snapshot")["fork"]["diverged"]


def test_allowed_commands_use_processor(worker, capture):
    start_raw(worker, capture)
    result = worker.result("command", {"text": "asm mstore(0x80, 42)"})
    assert result["command"]["ok"]
    assert int(result["snapshot"]["panes"]["memory"]["items"][4]["hex"], 16) == 42
    assert result["snapshot"]["fork"]["diverged"]
    cheat = worker.result("command", {"text": "vm.warp(123456)"})
    assert cheat["command"]["ok"]


@pytest.mark.parametrize(
    "method,params",
    [
        ("snapshot", {"memoryOffset": "-1"}),
        ("snapshot", {"memoryOffset": "16777217"}),
        ("snapshot", {"storageSlots": ["0"] * 65}),
        ("snapshot", {"storageSlots": [True]}),
        ("snapshot", {"aroundPc": 1.5}),
        ("navigate", {"action": "step_opcode", "count": 1001}),
        ("navigate", {"action": "step_opcode", "count": True}),
        ("evaluate", {"expression": "1", "keep": "false"}),
        ("mutate", {"target": "gas", "value": "-1"}),
        ("mutate", {"target": "memory", "offset": 16777216, "value": "0x1234"}),
        ("mutate", {"target": "variable", "name": "x; run", "value": "1"}),
        ("breakpoint", {"action": "delete", "id": True}),
    ],
)
def test_bounded_parameters(worker, capture, method, params):
    start_raw(worker, capture)
    assert worker.call(method, params)["error"]["code"] == "invalid_params"


def test_protocol_recovery_and_oversized_line():
    lines = [
        b'{"id":1,"method":"hello","params":{}}\n',
        b'{"id":"x","method":"hello","params":{},"revision":1}\n',
        b'{"id":"x","method":"hello","params":{},"id":"y"}\n',
        b'{"id":"x","method":"hello","params":{"a":NaN}}\n',
        b"x" * (MAX_REQUEST_BYTES + 100) + b"\n",
        b'{"id":"good","method":"hello","params":{}}\n',
        b'{"id":"bye","method":"shutdown","params":{}}\n',
    ]
    process = subprocess.run(
        [sys.executable, "-m", "chasm_debugger.web_service"],
        cwd=ROOT,
        input=b"".join(lines),
        capture_output=True,
        timeout=30,
    )
    assert process.returncode == 0, process.stderr
    responses = [json.loads(line) for line in process.stdout.splitlines()]
    assert len(responses) == len(lines)
    assert all(
        response["error"]["code"] == "invalid_request" for response in responses[:5]
    )
    assert responses[5]["id"] == "good"
    assert responses[6]["result"]["shutdown"]


def test_protocol_stdout_survives_python_and_descriptor_noise():
    code = """
import os, sys, subprocess
from chasm_debugger import web_service
output = web_service._protocol_stdout()
subprocess.run([sys.executable, '-c', "print('child compiler noise')"], check=True)
print('python compiler noise')
print('original python stdout noise', file=sys.__stdout__)
os.write(1, b'native compiler noise\\n')
raise SystemExit(web_service.serve(sys.stdin.buffer, output))
"""
    process = subprocess.run(
        [sys.executable, "-c", code],
        cwd=ROOT,
        input='{"id":"h","method":"hello","params":{}}\n',
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert process.returncode == 0
    assert len(process.stdout.splitlines()) == 1
    assert json.loads(process.stdout)["id"] == "h"
    assert "compiler noise" in process.stderr
    assert "original python stdout noise" in process.stderr


def test_large_values_and_nested_collections_are_bounded():
    result = json_value(
        {
            "word": 1 << 255,
            "stack_top": [{"hex": "0x" + "f" * 64, "decimal": None}],
            "small": 42,
            "flag": True,
            "rows": list(range(10000)),
            "metadata": {str(i): "x" * 9000 for i in range(200)},
        }
    )
    assert result["word"] == str(1 << 255)
    assert result["stack_top"][0]["decimal"] == str((1 << 256) - 1)
    assert result["small"] == 42
    assert result["flag"] is True
    assert len(result["rows"]) == 128
    assert len(result["metadata"]) == 128
    assert result["truncated"]
    assert len(result["truncation"]["paths"]) <= 128


def test_start_failure_cleans_session(worker, capture, tmp_path):
    start_raw(worker, capture)
    missing = worker.call(
        "start", {"mode": "offline", "capture_path": str(tmp_path / "missing.json")}
    )
    assert missing["error"]["code"] == "start_failed"
    assert worker.call("snapshot")["error"]["code"] == "no_session"
    assert start_raw(worker, capture)["status"]["stopped"]
    assert worker.call("start", {"mode": "local"})["error"]["code"] == "unsupported"


def test_raw_source_has_explicit_driver_error(capture):
    service = WebService()
    try:
        service.dispatch(
            {
                "id": "1",
                "method": "start",
                "params": {"mode": "offline", "capture_path": str(capture)},
            }
        )
        with pytest.raises(Exception, match="source unavailable"):
            service.driver.read_source()
        with pytest.raises(WebError, match="build-info"):
            service.dispatch(
                {"id": "2", "method": "evaluate", "params": {"expression": "1"}}
            )
    finally:
        service.driver.stop()


def test_mapped_source_evaluation_keep_discard_and_disassembly(worker):
    first = worker.result(
        "start",
        {
            "mode": "offline",
            "capture_path": str(DEMO / "source-replay.json"),
            "build_info_path": str(DEMO / "build-info.json"),
        },
    )
    assert first["capabilities"]["solidity"]
    worker.result("breakpoint", {"action": "add", "location": "WorkshopVault.withdraw"})
    stopped = worker.result("navigate", {"action": "continue"})
    assert stopped["status"]["location"]["function"] == "WorkshopVault.withdraw(uint256)"
    assert stopped["panes"]["source"]["items"]
    evaluated = worker.result("evaluate", {"expression": "feeBps"})
    assert evaluated["evaluation"]["display"] == "25"
    assert not evaluated["snapshot"]["fork"]["diverged"]
    discarded = worker.result("evaluate", {"expression": "feeBps = 40", "keep": False})
    assert not discarded["evaluation"]["kept"]
    assert (
        worker.result("evaluate", {"expression": "feeBps"})["evaluation"]["display"]
        == "25"
    )
    kept = worker.result("evaluate", {"expression": "feeBps = 40", "keep": True})
    assert kept["evaluation"]["kept"]
    assert kept["snapshot"]["fork"]["diverged"]
    assert (
        worker.result("evaluate", {"expression": "feeBps"})["evaluation"]["display"]
        == "40"
    )
    current_pc = stopped["status"]["pc"]
    around = worker.result("snapshot", {"aroundPc": "0", "sourceStart": 1})
    assert around["panes"]["disassembly"]["rows"][0]["pc"] == 0
    for row in around["panes"]["disassembly"]["rows"]:
        assert row["current"] == (row["pc"] == current_pc)
    assert len(around["panes"]["source"]["items"]) <= 101
    assert (
        worker.result("evaluate", {"expression": "balances[msg.sender]"})["evaluation"][
            "display"
        ]
        == "10000"
    )
    worker.result("breakpoint", {"action": "add", "location": "WorkshopVault._fee"})
    fee = worker.result("navigate", {"action": "continue"})
    assert fee["status"]["location"]["function"] == "WorkshopVault._fee(uint256)"
    assert fee["panes"]["locals"]["locals"]
    listing = worker.result("breakpoint", {"action": "list"})["breakpoints"][
        "breakpoints"
    ]
    for bp in listing:
        worker.result("breakpoint", {"action": "delete", "id": str(bp["id"])})
    finished = worker.result("navigate", {"action": "continue"})
    assert finished["status"]["finished"]
    bundle = worker.result("export_capture")
    assert bundle["baseline"]["validated"]
    assert not any("rpc_url" in key for key in bundle)


def test_foundry_safe_mode_requires_existing_setup(worker, tmp_path):
    path = tmp_path / "Lone.t.sol"
    path.write_text(
        "pragma solidity ^0.8.28; contract Lone { function testOne() public {} }"
    )
    before = set(tmp_path.rglob("*"))
    result = worker.call("start", {"mode": "foundry", "target_path": str(path)})
    assert result["error"]["code"] == "start_failed"
    assert "foundry.toml" in result["error"]["message"]
    assert set(tmp_path.rglob("*")) == before


@pytest.mark.parametrize(
    "verbatim", [False, True] if sys.platform == "win32" else [False]
)
def test_foundry_safe_mode_runs_without_project_writes(worker, tmp_path, verbatim):
    (tmp_path / "foundry.toml").write_text(
        '[profile.default]\nsolc_version = "0.8.28"\nevm_version = "cancun"\nlibs = []\n'
    )
    (tmp_path / "src").mkdir()
    (tmp_path / "test").mkdir()
    (tmp_path / "src" / "Vault.sol").write_text(
        "pragma solidity ^0.8.28;\ncontract Vault {\n uint256 public amount;\n}\n"
    )
    target = tmp_path / "test" / "Standalone.t.sol"
    target.write_text(
        'pragma solidity ^0.8.28;\nimport {Vault} from "../src/Vault.sol";\ncontract Standalone is Vault {\n function testOne() public {\n  amount = 42;\n  require(amount == 42);\n }\n}\n'
    )
    target_path = "\\\\?\\" + str(target) if verbatim else str(target)
    before = {path: path.read_bytes() for path in tmp_path.rglob("*") if path.is_file()}
    started = worker.result(
        "start", {"mode": "foundry", "target_path": target_path, "match": "testOne"}
    )
    assert started["status"]["stopped"]
    assert started["capabilities"]["solidity"]
    assert not started["capabilities"]["restart"]
    assert worker.result("navigate", {"action": "continue"})["status"]["finished"]
    assert {
        path: path.read_bytes() for path in tmp_path.rglob("*") if path.is_file()
    } == before


def test_finished_worker_ok_is_not_evm_success(worker, tmp_path):
    from chasm_debugger.fork import ReplayPlan

    plan, witness = synthetic_reference()
    metadata = plan.metadata
    target = metadata["target_record"]
    target["receipt"].update(status=0, gas_used=21004, cumulative_gas_used=42004)
    witness["accounts"][target["request"]["to"]]["code"] = "0x5f5ffd"
    prefix = metadata["prefix"][0]
    prefix["receipt"].update(status=1, gas_used=21000, cumulative_gas_used=21000)
    witness["accounts"][prefix["request"]["to"]]["code"] = "0x00"
    reverted = ReplayPlan(metadata, witness=witness)
    baseline = reverted.run_baseline()
    path = tmp_path / "revert.json"
    save_capture(path, reverted, baseline)
    start_raw(worker, path)
    stopped = worker.result("navigate", {"action": "continue"})
    assert stopped["status"]["stop_reason"] == "error"
    finished = worker.result("navigate", {"action": "continue"})
    assert finished["status"]["ok"] is True
    assert finished["status"]["execution"]["success"] is False
    assert finished["fork"]["baseline"]["validated"]
