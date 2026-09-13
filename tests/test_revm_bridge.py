"""The REVM spike crosses the Python boundary without owning the GIL."""

from __future__ import annotations

import json
import subprocess
import sys
import threading
import time

from sevm._revm import RevmSession, revm_version


def test_live_mutation_crosses_the_python_bridge():
    session = RevmSession(bytes.fromhex("60015f5500"), stop_pc=3)
    paused = session.wait()
    assert paused["type"] == "paused"
    assert paused["reason"] == "breakpoint"
    assert paused["stack"] == ["0x0", "0x1"]

    assert session.set_stack(1, 9) == "0x9"
    assert session.write_memory(3, b"\xaa\xbb") == 2
    assert session.write_storage(7, 8) == "0x8"
    assert session.snapshot()["memory"][3:5] == b"\xaa\xbb"
    session.resume()

    finished = session.wait()
    assert finished["type"] == "finished"
    assert finished["success"]
    storage = {(address, key): value for address, key, value in finished["storage"]}
    target = "0x1000000000000000000000000000000000000001"
    assert storage[(target, "0x0")] == "0x9"
    assert storage[(target, "0x7")] == "0x8"


def test_wait_releases_the_gil():
    session = RevmSession(bytes.fromhex("5b00"), stop_pc=0)
    assert session.wait()["type"] == "paused"
    result = []
    waiter = threading.Thread(target=lambda: result.append(session.wait()))
    waiter.start()
    time.sleep(0.05)
    session.resume()
    waiter.join(timeout=2)

    assert not waiter.is_alive()
    assert result[0]["type"] == "finished"
    assert revm_version() == "43.0.2"


def test_installed_headless_launcher_uses_json_rpc():
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}},
    ]
    result = subprocess.run(
        [sys.executable, "-m", "sevm.revm_server"],
        input="".join(json.dumps(request) + "\n" for request in requests),
        capture_output=True,
        text=True,
        check=True,
    )
    responses = [json.loads(line) for line in result.stdout.splitlines()]
    assert result.stderr == ""
    assert responses[0]["result"]["protocol"] == "sevm-debugger/1"
    assert responses[1]["result"] is None
