"""Inline assembly (Yul) at the prompt."""

from __future__ import annotations

import pytest


def test_assembly_parses_literals_and_nesting():
    from sevm.assembly import parse
    from sevm.assembly.parser import Call, Literal

    (node,) = parse("sstore(3, add(sload(3), 1))")
    assert isinstance(node, Call) and node.name == "sstore"
    assert node.args[0] == Literal(3, "3")
    inner = node.args[1]
    assert isinstance(inner, Call) and inner.name == "add"
    assert inner.args[0].name == "sload"


def test_assembly_parses_hex_units_bools_and_strings():
    from sevm.assembly import parse

    assert parse("mstore(0x80, 0xdeadBEEF)")[0].args[1].value == 0xDEADBEEF
    assert parse("mstore(0x80, 1 ether)")[0].args[1].value == 10**18
    assert parse("mstore(0x80, 2 gwei)")[0].args[1].value == 2 * 10**9
    assert parse("mstore(0x80, true)")[0].args[1].value == 1
    assert parse("mstore(0x80, 1_000)")[0].args[1].value == 1000
    # Yul pads a string literal on the right, so "hi" lands at the start of the word.
    word = parse('mstore(0x80, "hi")')[0].args[1].value
    assert word.to_bytes(32, "big") == b"hi".ljust(32, b"\x00")


def test_assembly_parses_several_statements():
    from sevm.assembly import parse

    nodes = parse("mstore(0x80, 1); mstore8(0xa0, 0x61); mload(0x80)")
    assert [n.name for n in nodes] == ["mstore", "mstore8", "mload"]
    # A trailing semicolon is not an error.
    assert len(parse("mstore(0x80, 1);")) == 1


def test_assembly_rejects_bad_input_with_a_reason():
    from sevm.assembly import AsmError, parse

    cases = {
        "mstore(1)": "takes 2 argument",
        "mstore(0x80, 1": "expected",
        "bogus(1)": "unknown Yul builtin",
        "jump(0x10)": "Yul has no `jump`",
        "dup1(1)": "Yul has no `dup`",
        "swap2(1)": "Yul has no `swap`",
        "revert(0, 0)": "would end the frame",
        "selfdestruct(0)": "destroy the contract",
        "caller": "is not a value",
        "": "nothing to run",
    }
    for source, expected in cases.items():
        with pytest.raises(AsmError) as excinfo:
            parse(source)
        assert expected in str(excinfo.value), f"{source!r}: {excinfo.value}"


def test_assembly_head_detection_leaves_solidity_alone():
    from sevm.assembly import has_builtin_head, lexes

    assert has_builtin_head("mstore(0x80, 1)")
    assert has_builtin_head("REVERT(0, 0)")  # blocked names claim the line too
    assert not has_builtin_head("deposit()")
    assert not has_builtin_head("balances[msg.sender]")
    # A Solidity expression whose head shares a builtin's name must not be hijacked.
    assert has_builtin_head("keccak256(abi.encode(owner))")
    assert not lexes("keccak256(abi.encode(owner))")
    assert lexes("keccak256(0x80, 0x20)")


def test_assembly_reads_the_live_machine(deposit_debugger):
    dbg = deposit_debugger
    result = dbg.run("add(1, 2)")
    assert result.ok, result.error
    assert "0x3" in result.lines[0]
    assert dbg.run("mload(0x40)").ok
    assert dbg.run("caller()").ok
    # Reads join the value history, so they can be reused like a `p` result.
    assert dbg.commands.history[-1].value == int.from_bytes(dbg.snap.sender, "big")


def test_assembly_mstore_writes_memory_and_the_snapshot_follows(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("mstore(0x80, 0xdeadbeef)").ok
    # Straight from the VM ...
    assert (
        int.from_bytes(dbg.session.inspect("read_memory", 0x80, 32), "big") == 0xDEADBEEF
    )
    # ... and from the snapshot the panes render, without stepping first.
    assert int.from_bytes(dbg.snap.memory[0x80:0xA0], "big") == 0xDEADBEEF


def test_assembly_sstore_writes_storage_through_the_real_opcode(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("sstore(1, 42)").ok
    assert dbg.session.inspect("read_storage", 1) == 42
    # Solidity agrees, so it really is slot 1 of this contract.
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 42
    # Nesting: read-modify-write in one statement.
    assert dbg.run("sstore(1, add(sload(1), 1))").ok
    assert dbg.session.inspect("read_storage", 1) == 43


def test_assembly_leaves_the_operand_stack_and_the_gas_meter_alone(deposit_debugger):
    """Poking at the machine must not change what the transaction goes on to do."""
    dbg = deposit_debugger
    before_stack = [entry.value for entry in dbg.snap.stack]
    before_gas = dbg.session.inspect("frame_info")["gas_remaining"]
    result = dbg.run("mstore(0x80, keccak256(0x0, 0x40)); sload(1); add(2, 3)")
    assert result.ok, result.error
    assert [entry.value for entry in dbg.snap.stack] == before_stack
    assert dbg.session.inspect("frame_info")["gas_remaining"] == before_gas
    # The cost is still reported, so nothing is hidden.
    assert any("gas" in line for line in result.lines)


def test_assembly_accepts_convenience_variables(deposit_debugger):
    dbg = deposit_debugger
    dbg.run("sstore(7, 0xc0ffee)")
    assert dbg.run("mstore(0x80, $storage[7])").ok
    assert int.from_bytes(dbg.session.inspect("read_memory", 0x80, 32), "big") == 0xC0FFEE
    assert dbg.run("add($pc, 0)").ok


def test_assembly_multiple_statements_via_the_asm_verb(deposit_debugger):
    dbg = deposit_debugger
    result = dbg.run("asm mstore(0x80, 1); mstore8(0xa0, 0x61); mload(0x80)")
    assert result.ok, result.error
    assert len(result.lines) == 3
    assert dbg.session.inspect("read_memory", 0xA0, 1) == b"a"
    for alias in ("assembly", "yul"):
        assert dbg.run(f"{alias} mload(0x80)").ok, alias
    assert not dbg.run("asm").ok


def test_assembly_refuses_control_flow_and_terminators(deposit_debugger):
    dbg = deposit_debugger
    for source in ("jump(0x10)", "revert(0, 0)", "stop()", "selfdestruct(0)", "pc()"):
        result = dbg.run(source)
        assert not result.ok, source
        assert result.error and "`" in result.error, source


def test_assembly_does_not_hijack_a_solidity_call(deposit_debugger):
    """`keccak256(abi.encode(x))` shares a builtin's name but is Solidity."""
    result = deposit_debugger.run("keccak256(abi.encode(owner))")
    assert result.ok, result.error
    assert "$1" in result.lines[0]


def test_assembly_failure_does_not_disturb_the_frame(deposit_debugger):
    dbg = deposit_debugger
    before = [entry.value for entry in dbg.snap.stack]
    assert not dbg.run("mstore(1)").ok
    assert not dbg.run("mload(mstore(0, 0))").ok
    assert [entry.value for entry in dbg.snap.stack] == before


def test_assembly_help_is_generated_from_the_builtin_table(deposit_debugger):
    from sevm.assembly import listing

    body = " ".join(deposit_debugger.run("help assembly").lines)
    for builtin in listing():
        assert builtin.name in body, f"{builtin.name} is missing from `help assembly`"
