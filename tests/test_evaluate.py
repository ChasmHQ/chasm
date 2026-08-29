"""Solidity expression evaluation at a breakpoint."""

from __future__ import annotations

import pytest
from harness import (
    line_of,
)

from sevm.compile import compile_standard
from sevm.evaluate import Evaluator, rewrite_msg
from sevm.session import SessionError, StepMode


@pytest.mark.parametrize(
    "expression,expected_type",
    [
        ("owner", "address"),
        ("feeBps", "uint96"),
        ("totalDeposits", "uint256"),
        ("name", "string memory"),
        ("balances[msg.sender]", "uint256"),
        ("accounts[owner].balance", "uint128"),
        ("accounts[owner].frozen", "bool"),
        ("msg.value", "uint256"),
        ("msg.sender", "address"),
        ("msg.data", "bytes memory"),
        ("msg.sig", "bytes4"),
        ("address(this).balance", "uint256"),
        ("keccak256(abi.encode(owner))", "bytes32"),
        ("owner == msg.sender", "bool"),
        ("_fee(msg.value)", "uint256"),
        ("history.length", "uint256"),
        ("block.number", "uint256"),
        ("type(uint256).max", "uint256"),
    ],
)
def test_evaluate_types(deposit_debugger, expression, expected_type):
    result = deposit_debugger.session.inspect("evaluate", expression)
    assert result.type_name == expected_type


def test_evaluate_values_and_units(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.session.inspect("evaluate", "feeBps").value == 25
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 10**18
    assert dbg.session.inspect("evaluate", "msg.value").value == 2 * 10**18
    assert (
        dbg.session.inspect("evaluate", "balances[owner] + 100 ether").value
        == 101 * 10**18
    )
    assert dbg.session.inspect("evaluate", "name").value == "sevm-bank"
    assert dbg.session.inspect("evaluate", "1 ether / 4").value == 25 * 10**16


def test_evaluate_msg_context_matches_the_paused_frame(deposit_debugger, bank):
    _w3, _proj, _contract, _callee, alice = bank
    result = deposit_debugger.session.inspect("evaluate", "msg.sender")
    assert result.value.lower() == alice.address.lower()


def test_evaluate_msg_data_is_the_frames_calldata(forward_debugger):
    """Read directly, msg.data would report the debugger's own eval call."""
    dbg, calldata = forward_debugger
    assert dbg.session.inspect("evaluate", "msg.data").value == calldata
    assert dbg.session.inspect("evaluate", "msg.sig").value == calldata[:4]
    assert dbg.session.inspect("evaluate", "msg.data.length").value == len(calldata)


def test_evaluate_msg_data_slices_like_calldata(forward_debugger):
    """`bytes memory` would compile but refuse the slice, which is the point of it."""
    dbg, _calldata = forward_debugger
    assert (
        dbg.session.inspect("evaluate", "abi.decode(msg.data[36:], (uint256))").value
        == 21
    )


def test_evaluate_msg_data_rides_alongside_a_local(deposit_debugger):
    dbg = deposit_debugger
    dbg.run("b Bank.sol:46")
    dbg.run("c")
    # deposit() takes no arguments, so its calldata is the bare selector.
    result = dbg.session.inspect("evaluate", "msg.data.length + amount")
    assert result.value == 4 + 2 * 10**18


def test_evaluate_leaves_msg_data_in_a_string_literal_alone(deposit_debugger):
    result = deposit_debugger.session.inspect("evaluate", 'bytes("msg.data").length')
    assert result.value == len("msg.data")


def test_rewrite_msg_rewrites_code_and_reports_what_it_bound():
    assert rewrite_msg("msg.data.length") == ("__sevm_msg_data.length", ["data"])
    assert rewrite_msg("msg . sig") == ("__sevm_msg_sig", ["sig"])
    # Order is the order of first appearance, which is the order they are encoded in.
    assert rewrite_msg("msg.sig == bytes4(msg.data)")[1] == ["sig", "data"]
    assert rewrite_msg("msg.sender") == ("msg.sender", [])
    assert rewrite_msg('bytes("msg.data")') == ('bytes("msg.data")', [])
    assert rewrite_msg("mymsg.data") == ("mymsg.data", [])


def test_evaluate_does_not_disturb_the_run(deposit_debugger):
    dbg = deposit_debugger
    before = dbg.session.inspect("evaluate", "totalDeposits").value
    dbg.session.inspect("evaluate", "totalDeposits = 999")  # keep defaults to False
    assert dbg.session.inspect("evaluate", "totalDeposits").value == before


def test_evaluate_with_keep_commits(deposit_debugger):
    dbg = deposit_debugger
    dbg.session.inspect("evaluate", "totalDeposits = 42", keep=True)
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 42


def test_evaluate_errors_are_actionable(deposit_debugger):
    dbg = deposit_debugger
    with pytest.raises(SessionError, match="mapping"):
        dbg.session.inspect("evaluate", "balances")
    with pytest.raises(SessionError, match="Undeclared"):
        dbg.session.inspect("evaluate", "nosuchvar")
    with pytest.raises(SessionError, match="primary expression"):
        dbg.session.inspect("evaluate", "1 +")
    with pytest.raises(SessionError, match="kaboom"):
        dbg.session.inspect("evaluate", "boom()")
    with pytest.raises(SessionError, match="function reference"):
        dbg.session.inspect("evaluate", "_fee")


def test_evaluate_caches_compilations(deposit_debugger):
    dbg = deposit_debugger
    dbg.session.inspect("evaluate", "totalDeposits")
    before = dbg.evaluator.compile_count
    for _ in range(5):
        dbg.session.inspect("evaluate", "totalDeposits")
    assert dbg.evaluator.compile_count == before


def test_evaluate_reads_uncommitted_mid_transaction_state(deposit_debugger):
    """The whole point: `p` must see writes the running transaction has not committed."""
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "totalDeposits += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line)
    dbg.step(StepMode.RUN)
    # balances[] was written on the previous line, inside this uncommitted transaction.
    assert dbg.session.inspect("evaluate", "balances[msg.sender]").value > 0


def test_eval_compiles_only_the_import_closure(token_project):
    """Sources the frame's contract cannot name never reach solc."""
    evaluator = Evaluator(token_project)
    token = token_project.artifacts["src/Token.sol:Token"]
    test = token_project.artifacts["test/Token.t.sol:TokenTest"]

    closure = evaluator._closure(token.source_key)
    assert closure == {token.source_key}
    assert len(closure) < len(token_project.sources)
    assert evaluator._sources_with(token, "owner", "address").keys() == closure

    # The test contract really does need forge-std, so its closure keeps it.
    assert "lib/forge-std/src/Test.sol" in evaluator._closure(test.source_key)


def test_narrowing_does_not_change_the_compiled_code(token_project):
    """The narrowed compile must be byte-for-byte what the whole-project one produced."""
    evaluator = Evaluator(token_project)
    token = token_project.artifacts["src/Token.sol:Token"]
    narrow, _ = evaluator._compiled(token, "balanceOf[owner]")

    wide = compile_standard(
        evaluator._sources_with(token, "balanceOf[owner]", "uint256")
        | {k: s.text for k, s in token_project.sources.items() if k != token.source_key},
        solc_version=token_project.solc_version,
        optimize=False,
        output_selection={"*": {"*": ["evm.deployedBytecode.object"]}},
        remappings=token_project.remappings or None,
    )
    expected = wide["contracts"][token.source_key][token.name]
    assert narrow.hex() == expected["evm"]["deployedBytecode"]["object"]
