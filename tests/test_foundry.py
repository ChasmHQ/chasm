"""Foundry-test entry point and cheatcode engine.

Compiles the two fixture layouts under tests/ (a standalone test that leans on the bundled
forge-std, and a real project with its own lib/forge-std + remappings) and drives them
through the debug session to prove the cheatcodes actually take effect.
"""

from __future__ import annotations

import os

import pytest

from sevm.cheatcodes import (
    CheatError,
    CheatState,
    apply_cheat,
    decode_console_log,
    encode_cheat_call,
    parse_cheat_arg,
    spec_by_name,
)
from sevm.compile import BUNDLED_FORGE_STD_SRC, compile_foundry_project
from sevm.evaluate import Evaluator, make_eval_hook
from sevm.foundry import (
    compile_test,
    discover_tests,
    make_test_driver,
    resolve_project,
    select_test,
)
from sevm.session import DebugSession, Finished, Paused, StepMode

HERE = os.path.dirname(os.path.abspath(__file__))
SOLO = os.path.join(HERE, "foundry_solo", "AllCheats.t.sol")
PROJECT = os.path.join(HERE, "foundry_project")
PROJECT_TEST = os.path.join(PROJECT, "test", "Token.t.sol")
TIMEOUT = 30.0

_cache: dict[str, object] = {}


def solo_project():
    if "solo" not in _cache:
        root, _ = resolve_project(SOLO, assume_yes=False)
        _cache["solo"] = compile_test(SOLO, root)
    return _cache["solo"]


def token_project():
    if "token" not in _cache:
        _cache["token"] = compile_foundry_project(PROJECT, target_file=PROJECT_TEST)
    return _cache["token"]


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


def test_standalone_uses_bundled_forge_std():
    project = solo_project()
    names = {a.name for a in project.artifacts.values()}
    assert {"AllCheatsTest", "Recorder", "Test", "Vm"} <= names
    assert any("forge-std/=" in r for r in project.remappings)
    # forge-std resolved to the packaged copy, not any on-disk lib.
    test_src = project.sources["lib/forge-std/src/Test.sol"]
    assert os.path.abspath(BUNDLED_FORGE_STD_SRC) in os.path.abspath(test_src.abs_path)


def test_existing_project_lib_wins_over_bundled():
    project = token_project()
    names = {a.name for a in project.artifacts.values()}
    assert {"TokenTest", "Token", "Test", "Vm"} <= names
    # forge-std came from the project's own lib/, not the packaged copy.
    test_src = project.sources["lib/forge-std/src/Test.sol"]
    assert os.path.abspath(PROJECT) in os.path.abspath(test_src.abs_path)
    assert os.path.abspath(BUNDLED_FORGE_STD_SRC) not in os.path.abspath(
        test_src.abs_path
    )


def test_resolve_project_finds_foundry_root():
    root, existing = resolve_project(PROJECT_TEST, assume_yes=False)
    assert existing is True
    assert os.path.abspath(root) == os.path.abspath(PROJECT)


def test_discover_and_select_tests():
    targets = discover_tests(solo_project())
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
def test_cheats_take_effect(fn):
    session, event = run_to_finish(solo_project(), "AllCheatsTest", fn)
    assert isinstance(event, Finished)
    assert event.ok, f"{fn} reverted: {session.exit_error}"


def test_console_log_captured():
    session, event = run_to_finish(solo_project(), "AllCheatsTest", "testEnv")
    assert isinstance(event, Finished) and event.ok
    assert any("env ok at 4242" in line for line in session.cheats.console_lines)


def test_project_prank_reverts_non_owner():
    # A pranked non-owner mint must revert (caught inside the test); the test passes.
    session, event = run_to_finish(
        token_project(), "TokenTest", "testMintPrankRevertsForNonOwner"
    )
    assert isinstance(event, Finished) and event.ok


def test_multi_test_stops_at_each_test():
    from sevm.foundry import make_tests_driver

    project = solo_project()
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


def test_session_opens_at_test_function():
    project = solo_project()
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
