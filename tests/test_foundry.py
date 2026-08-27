"""Foundry-test entry point and cheatcode engine.

Compiles the two fixture layouts under tests/ (a standalone test whose forge-std is
installed from a local repo, and a project with its own lib/forge-std + remappings) and
drives them through the debug session to prove the cheatcodes actually take effect.
"""

from __future__ import annotations

import os

import pytest

from sevm.cheatcodes import (
    CheatError,
    CheatState,
    all_specs,
    apply_cheat,
    decode_console_log,
    encode_cheat_call,
    listing,
    parse_cheat_arg,
    spec_by_name,
)
from sevm.evaluate import Evaluator, make_eval_hook
from sevm.foundry import (
    discover_tests,
    make_test_driver,
    prepare_project,
    select_test,
)
from sevm.session import DebugSession, Finished, Paused, StepMode

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.join(HERE, "foundry_project")
PROJECT_TEST = os.path.join(PROJECT, "test", "Token.t.sol")
TIMEOUT = 30.0


def run_to_finish(project, contract: str, function: str):
    """Deploy + setUp + run a test to completion; return (session, final_event)."""
    targets = discover_tests(project)
    target = select_test(targets, match=function, match_contract=contract)
    assert target is not None, f"no target {contract}.{function}"
    session = DebugSession(project)
    session.foundry_mode = True
    evaluator = Evaluator(project)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.start(make_test_driver(project, target))
    event = session.wait(timeout=TIMEOUT)
    for _ in range(200):
        if isinstance(event, Finished):
            break
        event = session.resume(StepMode.RUN, count=1, timeout=TIMEOUT)
    try:
        session.detach(timeout=TIMEOUT)
    except Exception:
        session.uninstall()
    return session, event


# ==================================================================
# compilation and project resolution
# ==================================================================


def test_standalone_installs_forge_std(solo_project, solo_root):
    names = {a.name for a in solo_project.artifacts.values()}
    assert {"AllCheatsTest", "Recorder", "Test", "Vm"} <= names
    assert any("forge-std/=" in r for r in solo_project.remappings)
    # forge-std was cloned into the target's own lib/, the way forge install leaves it.
    test_src = solo_project.sources["lib/forge-std/src/Test.sol"]
    assert os.path.abspath(solo_root) in os.path.abspath(test_src.abs_path)
    assert os.path.isdir(os.path.join(solo_root, "lib", "forge-std", ".git"))


def test_existing_project_lib_is_used_as_is(token_project, token_root):
    names = {a.name for a in token_project.artifacts.values()}
    assert {"TokenTest", "Token", "Test", "Vm"} <= names
    # forge-std came from the project's own lib/; nothing was fetched.
    test_src = token_project.sources["lib/forge-std/src/Test.sol"]
    assert os.path.abspath(token_root) in os.path.abspath(test_src.abs_path)


def test_prepare_project_finds_foundry_root():
    prepared = prepare_project(PROJECT_TEST, assume_yes=False)
    assert prepared.existing is True
    assert os.path.abspath(prepared.root) == os.path.abspath(PROJECT)


def test_discover_and_select_tests(solo_project):
    targets = discover_tests(solo_project)
    fns = {(t.contract, t.function) for t in targets}
    assert ("AllCheatsTest", "testEnv") in fns
    assert all(t.has_setup for t in targets)
    picked = select_test(targets, match="testPrank")
    assert picked is not None and picked.function == "testPrank"
    assert select_test(targets, match="nope") is None


# ==================================================================
# cheatcodes take effect end to end (internal assertEq proves it)
# ==================================================================


@pytest.mark.parametrize(
    "fn", ["testEnv", "testPrank", "testStorageAndKeys", "testPrankValue"]
)
def test_cheats_take_effect(fn, solo_project):
    session, event = run_to_finish(solo_project, "AllCheatsTest", fn)
    assert isinstance(event, Finished)
    assert event.ok, f"{fn} reverted: {session.exit_error}"


def test_failed_assertion_reverts_with_its_message(failing_project):
    # forge-std routes a failed assertEq through vm.assertEq, so the revert reason has to
    # come out of sevm's cheat engine, not out of a plain Solidity require.
    session, event = run_to_finish(failing_project, "DemoTest", "testFails")
    assert isinstance(event, Finished) and not event.ok
    assert "DemoTest.testFails reverted" in str(session.exit_error)
    assert "1 != 2" in str(session.last_revert)


def test_console_log_captured(solo_project):
    session, event = run_to_finish(solo_project, "AllCheatsTest", "testEnv")
    assert isinstance(event, Finished) and event.ok
    assert any("env ok at 4242" in line for line in session.cheats.console_lines)


def test_project_prank_reverts_non_owner(token_project):
    # A pranked non-owner mint must revert (caught inside the test); the test passes.
    session, event = run_to_finish(
        token_project, "TokenTest", "testMintPrankRevertsForNonOwner"
    )
    assert isinstance(event, Finished) and event.ok


