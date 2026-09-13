"""The source-level Foundry path runs on REVM without a Py-EVM chain."""

from __future__ import annotations

import pytest

from sevm.evaluate import Evaluator, make_eval_hook
from sevm.foundry import RevmTestsDriver, discover_tests, select_test
from sevm.session import Finished, Paused, RevmDebugSession, StepMode

TIMEOUT = 30.0


def _driver(project, contract: str, function: str) -> RevmTestsDriver:
    target = select_test(
        discover_tests(project),
        match=function,
        match_contract=contract,
    )
    assert target is not None
    return RevmTestsDriver(project, (target,))


def test_revm_foundry_session_stops_and_steps_in_source(token_project):
    session = RevmDebugSession(token_project)
    session.break_at_function("TokenTest.testMintAsOwner")
    session.start(_driver(token_project, "TokenTest", "testMintAsOwner"))
    try:
        event = session.wait(timeout=TIMEOUT)
        assert isinstance(event, Paused)
        assert event.snapshot.contract_name == "TokenTest"
        assert event.snapshot.function is not None
        assert event.snapshot.function.name == "testMintAsOwner"
        before = event.snapshot.step

        event = session.resume(StepMode.STEPI, timeout=TIMEOUT)
        assert isinstance(event, Paused)
        assert event.snapshot.step == before + 1

        event = session.resume(StepMode.RUN, timeout=TIMEOUT)
        assert isinstance(event, Finished)
        assert event.ok
    finally:
        session.detach(timeout=TIMEOUT)


def test_revm_foundry_session_evaluates_and_keeps_state(token_project):
    session = RevmDebugSession(token_project)
    session.set_eval_hook(make_eval_hook(Evaluator(token_project)))
    session.break_at_function("TokenTest.testMintAsOwner")
    session.start(_driver(token_project, "TokenTest", "testMintAsOwner"))
    try:
        event = session.wait(timeout=TIMEOUT)
        assert isinstance(event, Paused)
        assert (
            session.inspect("evaluate", "bob").value
            == "0x0000000000000000000000000000000000000b0b"
        )

        session.inspect("evaluate", "bob = address(0x1234)", keep=True)
        assert (
            session.inspect("evaluate", "bob").value
            == "0x0000000000000000000000000000000000001234"
        )

        event = session.resume(StepMode.RUN, timeout=TIMEOUT)
        assert isinstance(event, Finished)
        assert event.ok
    finally:
        session.detach(timeout=TIMEOUT)


def test_revm_foundry_session_honors_breakpoint_conditions(token_project):
    session = RevmDebugSession(token_project)
    session.set_eval_hook(make_eval_hook(Evaluator(token_project)))
    breakpoint, _ = session.break_at_function(
        "TokenTest.testMintAsOwner",
        condition="bob == address(0xCAFE)",
    )
    session.start(_driver(token_project, "TokenTest", "testMintAsOwner"))
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert event.ok
    assert breakpoint.hit_count == 0
    assert breakpoint.condition_error is None


def test_revm_foundry_session_rewrites_a_live_local(solo_project):
    session = RevmDebugSession(solo_project)
    source = solo_project.sources["AllCheats.t.sol"].text.splitlines()
    line = next(index for index, text in enumerate(source, 1) if "assertTrue(a1" in text)
    session.break_at_line("AllCheats.t.sol", line)
    session.start(_driver(solo_project, "AllCheatsTest", "testStorageAndKeys"))
    try:
        event = session.wait(timeout=TIMEOUT)
        assert isinstance(event, Paused)
        assert event.snapshot.contract_name == "AllCheatsTest"
        locals_before = {item["name"]: item for item in session.inspect("locals")}
        original = int(locals_before["a1"]["value"], 16)

        changed = session.inspect("write_local", "a1", 7)
        assert changed["display"] == "0x0000000000000000000000000000000000000007"
        session.inspect("write_local", "a1", original)

        event = session.resume(StepMode.RUN, timeout=TIMEOUT)
        assert isinstance(event, Finished)
        assert event.ok
    finally:
        session.detach(timeout=TIMEOUT)


def test_revm_foundry_session_runs_yul_on_the_live_frame(token_project):
    session = RevmDebugSession(token_project)
    session.break_at_function("TokenTest.testMintAsOwner")
    session.start(_driver(token_project, "TokenTest", "testMintAsOwner"))
    try:
        event = session.wait(timeout=TIMEOUT)
        assert isinstance(event, Paused)
        before_stack = event.snapshot.stack
        before_gas = event.snapshot.gas_remaining

        rows = session.inspect(
            "assembly",
            "mstore(0x80, 0xdeadbeef); sstore(99, add(40, 2)); "
            "sload(123); log1(0x80, 32, 0xabc); mload(0x80)",
        )
        assert rows[-1]["value"] == 0xDEADBEEF
        assert session.inspect("read_storage", 99) == 42
        assert session.inspect("is_warm", 123)
        logs = session.inspect("logs")
        assert logs[-1][0] == event.snapshot.address
        assert logs[-1][1] == (0xABC,)
        assert int.from_bytes(logs[-1][2], "big") == 0xDEADBEEF
        assert (
            int.from_bytes(session.inspect("read_memory", 0x80, 32), "big") == 0xDEADBEEF
        )
        snapshot = session.refresh_snapshot()
        assert snapshot is not None
        assert snapshot.stack == before_stack
        assert snapshot.gas_remaining == before_gas

        event = session.resume(StepMode.RUN, timeout=TIMEOUT)
        assert isinstance(event, Finished)
        assert event.ok
    finally:
        session.detach(timeout=TIMEOUT)


def test_revm_foundry_session_applies_prank(token_project):
    session = RevmDebugSession(token_project)
    session.start(_driver(token_project, "TokenTest", "testMintPrankRevertsForNonOwner"))
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert event.ok


def test_revm_foundry_session_breaks_in_a_contract_created_during_setup(token_project):
    session = RevmDebugSession(token_project)
    session.break_at_function("Token.mint")
    session.start(_driver(token_project, "TokenTest", "testMintAsOwner"))
    try:
        event = session.wait(timeout=TIMEOUT)
        assert isinstance(event, Paused)
        assert event.snapshot.contract_name == "Token"
        assert event.snapshot.function is not None
        assert event.snapshot.function.name == "mint"

        event = session.resume(StepMode.RUN, timeout=TIMEOUT)
        assert isinstance(event, Finished)
        assert event.ok
    finally:
        session.detach(timeout=TIMEOUT)


def test_revm_foundry_session_returns_assertion_reverts(failing_project):
    session = RevmDebugSession(failing_project)
    session.start(_driver(failing_project, "DemoTest", "testFails"))
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert not event.ok
    assert "1 != 2" in str(session.last_revert)


@pytest.mark.parametrize(
    "function",
    [
        "testEnv",
        "testStorageAndKeys",
        "testBlockGetters",
        "testFeeIsNotChargedAtSettlement",
        "testNonceCheats",
        "testPrankValue",
        "testPrank",
        "testPrankOrigin",
        "testDelegatePrank",
        "testStartPrankDelegate",
    ],
)
def test_revm_foundry_session_applies_stateful_cheats(solo_project, function):
    session = RevmDebugSession(solo_project)
    session.start(_driver(solo_project, "AllCheatsTest", function))
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert event.ok, session.exit_error


def test_revm_foundry_session_captures_console_logs(solo_project):
    session = RevmDebugSession(solo_project)
    session.start(_driver(solo_project, "AllCheatsTest", "testEnv"))
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert event.ok
    assert any("env ok at 4242" in line for line in session.cheats.console_lines)
