"""What `sevm doctor` reports.

The point of the command is to answer, on a machine sevm has never worked on, which
compiler it would use and what is missing — so the tests are about which facts get
marked as problems, since that is what the exit status is built from.
"""

from __future__ import annotations

import pytest

from sevm import doctor
from sevm.compile import solcbin, wasm


@pytest.fixture
def machine(monkeypatch):
    """A machine with a working native solc and a JS runtime, adjustable per test."""
    monkeypatch.setattr(solcbin, "override", lambda: None)
    monkeypatch.setattr(solcbin, "find", lambda version: "/home/u/.solcx/solc-v0.8.28")
    monkeypatch.setattr(solcbin, "installed_versions", lambda: ["0.8.28"])
    monkeypatch.setattr(
        solcbin,
        "release_index",
        lambda: {"0.8.28": solcbin.Release("0.8.28", "https://example/solc", "ab")},
    )
    monkeypatch.setattr(wasm, "runtime", lambda: "/usr/bin/node")
    monkeypatch.setattr(doctor, "_tool_version", lambda command, *args: "git version 2")
    return monkeypatch


def _value(lines, label):
    return next(line for line in lines if line.label == label).value


def test_a_working_machine_reports_no_problems(machine):
    lines = doctor.report()

    assert doctor.ok(lines)
    assert "0.8.28" in _value(lines, "solc")
    assert "1 releases" in _value(lines, "available")


def test_a_compiler_that_is_not_installed_yet_names_its_download(machine):
    machine.setattr(solcbin, "find", lambda version: None)
    machine.setattr(solcbin, "installed_versions", lambda: [])

    lines = doctor.report()
    assert doctor.ok(lines)
    assert "https://example/solc" in _value(lines, "solc")


def test_no_build_and_no_runtime_is_a_problem(machine):
    machine.setattr(solcbin, "find", lambda version: None)
    machine.setattr(solcbin, "installed_versions", lambda: [])
    machine.setattr(solcbin, "release_index", dict)
    machine.setattr(wasm, "runtime", lambda: None)

    lines = doctor.report()
    assert not doctor.ok(lines)
    assert "!" in doctor.render(lines)


def test_no_build_but_a_runtime_is_fine(machine):
    # An unpublished architecture is only a problem if the wasm build is out of reach.
    machine.setattr(solcbin, "find", lambda version: None)
    machine.setattr(solcbin, "installed_versions", lambda: [])
    machine.setattr(solcbin, "release_index", dict)

    lines = doctor.report()
    assert doctor.ok(lines)
    assert "WebAssembly" in _value(lines, "available")


def test_an_override_that_does_not_run_is_a_problem(machine):
    machine.setattr(solcbin, "override", lambda: "/opt/solc")
    machine.setattr(solcbin, "override_version", lambda: None)

    lines = doctor.report()
    assert not doctor.ok(lines)
    assert "DOES NOT RUN" in _value(lines, "solc")


def test_missing_git_fails_even_with_a_compiler(machine):
    machine.setattr(doctor, "_tool_version", lambda command, *args: None)

    lines = doctor.report()
    assert not doctor.ok(lines)
    assert "library resolution" in _value(lines, "git")


def test_the_command_exits_with_the_verdict(machine, capsys):
    from sevm.cli import main

    assert main(["doctor"]) == 0
    assert "platform" in capsys.readouterr().out

    machine.setattr(doctor, "_tool_version", lambda command, *args: None)
    assert main(["doctor"]) == 1