def test_multi_test_stops_at_each_test(solo_project):
    from sevm.foundry import make_tests_driver

    project = solo_project
    targets = discover_tests(project)
    names = {f"{t.contract}.{t.function}" for t in targets}
    session = DebugSession(project)
    session.foundry_mode = True
    evaluator = Evaluator(project)
    session.set_eval_hook(make_eval_hook(evaluator))
    for t in targets:
        session.break_at_function(f"{t.contract}.{t.function}")
    session.start(make_tests_driver(project, targets))
    event = session.wait(timeout=TIMEOUT)
    seen: set[str] = set()
    for _ in range(400):
        if isinstance(event, Finished):
            break
        snap = session.last_snapshot
        fn = getattr(snap, "function", None)
        if snap is not None and snap.stop_reason == "breakpoint" and fn is not None:
            seen.add(f"{fn.contract}.{fn.name}")
        event = session.resume(StepMode.RUN, count=1, timeout=TIMEOUT)
    try:
        session.detach(timeout=TIMEOUT)
    except Exception:
        session.uninstall()
    # Every test body was stopped at, and nothing else (proves contract-scoped breakpoints).
    assert seen == names


def test_session_opens_at_test_function(solo_project):
    project = solo_project
    session = DebugSession(project)
    session.foundry_mode = True
    evaluator = Evaluator(project)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.break_at_function("AllCheatsTest.testEnv", temporary=True)
    target = select_test(discover_tests(project), match="testEnv")
    session.start(make_test_driver(project, target))
    session.wait(timeout=TIMEOUT)
    event = session.resume(StepMode.RUN, count=1, timeout=TIMEOUT)
    try:
        assert isinstance(event, Paused)
        assert event.snapshot.stop_reason == "breakpoint"
        assert event.snapshot.function.name == "testEnv"
        assert event.snapshot.function.contract == "AllCheatsTest"
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()


# ==================================================================
# cheatcode engine units
# ==================================================================


def _assert_cheat(name, values):
    """Run one `vm.assert*` the way a test contract would, via the registry."""
    apply_cheat(CheatState(), None, encode_cheat_call(name, values), caller=None)


@pytest.mark.parametrize(
    "name,values",
    [
        ("assertEq", [7, 7]),
        ("assertEq", ["0x" + "11" * 20, "0x" + "11" * 20]),
        ("assertEq", ["same", "same"]),
        ("assertNotEq", [1, 2]),
        ("assertTrue", [True]),
        ("assertFalse", [False]),
        ("assertGt", [2, 1]),
        ("assertGe", [1, 1]),
        ("assertLt", [1, 2]),
        ("assertLe", [1, 1]),
        ("assertEqDecimal", [10**18, 10**18, 18]),
        ("assertApproxEqAbs", [100, 95, 5]),
        ("assertApproxEqRel", [101, 100, 10**16]),  # 1% allowed, 1% off
    ],
)
def test_assert_cheats_pass_quietly(name, values):
    _assert_cheat(name, values)


@pytest.mark.parametrize(
    "name,values,message",
    [
        ("assertEq", [1, 2], "assertion failed: 1 != 2"),
        ("assertNotEq", [2, 2], "assertion failed: 2 == 2"),
        ("assertTrue", [False], "assertion failed"),
        ("assertFalse", [True], "assertion failed"),
        ("assertGt", [1, 2], "assertion failed: 1 <= 2"),
        ("assertLt", [2, 1], "assertion failed: 2 >= 1"),
        (
            "assertEqDecimal",
            [10**18, 2 * 10**18, 18],
            "assertion failed: 1.000000000000000000 != 2.000000000000000000",
        ),
        ("assertApproxEqAbs", [100, 90, 5], "real delta: 10"),
        ("assertApproxEqRel", [110, 100, 10**16], "real delta: 10.0000000000000000%"),
    ],
)
def test_assert_cheats_revert_with_the_comparison(name, values, message):
    with pytest.raises(CheatError) as exc:
        _assert_cheat(name, values)
    assert message in str(exc.value)


def test_assert_cheat_keeps_a_custom_message():
    with pytest.raises(CheatError, match="balances drifted: 1 != 2"):
        _assert_cheat("assertEq", [1, 2, "balances drifted"])


def test_assert_overload_follows_the_literal_types():
    # `1` encodes as bool just as happily as uint256; the numeric overload must win, or a
    # failing assertEq(1, 2) would silently pass as assertEq(true, true).
    with pytest.raises(CheatError, match="1 != 2"):
        _assert_cheat("assertEq", [1, 2])
    _assert_cheat("assertEq", [True, True])


def test_every_cheat_is_documented():
    undocumented = [spec.signature for spec in all_specs() if not spec.doc]
    assert not undocumented


def test_help_listing_collapses_the_assert_family():
    rows = listing()
    families = [row for row in rows if row.family == "assert"]
    assert len(families) == 1
    assert "116 overloads" in families[0].doc
    assert all(not row.name.startswith("assert") or row.family for row in rows)


def test_apply_cheat_unimplemented_raises():
    from eth_utils import function_signature_to_4byte_selector

    with pytest.raises(CheatError):
        apply_cheat(
            CheatState(),
            None,
            function_signature_to_4byte_selector("expectRevert()"),
            caller=None,
        )


def test_parse_cheat_arg_units():
    assert parse_cheat_arg("1 ether") == 10**18
    assert parse_cheat_arg("42") == 42
    assert parse_cheat_arg("0xA11cE").lower() == "0xa11ce"
    assert parse_cheat_arg('"hi"') == "hi"


def test_encode_cheat_call_roundtrips():
    data = encode_cheat_call("warp", [12345])
    spec = spec_by_name("warp")
    assert data[:4] == __import__("eth_utils").function_signature_to_4byte_selector(
        spec.signature
    )


def test_decode_console_log():
    from eth_abi import encode
    from eth_utils import function_signature_to_4byte_selector

    payload = function_signature_to_4byte_selector("log(string,uint256)") + encode(
        ["string", "uint256"], ["n", 7]
    )
    assert decode_console_log(payload) == "n 7"
