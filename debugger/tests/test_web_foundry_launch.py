"""Foundry launches over the web worker: breakpoints before the first step,
the entry pause, and the output pane that says how a run ended."""

from __future__ import annotations

import pytest
from test_web_service import Worker


@pytest.fixture
def worker():
    instance = Worker()
    try:
        yield instance
    finally:
        instance.close()


def foundry_project(tmp_path):
    (tmp_path / "foundry.toml").write_text(
        '[profile.default]\nsolc_version = "0.8.28"\nevm_version = "cancun"\nlibs = []\n'
    )
    (tmp_path / "test").mkdir()
    target = tmp_path / "test" / "Steps.t.sol"
    target.write_text(
        "pragma solidity ^0.8.28;\n"
        "contract Steps {\n"
        " uint256 public amount;\n"
        " function testOne() public {\n"
        "  amount = 1;\n"
        "  amount = 2;\n"
        "  require(amount == 2);\n"
        " }\n"
        "}\n"
    )
    return target


def test_launch_breakpoints_apply_before_the_first_step(worker, tmp_path):
    target = foundry_project(tmp_path)
    started = worker.result(
        "start",
        {
            "mode": "foundry",
            "target_path": str(target),
            "match": "testOne",
            "pause_at_entry": False,
            "breakpoints": ["Steps.t.sol:6", "Nope.sol:1"],
        },
    )
    status = started["status"]
    assert status["stopped"] and status["location"]["line"] == 6, status
    lines = started["panes"]["output"]["lines"]
    assert any(line.startswith("breakpoint ") and "Steps.t.sol" in line for line in lines), lines
    assert any("'Nope.sol:1' not set" in line for line in lines), lines
    finished = worker.result("navigate", {"action": "continue"})
    assert finished["status"]["finished"]


def test_the_default_pauses_at_the_test_entry(worker, tmp_path):
    target = foundry_project(tmp_path)
    paused = worker.result(
        "start", {"mode": "foundry", "target_path": str(target), "match": "testOne"}
    )
    assert paused["status"]["stopped"] and paused["status"]["location"]["line"] == 5, paused["status"]


def test_a_run_without_stops_says_why_it_finished(worker, tmp_path):
    target = foundry_project(tmp_path)
    started = worker.result(
        "start", {"mode": "foundry", "target_path": str(target), "pause_at_entry": False}
    )
    assert started["status"]["finished"] and started["status"]["ok"], started["status"]
    assert any(
        "ran to the end without stopping and passed" in line
        for line in started["panes"]["output"]["lines"]
    ), started["panes"]["output"]


def test_launch_parameters_are_validated(worker, tmp_path):
    target = foundry_project(tmp_path)
    for bad in ("Steps.t.sol:6", [1], ["x"] * 33, ["a\nb"], [""]):
        result = worker.call(
            "start", {"mode": "foundry", "target_path": str(target), "breakpoints": bad}
        )
        assert result["error"]["code"] == "invalid_params", bad
    result = worker.call(
        "start", {"mode": "foundry", "target_path": str(target), "pause_at_entry": "no"}
    )
    assert result["error"]["code"] == "invalid_params"


def test_a_compile_error_names_the_problem(worker, tmp_path):
    target = foundry_project(tmp_path)
    target.write_text(
        "pragma solidity ^0.8.28;\ncontract Broken { function f() public { x = 1; } }\n"
    )
    result = worker.call("start", {"mode": "foundry", "target_path": str(target)})
    assert result["error"]["code"] == "start_failed"
    message = result["error"]["message"]
    assert "compile failed" in message and "Undeclared identifier" in message, message
