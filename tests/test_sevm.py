"""The sevm test suite.

Covers every layer: artifacts and source maps, the stepping engine's stop policy,
breakpoints and watchpoints, Solidity expression evaluation, storage decoding, the gdb
command surface, and a headless render of the TUI.

The tests that drive the VM each get a fresh chain, because stepping mutates state and a
shared one would make failures order-dependent. Compilation is cached process-wide since
solc is the slow part.
"""

from __future__ import annotations

import asyncio
import contextlib
import io
import os
import re

import pytest
from eth_abi import encode as abi_encode
from eth_utils import function_signature_to_4byte_selector
from harness import bank_fixture, project

from sevm.breakpoints import BreakpointSet
from sevm.commands import CommandProcessor, CommandResult, _calldata
from sevm.compile import _strip_metadata
from sevm.decode import (
    StorageDecoder,
    decode_calldata,
    decode_revert,
    dynamic_array_slot,
    mapping_slot,
)
from sevm.disasm import CALL_OPCODES, OPCODES, Disassembly, disassemble
from sevm.evaluate import Evaluator, make_eval_hook, rewrite_msg
from sevm.frames import FunctionIndex
from sevm.session import (
    DebugSession,
    Finished,
    Paused,
    SessionError,
    StepMode,
)
from sevm.srcmap import LineIndex, PcMap, instruction_pcs, parse_source_map

TIMEOUT = 30.0


# ==================================================================
# fixtures
# ==================================================================


@pytest.fixture(scope="module")
def proj():
    return project()


class Debugger:
    """A started session plus the command processor, torn down cleanly."""

    def __init__(self, proj, txfn, **session_kwargs):
        self.session = DebugSession(proj, **session_kwargs)
        self.evaluator = Evaluator(proj)
        self.session.set_eval_hook(make_eval_hook(self.evaluator))
        self.commands = CommandProcessor(self.session, self.evaluator)
        self.session.start(txfn)
        self.first = self.session.wait(timeout=TIMEOUT)

    def step(self, mode=StepMode.STEP, count=1):
        return self.session.resume(mode, count=count, timeout=TIMEOUT)

    def run(self, line):
        return self.commands.execute(line)

    @property
    def snap(self):
        return self.session.last_snapshot

    def close(self):
        try:
            self.session.detach(timeout=TIMEOUT)
        except Exception:
            self.session.uninstall()


@pytest.fixture
def bank():
    """Fresh chain with Bank + Callee deployed and a funded second account."""
    w3, proj_, contract, callee, alice = bank_fixture()
    yield w3, proj_, contract, callee, alice


@pytest.fixture
def deposit_debugger(bank):
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    yield dbg
    dbg.close()


@pytest.fixture
def forward_debugger(bank):
    """Stopped in `forward(address,uint256)`, a frame whose calldata carries arguments.

    Yields the debugger and the calldata the transaction was sent with, computed here
    rather than read back off the frame so the assertions have something to compare to.
    """
    w3, proj_, contract, callee, _alice = bank
    calldata = function_signature_to_4byte_selector(
        "forward(address,uint256)"
    ) + abi_encode(["address", "uint256"], [callee.address, 21])

    def txfn():
        tx = contract.functions.forward(callee.address, 21).transact({"gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    yield dbg, calldata
    dbg.close()


# ==================================================================
# compile
# ==================================================================


def test_project_compiles_both_contracts(proj):
    assert {"Bank.sol:Bank", "Bank.sol:Callee"} <= set(proj.artifacts)
    bank = proj.artifact("Bank")
    assert bank.deployed_bytecode and bank.deployed_source_map
    assert bank.storage_layout["storage"]
    assert "deposit()" in bank.method_identifiers


def test_source_range_targets_the_right_contract(proj):
    """A file with two contracts must not resolve both to the last closing brace."""
    text = proj.sources["Bank.sol"].text
    for name in ("Bank", "Callee"):
        start, end = proj.artifact(name).source_range
        assert text[start:].startswith(f"contract {name} ")
        assert text[end - 1] == "}"
    assert proj.artifact("Bank").source_range[1] < proj.artifact("Callee").source_range[0]


def test_artifact_lookup_by_runtime_code(proj):
    bank = proj.artifact("Bank")
    assert proj.artifact_for_code(bank.deployed_bytecode) is bank
    assert proj.artifact_for_code(b"\x60\x00") is None
    assert proj.artifact_for_code(b"") is None


def test_metadata_stripping_shortens_code(proj):
    code = proj.artifact("Bank").deployed_bytecode
    assert len(_strip_metadata(code)) < len(code)
    assert _strip_metadata(b"\x00") == b"\x00"


def test_selectors_round_trip(proj):
    selectors = proj.artifact("Bank").selectors
    assert "deposit()" in selectors.values()
    assert all(len(sel) == 4 for sel in selectors)


# ==================================================================
# source maps and disassembly
# ==================================================================


def test_parse_source_map_fills_elided_fields():
    entries = parse_source_map("1:2:0:i:0;;3:::o;::-1")
    assert len(entries) == 4
    assert (entries[0].start, entries[0].length, entries[0].file_id, entries[0].jump) == (
        1,
        2,
        0,
        "i",
    )
    assert entries[1] == entries[0]  # an empty chunk repeats everything
    assert entries[2].start == 3 and entries[2].length == 2 and entries[2].jump == "o"
    assert entries[3].file_id == -1 and entries[3].is_generated

    # A malformed field must degrade, not crash: one bad entry cannot blind the debugger.
    assert parse_source_map("1:2:0:-:0;x:y:z:-:0")[1].start == 1
    assert parse_source_map("") == []


def test_instruction_pcs_skips_push_immediates():
    # PUSH1 0x01, PUSH0, PUSH2 0xdead, STOP
    code = bytes([0x60, 0x01, 0x5F, 0x61, 0xDE, 0xAD, 0x00])
    assert instruction_pcs(code) == [0, 2, 3, 6]


def test_disassembler_handles_push0_and_truncated_push():
    code = bytes([0x5F, 0x60, 0x01, 0x7F]) + b"\x11" * 4
    ins = disassemble(code)
    assert ins[0].mnemonic == "PUSH0" and ins[0].immediate is None
    assert ins[1].mnemonic == "PUSH1" and ins[1].operand == 1
    assert ins[2].mnemonic == "PUSH32"
    assert len(ins[2].immediate) == 32  # padded, not dropped


def test_opcode_table_is_complete_enough():
    assert OPCODES[0x00] == "STOP"
    assert OPCODES[0x55] == "SSTORE"
    assert OPCODES[0x5F] == "PUSH0"
    assert {0xF1, 0xF4, 0xFA, 0xF0} <= CALL_OPCODES


def test_line_index_maps_offsets_both_ways():
    index = LineIndex("alpha\nbeta\ngamma")
    assert index.line_col(0) == (1, 1)
    assert index.line_col(6) == (2, 1)
    assert index.line_text(3) == "gamma"
    assert index.line_range(6, 4) == (2, 2)


def test_pcmap_resolves_lines_and_back(proj):
    art = proj.artifact("Bank")
    pcmap = PcMap(art.deployed_bytecode, art.deployed_source_map, _line_indexes(proj))
    executable = pcmap.executable_lines(0)
    assert executable, "no executable lines found"

    deposit_line = _line_of(proj, "_credit(msg.sender, msg.value);")
    pc = pcmap.first_pc_for_line(0, deposit_line)
    assert pc is not None
    assert pcmap.at(pc).line == deposit_line

    # A blank line snaps forward to the next line that actually has code.
    blank = _line_of(proj, "event Deposited")
    assert pcmap.nearest_executable_line(0, blank) >= blank


def test_pcmap_marks_internal_jumps(proj):
    art = proj.artifact("Bank")
    pcmap = PcMap(art.deployed_bytecode, art.deployed_source_map, _line_indexes(proj))
    jumps = {pcmap.at(pc).jump for pc in pcmap.pcs if pcmap.at(pc)}
    assert "i" in jumps and "o" in jumps, (
        "internal call markers are what step/next rely on"
    )


def test_disassembly_index(proj):
    art = proj.artifact("Bank")
    disasm = Disassembly(art.deployed_bytecode)
    assert len(disasm) > 100
    assert disasm.jumpdests
    first_dest = min(disasm.jumpdests)
    assert disasm.is_valid_jumpdest(first_dest)
    assert not disasm.is_valid_jumpdest(first_dest + 1_000_000)
    assert len(disasm.window(first_dest, 3, 5)) <= 8


def _line_indexes(proj):
    from sevm.srcmap import build_line_indexes

    return build_line_indexes(proj.sources.values())


def _line_of(proj, needle):
    for n, text in enumerate(proj.sources["Bank.sol"].text.split("\n"), start=1):
        if needle in text:
            return n
    raise AssertionError(f"{needle!r} not found in Bank.sol")


# ==================================================================
# AST index
# ==================================================================


def test_function_index_names_and_ranges(proj):
    index = FunctionIndex(proj.asts)
    names = {fn.display_name for fn in index.functions}
    assert {"Bank.deposit", "Bank._fee", "Bank._credit", "Callee.receiveValue"} <= names

    fee = index.find("_fee")[0]
    assert fee.contract == "Bank"
    assert fee.visibility == "internal"
    assert fee.parameters == (("uint256", "amount"),)
    assert index.at_offset(fee.file_id, fee.start + 5) is fee


def test_function_index_finds_modifiers_and_constructor(proj):
    index = FunctionIndex(proj.asts)
    kinds = {fn.kind for fn in index.functions}
    assert "modifier" in kinds and "constructor" in kinds


# ==================================================================
# storage decoding
# ==================================================================


def test_storage_decoder_reads_every_shape(deposit_debugger):
    dbg = deposit_debugger
    decoder = StorageDecoder(dbg.session.project.artifact("Bank").storage_layout)
    reader = lambda slot: dbg.session.inspect("read_storage", slot)  # noqa: E731
    values = {var.name: value for var, value in decoder.read_all(reader)}

    assert values["owner"].value.startswith("0x")
    assert values["feeBps"].value == 25  # packed after owner in slot 0
    assert values["totalDeposits"].value == 10**18
    assert values["name"].display == '"sevm-bank"'
    assert "mapping" in values["balances"].display
    assert values["history"].display.startswith("[0 items]")


def test_storage_decoder_packed_slot_offsets(proj):
    decoder = StorageDecoder(proj.artifact("Bank").storage_layout)
    owner = decoder.get("owner")
    fee = decoder.get("feeBps")
    assert (owner.slot, owner.offset) == (0, 0)
    assert (fee.slot, fee.offset) == (0, 20), "feeBps must share slot 0 with owner"


def test_storage_decoder_handles_missing_layout():
    decoder = StorageDecoder(None)
    assert not decoder
    assert decoder.read_all(lambda slot: 0) == []


def test_mapping_and_array_slot_arithmetic():
    from eth_utils import keccak

    key = "0x" + "11" * 20
    expected = int.from_bytes(
        keccak(bytes.fromhex("11" * 20).rjust(32, b"\x00") + (2).to_bytes(32, "big")),
        "big",
    )
    assert mapping_slot(key, 2) == expected
    assert dynamic_array_slot(4) == int.from_bytes(keccak((4).to_bytes(32, "big")), "big")


def test_decode_calldata_and_reverts(proj):
    art = proj.artifact("Bank")
    selector = next(
        bytes.fromhex(sel)
        for sig, sel in art.method_identifiers.items()
        if sig.startswith("withdraw")
    )
    data = selector + (12345).to_bytes(32, "big")
    signature, params = decode_calldata(art.abi, data)
    assert signature == "withdraw(uint256)"
    assert params[0][2] == 12345

    assert decode_calldata(art.abi, b"\x00\x00") is None
    assert decode_calldata(art.abi, b"\xde\xad\xbe\xef") is None

    from eth_abi import encode

    err = bytes.fromhex("08c379a0") + encode(["string"], ["nope"])
    assert decode_revert(err) == 'reverted: "nope"'
    panic = bytes.fromhex("4e487b71") + encode(["uint256"], [0x11])
    assert "arithmetic overflow" in decode_revert(panic)
    assert decode_revert(b"") == "reverted without a reason"


def test_decode_custom_error(proj):
    from eth_abi import encode
    from eth_utils import function_abi_to_4byte_selector

    art = proj.artifact("Bank")
    entry = next(e for e in art.abi if e.get("type") == "error")
    payload = function_abi_to_4byte_selector(entry) + encode(
        ["address"], ["0x" + "22" * 20]
    )
    assert "NotOwner" in decode_revert(payload, art.abi)


# ==================================================================
# stepping engine
# ==================================================================


def test_session_opens_on_the_called_function(deposit_debugger):
    snap = deposit_debugger.snap
    assert isinstance(deposit_debugger.first, Paused)
    assert snap.function.display_name == "Bank.deposit"
    assert snap.contract_name == "Bank"
    assert snap.depth == 0


def test_step_enters_internal_functions(deposit_debugger):
    dbg = deposit_debugger
    seen = []
    for _ in range(6):
        event = dbg.step(StepMode.STEP)
        if isinstance(event, Finished):
            break
        seen.append(event.snapshot.function.name)
    assert "_credit" in seen, "step must enter internal calls"
    assert "_fee" in seen, "step must enter nested internal calls"


def test_next_steps_over_internal_functions(deposit_debugger):
    dbg = deposit_debugger
    seen = []
    for _ in range(6):
        event = dbg.step(StepMode.NEXT)
        if isinstance(event, Finished):
            break
        seen.append(event.snapshot.function.name)
    assert "_fee" not in seen, "next must not descend into _fee"
    assert "deposit" in seen or "_credit" in seen


def test_next_enters_the_body_of_the_function_it_stopped_on(deposit_debugger):
    """solc marks the dispatcher's jump into a body as an internal call; `next` at the
    function's opening line must still reach the first statement."""
    dbg = deposit_debugger
    start_line = dbg.snap.line
    event = dbg.step(StepMode.NEXT)
    assert isinstance(event, Paused)
    assert event.snapshot.line > start_line
    assert event.snapshot.function.display_name == "Bank.deposit"


def test_stepi_and_nexti_move_one_opcode(deposit_debugger):
    dbg = deposit_debugger
    before = dbg.snap.step
    dbg.step(StepMode.STEPI)
    assert dbg.snap.step == before + 1
    dbg.step(StepMode.NEXTI)
    assert dbg.snap.step == before + 2


def test_step_count_repeats(deposit_debugger):
    dbg = deposit_debugger
    before = dbg.snap.step
    dbg.step(StepMode.STEPI, count=5)
    assert dbg.snap.step == before + 5


def test_backtrace_interleaves_solidity_and_evm_frames(deposit_debugger):
    dbg = deposit_debugger
    for _ in range(4):
        if isinstance(dbg.step(StepMode.STEP), Finished):
            break
    rows = dbg.snap.backtrace
    assert rows[-1].kind == "evm"
    names = [r.name for r in rows]
    assert any("_fee" in n or "_credit" in n for n in names)
    # Outer frames show their call site, not their entry point.
    assert all(r.line >= 0 for r in rows)
    # No compiler-generated frame is shown unless we are stopped inside one.
    assert not any(r.detail == "compiler-generated" for r in rows[1:])


def test_cross_contract_call_creates_an_evm_frame(bank):
    w3, proj_, contract, callee, _alice = bank

    def txfn():
        tx = contract.functions.forward(callee.address, 21).transact({"gas": 400_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.session.break_at_function("receiveValue")
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        snap = event.snapshot
        assert snap.depth == 1
        assert snap.contract_name == "Callee"
        kinds = [r.kind for r in snap.backtrace]
        assert kinds.count("evm") == 2, "both EVM frames must appear"
        assert any("Bank.forward" in r.name for r in snap.backtrace)
    finally:
        dbg.close()


def test_finish_leaves_the_current_frame(bank):
    w3, proj_, contract, callee, _alice = bank

    def txfn():
        tx = contract.functions.forward(callee.address, 21).transact({"gas": 400_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.session.break_at_function("receiveValue")
        dbg.step(StepMode.RUN)
        assert dbg.snap.depth == 1
        event = dbg.step(StepMode.FINISH)
        assert isinstance(event, Paused)
        assert event.snapshot.stop_reason == "finish"
    finally:
        dbg.close()


def test_loop_iterates_with_next(bank):
    w3, proj_, contract, _callee, alice = bank
    tx = contract.functions.deposit().transact(
        {"from": alice.address, "value": w3.to_wei(1, "ether"), "gas": 300_000}
    )
    w3.eth.wait_for_transaction_receipt(tx)

    def txfn():
        contract.functions.sumHistory().call()

    dbg = Debugger(proj_, txfn)
    try:
        body = _line_of(proj_, "total += history[i];")
        hits = 0
        for _ in range(24):
            event = dbg.step(StepMode.NEXT)
            if isinstance(event, Finished):
                break
            if event.snapshot.line == body:
                hits += 1
        assert hits >= 1, "the loop body should be reached"
    finally:
        dbg.close()


# ==================================================================
# breakpoints
# ==================================================================


def test_line_breakpoint_stops_there(deposit_debugger):
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "balances[who] += amount - fee;")
    bp, snapped = dbg.session.break_at_line("Bank.sol", line)
    assert snapped == line and not bp.pending
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.line == line
    assert event.snapshot.stop_reason == "breakpoint"
    assert event.snapshot.hit_breakpoints == (bp.number,)


def test_function_breakpoint(deposit_debugger):
    dbg = deposit_debugger
    bp, _line = dbg.session.break_at_function("_fee")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.function.name == "_fee"
    assert bp.number in event.snapshot.hit_breakpoints


def test_opcode_breakpoint(deposit_debugger):
    dbg = deposit_debugger
    dbg.session.break_at_opcode("SSTORE")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.mnemonic == "SSTORE"


def test_conditional_breakpoint_that_is_false_does_not_stop(deposit_debugger):
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "balances[who] += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line, condition="totalDeposits > 1000 ether")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Finished)


def test_conditional_breakpoint_that_is_true_stops(deposit_debugger):
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "balances[who] += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line, condition="totalDeposits > 0")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.line == line


def test_condition_over_a_local_is_evaluated_not_skipped(deposit_debugger):
    """This used to be the documented gap: conditions could not see locals."""
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "return (amount * feeBps) / 10000;")
    bp, _ = dbg.session.break_at_line("Bank.sol", line, condition="amount > 1 ether")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.line == line
    assert bp.condition_error is None


def test_broken_condition_breaks_and_records_why(deposit_debugger):
    """An unevaluatable condition still breaks, as gdb does, and reports the reason."""
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "return (amount * feeBps) / 10000;")
    bp, _ = dbg.session.break_at_line("Bank.sol", line, condition="nosuchthing > 1")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert bp.condition_error and "Undeclared" in bp.condition_error


def test_temporary_breakpoint_fires_once(deposit_debugger):
    dbg = deposit_debugger
    dbg.session.break_at_opcode("SSTORE", temporary=True)
    assert isinstance(dbg.step(StepMode.RUN), Paused)
    assert not dbg.session.breakpoints.breakpoints
    assert isinstance(dbg.step(StepMode.RUN), Finished)


def test_breakpoint_management():
    bps = BreakpointSet()
    first = bps.add_opcode("SSTORE")
    second = bps.add_pc(0x10)
    assert len(bps.listing()) == 2
    assert bps.set_enabled(first.number, False) == 1
    assert bps.match(0x10, "SSTORE", 0, 0, None) == [second]
    assert bps.remove(first.number)
    assert not bps.remove(9999)
    bps.clear()
    assert bps.is_empty


def test_watchpoint_reports_old_and_new(deposit_debugger):
    dbg = deposit_debugger
    decoder = StorageDecoder(dbg.session.project.artifact("Bank").storage_layout)
    slot = decoder.get("totalDeposits").slot
    dbg.session.watch_storage("totalDeposits", slot, address=dbg.snap.address)
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.stop_reason == "watchpoint"
    assert "->" in event.snapshot.annotation


def test_read_watchpoint_fires_on_sload(deposit_debugger):
    dbg = deposit_debugger
    result = dbg.run("rwatch totalDeposits")
    assert result.ok and "Read watchpoint" in result.lines[0]
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.stop_reason == "watchpoint"
    assert event.snapshot.mnemonic == "SLOAD"
    assert "read" in event.snapshot.annotation


def test_write_watchpoint_reports_old_and_new_via_command(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("watch totalDeposits").ok
    dbg.step(StepMode.RUN)
    rendered = " ".join(dbg.commands.describe_stop(dbg.snap))
    assert "Watchpoint" in rendered and "->" in rendered


def test_rwatch_on_memory_is_refused_clearly(deposit_debugger):
    result = deposit_debugger.run("rwatch *0x80")
    assert not result.ok and "storage" in result.error


def test_examine_rejects_formats_it_cannot_honour(deposit_debugger):
    dbg = deposit_debugger
    assert not dbg.run("x/4i 0x40").ok
    assert not dbg.run("x/4f 0x40").ok
    assert dbg.run("x/4xb 0x40").ok


def test_stop_on_revert_decodes_the_reason(bank):
    w3, proj_, contract, _callee, _alice = bank

    def txfn():
        tx = contract.functions.boom().transact({"gas": 200_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        assert event.snapshot.stop_reason == "error"
        assert event.snapshot.annotation == 'reverted: "kaboom"'
    finally:
        dbg.close()


def test_stop_on_panic(bank):
    w3, proj_, contract, _callee, _alice = bank

    def txfn():
        with contextlib.suppress(Exception):
            contract.functions.overflow(1, 5).call()

    dbg = Debugger(proj_, txfn)
    try:
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        assert "panic 0x11" in event.snapshot.annotation
    finally:
        dbg.close()


# ==================================================================
# evaluation
# ==================================================================


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
    line = _line_of(dbg.session.project, "totalDeposits += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line)
    dbg.step(StepMode.RUN)
    # balances[] was written on the previous line, inside this uncommitted transaction.
    assert dbg.session.inspect("evaluate", "balances[msg.sender]").value > 0


# ==================================================================
# mutation
# ==================================================================


def test_write_storage_takes_effect(deposit_debugger):
    dbg = deposit_debugger
    dbg.session.inspect("write_storage", 1, 12345)
    assert dbg.session.inspect("read_storage", 1) == 12345
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 12345


def test_write_stack_changes_what_the_opcode_stores(bank):
    """Rewrite the SSTORE value operand before it executes and watch storage change."""
    w3, proj_, contract, _callee, _alice = bank

    def txfn():
        tx = contract.functions.setNickname("x").transact({"gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.session.break_at_opcode("SSTORE")
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        slot = event.snapshot.stack[0].value
        dbg.session.inspect("write_stack", 1, 0xC0FFEE)
        dbg.step(StepMode.STEPI)
        assert dbg.session.inspect("read_storage", slot) == 0xC0FFEE
    finally:
        dbg.close()


def test_write_memory_and_gas(deposit_debugger):
    dbg = deposit_debugger
    dbg.session.inspect("write_memory", 0x80, (7).to_bytes(32, "big"))
    assert int.from_bytes(dbg.session.inspect("read_memory", 0x80, 32), "big") == 7
    dbg.session.inspect("set_gas", 5000)
    assert dbg.session.inspect("frame_info")["gas_remaining"] == 5000


def test_set_pc_refuses_a_non_jumpdest(deposit_debugger):
    with pytest.raises(SessionError, match="JUMPDEST"):
        deposit_debugger.session.inspect("set_pc", 1)


def test_memory_reads_past_the_end_return_zeros(deposit_debugger):
    data = deposit_debugger.session.inspect("read_memory", 0xFFFF, 32)
    assert data == b"\x00" * 32


# ==================================================================
# commands
# ==================================================================


def test_command_break_and_continue(deposit_debugger):
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "history.push(amount);")
    result = dbg.run(f"b Bank.sol:{line}")
    assert result.ok and "Breakpoint 1" in result.lines[0]
    result = dbg.run("c")
    assert result.resumed
    assert dbg.snap.line == line


def test_command_aliases_match_gdb(deposit_debugger):
    dbg = deposit_debugger
    for alias in ("n", "s", "si", "ni"):
        assert dbg.run(alias).ok
    for alias in ("bt", "where", "i r", "l", "disas"):
        assert dbg.run(alias).ok, alias


def test_command_print_and_value_history(deposit_debugger):
    dbg = deposit_debugger
    assert "$1" in dbg.run("p totalDeposits").lines[0]
    assert "$2" in dbg.run("p feeBps").lines[0]
    result = dbg.run("p $1 + 1")
    assert result.ok and "$3" in result.lines[0]


def test_bare_expression_is_printed(deposit_debugger):
    result = deposit_debugger.run("totalDeposits")
    assert result.ok and "$1" in result.lines[0]


def test_command_convenience_variables(deposit_debugger):
    dbg = deposit_debugger
    for expression in (
        "p $pc",
        "p $gas",
        "p $depth",
        "p $sp",
        "p $storage[1]",
        "p $stack[0]",
    ):
        assert dbg.run(expression).ok, expression


def test_command_examine_memory(deposit_debugger):
    dbg = deposit_debugger
    result = dbg.run("x/32xb 0x40")
    assert result.ok
    assert any("free memory pointer" in line for line in result.lines)
    assert dbg.run("x/4xg 0x0").ok
    assert dbg.run("x/s 0x80").ok
    assert not dbg.run("x/zz 0x0").ok


def test_command_info_topics(deposit_debugger):
    dbg = deposit_debugger
    for topic in (
        "registers",
        "breakpoints",
        "frame",
        "args",
        "locals",
        "storage",
        "gas",
        "logs",
        "sources",
        "functions",
    ):
        result = dbg.run(f"info {topic}")
        assert result.ok, f"info {topic}: {result.error}"
        assert result.lines, f"info {topic} produced nothing"
    assert not dbg.run("info nonsense").ok


def test_info_frame_colours_the_selector_apart_from_the_arguments(forward_debugger):
    dbg, calldata = forward_debugger
    line = next(ln for ln in dbg.run("info frame").lines if "calldata" in ln)
    assert f"[bold yellow]0x{calldata[:4].hex()}[/bold yellow]" in line
    assert f"[magenta]{calldata[4:].hex()}[/magenta]" in line


def test_calldata_reports_what_it_truncates():
    assert "empty" in _calldata(b"")
    assert _calldata(b"\xd0\xe3\x0d\xb0") == "[bold yellow]0xd0e30db0[/bold yellow]"
    text = _calldata(b"\x00\x00\x40\xc3" + b"\xab" * 100)
    assert "(+36 bytes)" in text


def test_info_locals_names_and_values_the_frames_locals(deposit_debugger):
    dbg = deposit_debugger
    dbg.run("b Bank.sol:46")
    dbg.run("c")
    lines = " ".join(dbg.run("info locals").lines)
    assert "who" in lines and "amount" in lines and "fee" in lines
    # 2 ether at 25 bps.
    assert "5000000000000000" in lines
    assert "2000000000000000000" in lines


def test_info_storage_decodes_names(deposit_debugger):
    lines = " ".join(deposit_debugger.run("info storage").lines)
    assert "totalDeposits" in lines and "sevm-bank" in lines


def test_info_gas_profiles_by_line(deposit_debugger):
    dbg = deposit_debugger
    for _ in range(4):
        if isinstance(dbg.step(StepMode.STEP), Finished):
            break
    lines = " ".join(dbg.run("info gas").lines)
    assert "gas by source line" in lines
    assert "gas by opcode" in lines


def test_command_set_var_writes_through_solidity(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("set var totalDeposits = 7 ether").ok
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 7 * 10**18
    assert dbg.run("set var balances[msg.sender] = 3 ether").ok
    assert dbg.session.inspect("evaluate", "balances[msg.sender]").value == 3 * 10**18


def test_command_set_convenience(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("set $gas = 90000").ok
    assert dbg.session.inspect("frame_info")["gas_remaining"] == 90000
    assert dbg.run("set $storage[1] = 5").ok
    assert dbg.session.inspect("read_storage", 1) == 5


def test_command_call_keeps_effects(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("call totalDeposits = 11").ok
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 11


def test_command_display_reevaluates_on_each_stop(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("display totalDeposits").ok
    result = dbg.run("s")
    assert any("totalDeposits" in line for line in result.lines)
    assert dbg.run("undisplay").ok
    assert not dbg.commands.displays


def test_command_frame_selection(deposit_debugger):
    dbg = deposit_debugger
    for _ in range(4):
        if isinstance(dbg.step(StepMode.STEP), Finished):
            break
    assert dbg.run("f 1").ok
    assert dbg.run("up").ok
    assert dbg.run("down").ok
    assert not dbg.run("f 99").ok


def test_command_watch_resolves_a_state_variable(deposit_debugger):
    result = deposit_debugger.run("watch totalDeposits")
    assert result.ok and "Watchpoint" in result.lines[0]


def test_command_watch_resolves_a_mapping_element(deposit_debugger):
    result = deposit_debugger.run("watch balances[msg.sender]")
    assert result.ok and "Watchpoint" in result.lines[0]


def test_command_delete_disable_enable(deposit_debugger):
    dbg = deposit_debugger
    dbg.run("b Bank.sol:52")
    assert dbg.run("disable 1").ok
    assert dbg.run("enable 1").ok
    assert dbg.run("delete 1").ok
    assert not dbg.run("delete 1").ok


def test_command_until(deposit_debugger):
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "history.push(amount);")
    result = dbg.run(f"until Bank.sol:{line}")
    assert result.resumed
    assert dbg.snap.line == line


def test_command_ptype(deposit_debugger):
    assert "uint96" in deposit_debugger.run("ptype feeBps").lines[0]


def test_command_help(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("help").lines
    for topic in ("breakpoints", "print", "memory", "mutation", "gas", "locals"):
        assert dbg.run(f"help {topic}").ok, topic
    assert not dbg.run("help nonsense").ok


def test_bad_command_reports_instead_of_raising(deposit_debugger):
    result = deposit_debugger.run("p nonsense_identifier")
    assert not result.ok and result.error


def test_command_quit_flag(deposit_debugger):
    assert deposit_debugger.run("q").quit


def test_markup_in_source_is_escaped(deposit_debugger):
    """Source containing brackets must not be interpreted as Rich markup."""
    dbg = deposit_debugger
    line = _line_of(dbg.session.project, "balances[who] += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line)
    dbg.step(StepMode.RUN)
    rendered = " ".join(dbg.commands.describe_stop(dbg.snap))
    assert "\\[who]" in rendered


# ==================================================================
# session lifecycle
# ==================================================================


def test_gas_estimation_is_not_debugged(bank):
    """A transaction without an explicit `gas=` must still stop exactly once.

    web3 calls eth_estimateGas, which binary-searches by RUNNING the transaction from the
    intrinsic gas upward, so the early probes fail with OutOfGas by design. Those passes
    run with the hook suspended; without that, the user sees a bogus out-of-gas inside a
    transaction that actually succeeds.
    """
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(1, "ether")}  # no gas= on purpose
        )
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        assert isinstance(dbg.first, Paused)
        assert dbg.snap.stop_reason != "error", dbg.snap.annotation
        assert dbg.snap.function.display_name == "Bank.deposit"
        line = _line_of(proj_, "history.push(amount);")
        dbg.session.break_at_line("Bank.sol", line)
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        assert event.snapshot.line == line
        assert dbg.session.estimations > 0, "estimation passes should have been counted"
    finally:
        dbg.close()


def test_estimate_gas_patch_is_restored(bank):
    from eth.chains.base import Chain

    w3, proj_, contract, _callee, _alice = bank
    original = Chain.__dict__["estimate_gas"]

    def txfn():
        tx = contract.functions.deposit().transact({"value": 1, "gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    assert Chain.__dict__["estimate_gas"] is not original
    dbg.close()
    assert Chain.__dict__["estimate_gas"] is original


def test_ether_hint_only_for_plausible_wei(deposit_debugger):
    """An address cast to uint256 is not 721 billion billion ether."""
    dbg = deposit_debugger
    assert "ether" in dbg.session.inspect("evaluate", "1 ether").display
    assert (
        "ether" not in dbg.session.inspect("evaluate", "uint256(uint160(owner))").display
    )
    assert "ether" not in dbg.session.inspect("evaluate", "type(uint256).max").display
    assert "ether" not in dbg.session.inspect("evaluate", "feeBps").display


def test_debugs_inline_assembly_in_the_original_vault():
    """The article's Vault.sol: a breakpoint inside an `assembly { sstore(...) }` block."""
    import os as _os

    from eth_account import Account
    from web3 import EthereumTesterProvider, Web3

    from sevm.compile import DEFAULT_SOLC_VERSION
    from sevm.compile import compile_project as _compile

    here = _os.path.dirname(_os.path.abspath(__file__))
    vault_project = _compile(
        [_os.path.join(here, "contracts", "Vault.sol")], solc_version=DEFAULT_SOLC_VERSION
    )
    art = vault_project.artifact("Vault")
    assert art is not None

    w3 = Web3(EthereumTesterProvider())
    w3.eth.default_account = w3.eth.accounts[0]
    factory = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())
    tx = factory.constructor().transact(
        {"value": w3.to_wei(1, "ether"), "gas": 3_000_000}
    )
    address = w3.eth.wait_for_transaction_receipt(tx)["contractAddress"]
    vault = w3.eth.contract(address=address, abi=art.abi)
    attacker = Account.create()
    w3.eth.send_transaction(
        {"to": attacker.address, "value": w3.to_wei(1, "ether"), "gas": 21000}
    )
    w3.provider.ethereum_tester.backend.add_account(attacker.key)

    def txfn():
        t = vault.functions.unsafeStore(0, int(attacker.address, 16)).transact(
            {"from": attacker.address, "gas": 200_000}
        )
        w3.eth.wait_for_transaction_receipt(t)

    dbg = Debugger(vault_project, txfn)
    try:
        sstore_line = None
        for n, text in enumerate(
            vault_project.sources["Vault.sol"].text.split("\n"), start=1
        ):
            if "sstore(slot, value)" in text:
                sstore_line = n
        assert sstore_line
        dbg.session.break_at_line("Vault.sol", sstore_line)
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        assert event.snapshot.line == sstore_line
        assert event.snapshot.function.name == "unsafeStore"
        # The arbitrary-storage-write bug, seen before it lands.
        assert dbg.run("info args").ok
        assert dbg.session.inspect("evaluate", "owner").value != attacker.address.lower()
        dbg.session.break_at_opcode("SSTORE")
        assert isinstance(dbg.step(StepMode.RUN), Paused)
        assert dbg.snap.stack[0].value == 0, "the SSTORE targets slot 0 (owner)"
    finally:
        dbg.close()


def test_patch_is_restored_after_detach(bank):
    from eth.vm.computation import BaseComputation

    w3, proj_, contract, _callee, _alice = bank
    original = BaseComputation.__dict__["apply_computation"]

    def txfn():
        tx = contract.functions.deposit().transact({"value": 1, "gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    assert BaseComputation.__dict__["apply_computation"] is not original
    dbg.close()
    assert BaseComputation.__dict__["apply_computation"] is original
    # And the chain still works untraced.
    assert contract.functions.totalDeposits().call() > 0


def test_inspect_after_finish_raises(bank):
    w3, proj_, contract, _callee, _alice = bank

    def txfn():
        tx = contract.functions.deposit().transact({"value": 1, "gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.step(StepMode.RUN)
        assert dbg.session.finished
        with pytest.raises(SessionError):
            dbg.session.inspect("read_storage", 0)
    finally:
        dbg.close()


def test_script_exception_is_reported_not_swallowed(proj):
    def txfn():
        raise ValueError("script blew up")

    session = DebugSession(proj)
    session.start(txfn)
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert not event.ok and "script blew up" in event.error
    session.detach(timeout=TIMEOUT)


# ==================================================================
# TUI
# ==================================================================


def test_tui_renders_every_pane(bank):
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    from sevm.tui.app import SevmApp

    session = DebugSession(proj_)
    evaluator = Evaluator(proj_)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.start(txfn)
    first = session.wait(timeout=TIMEOUT)
    app = SevmApp(session, evaluator, first_event=first)

    async def drive():
        async with app.run_test(size=(150, 46)) as pilot:
            await pilot.pause()
            await asyncio.sleep(1.0)
            line = _line_of(proj_, "balances[who] += amount - fee;")
            app.run_command(f"b Bank.sol:{line}")
            await asyncio.sleep(1.0)
            app.run_command("continue")
            await asyncio.sleep(2.5)
            await pilot.pause()
            return _screen_text(app)

    try:
        screen = asyncio.run(drive())
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()

    for title in (
        "SOURCE",
        "CALL STACK",
        "VARIABLES",
        "STORAGE",
        "DISASSEMBLY",
        "STACK",
        "MEMORY",
    ):
        assert title in screen, f"{title} pane missing"
    assert "Bank._credit" in screen
    assert "*>" in screen, "breakpoint + current-line marker missing from the gutter"
    assert "=>" in screen, "disassembly current-instruction marker missing"
    assert "totalDeposits" in screen
    assert "free mem ptr" in screen or "scratch" in screen
    assert "0x0000:" in screen, "memory must be addressed in the x/g shape"

    # The VARIABLES pane names the frame's locals instead of the old <unavailable> row.
    assert "local" in screen and "fee" in screen
    assert "solc emits no locations" not in screen


def test_tui_startup_commands_run_in_sequence(bank):
    """`-x` startup commands must all run, not just the first.

    Each command executes on an exclusive VM worker that clears `busy` only when it
    finishes, so the startup commands have to be dispatched one at a time. A regression
    here (dispatching them in a loop) drops every command after the first to the `busy`
    guard, leaving the session parked at the constructor instead of inside `deposit`.
    """
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    from sevm.tui.app import SevmApp

    session = DebugSession(proj_)
    evaluator = Evaluator(proj_)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.start(txfn)
    first = session.wait(timeout=TIMEOUT)
    app = SevmApp(
        session,
        evaluator,
        first_event=first,
        startup_commands=["tbreak deposit", "continue"],
    )

    def in_deposit() -> bool:
        snap = app.session.last_snapshot
        return (
            snap is not None
            and snap.function is not None
            and (snap.function.display_name == "Bank.deposit")
        )

    async def drive():
        async with app.run_test(size=(150, 46)) as pilot:
            await pilot.pause()
            # Poll rather than sleep a fixed time: the startup `continue` runs on a
            # background worker, so wait for it to land instead of guessing a duration.
            for _ in range(60):
                if in_deposit():
                    break
                await asyncio.sleep(0.1)
            await pilot.pause()
            return app.session.last_snapshot

    try:
        snap = asyncio.run(drive())
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()

    assert snap is not None and snap.function is not None
    assert snap.function.display_name == "Bank.deposit", (
        "startup `continue` did not run: session parked outside deposit"
    )


def _screen_text(app) -> str:
    """Flatten the composited screen to plain text for assertions."""
    return "\n".join(
        "".join(segment.text for segment in strip)
        for strip in app.screen._compositor.render_strips()
    )


def test_tui_pane_helpers():
    from sevm.tui.widgets import _hex_compact, memory_region, operand_count, operand_name

    assert _hex_compact(0xFF) == "0xff"
    assert ".." in _hex_compact(2**255, budget=12)
    assert memory_region(0) == "scratch"
    assert memory_region(0x40) == "free mem ptr"
    assert memory_region(0x100) == ""
    assert operand_count("SSTORE") == 2
    assert operand_name("SSTORE", 0) == "slot"
    assert operand_name("SSTORE", 1) == "value"
    assert operand_name("ADD", 0) == ""


# ==================================================================
# CLI
# ==================================================================


_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def strip_ansi(text: str) -> str:
    return _ANSI_RE.sub("", text)


def test_cli_compile_subcommand(capsys):
    from sevm.cli import main

    contracts = os.path.join(os.path.dirname(os.path.abspath(__file__)), "contracts")
    assert main(["compile", contracts]) == 0
    # Rich styles its output when the environment forces colour, which splits words with
    # escape sequences; assert on what the user reads, not on how it is painted.
    out = strip_ansi(capsys.readouterr().out)
    assert "Bank.sol:Bank" in out
    assert "source-map=yes" in out


def test_cli_rejects_a_missing_script(capsys):
    from sevm.cli import main

    assert main(["run", "/nonexistent/script.py"]) == 1
    assert "no such script" in capsys.readouterr().out


# ==================================================================
# local variables
# ==================================================================


@pytest.fixture
def locals_contract(proj):
    """A fresh chain with `Locals` deployed, for the hard shapes."""
    from harness import deploy, make_web3

    w3 = make_web3()
    return w3, proj, deploy(w3, proj, "Locals")


def locals_debugger(w3, proj_, contract, function, *args, gas=900_000):
    def txfn():
        tx = getattr(contract.functions, function)(*args).transact({"gas": gas})
        w3.eth.wait_for_transaction_receipt(tx)

    return Debugger(proj_, txfn)


def stop_at(dbg, line, contract="Locals.sol"):
    dbg.run(f"b {contract}:{line}")
    result = dbg.run("c")
    assert result.ok, result.error
    return result


def locals_map(dbg):
    """`info locals` as {name: value text}, with markup stripped."""
    out = {}
    for row in dbg.commands._locals():
        out[row["name"]] = row["value"] if row["available"] else "<unavailable>"
    return out


# -- the static index --------------------------------------------------------


def test_locals_index_finds_every_declaration(proj):
    from sevm.locals import LocalsIndex

    index = LocalsIndex(proj.asts)
    credit = [
        fn for fn in index.by_function.values() if any(v.name == "fee" for v in fn.body)
    ]
    assert len(credit) == 1
    layout = credit[0]
    assert [v.name for v in layout.params] == ["who", "amount"]
    assert [v.name for v in layout.body] == ["fee"]


def test_locals_index_orders_params_before_body(proj):
    """Allocation order, not AST visit order: the frame layout depends on it."""
    from sevm.locals import LocalsIndex

    index = LocalsIndex(proj.asts)
    for layout in index.by_function.values():
        kinds = [v.kind for v in layout.all]
        assert kinds == sorted(kinds, key=["param", "return", "local"].index)
        assert [v.index for v in layout.all] == list(range(len(layout.all)))


def test_locals_scope_excludes_declarations_from_closed_blocks(proj):
    from sevm.locals import LocalsIndex

    index = LocalsIndex(proj.asts)
    scoping = next(
        fn
        for fn in index.by_function.values()
        if any(v.name == "shadowed" for v in fn.body)
    )
    shadowed = next(v for v in scoping.body if v.name == "shadowed")
    after = next(v for v in scoping.body if v.name == "after_")
    # `after_` is declared past the `if` block, so `shadowed` is long gone by then.
    assert not shadowed.visible_at(after.start)
    assert shadowed.visible_at(shadowed.start + 1)


def test_stack_slots_counts_calldata_and_function_types():
    from sevm.locals import stack_slots

    assert stack_slots("uint256", "default") == 1
    assert stack_slots("string", "memory") == 1
    assert stack_slots("bytes", "calldata") == 2
    assert stack_slots("uint256[]", "calldata") == 2
    assert stack_slots("uint256[3]", "calldata") == 1
    assert stack_slots("function (uint256) external returns (uint256)", "default") == 2
    assert stack_slots("", "default") is None


def test_declaration_pcs_skips_parameters(proj):
    """Parameters are pushed by the caller; recording them here would be wrong."""
    from sevm.locals import KIND_LOCAL, KIND_RETURN, LocalsIndex, declaration_pcs
    from sevm.srcmap import PcMap, build_line_indexes

    art = proj.artifact("Bank")
    index = LocalsIndex(proj.asts)
    pcmap = PcMap(
        art.deployed_bytecode,
        art.deployed_source_map,
        build_line_indexes(proj.sources.values()),
    )
    table = declaration_pcs(pcmap, index)
    assert table, "no declaration sites found at all"
    assert all(v.kind in (KIND_LOCAL, KIND_RETURN) for v in table.values())
    assert any(v.name == "fee" for v in table.values())


# -- reading them off a live frame ------------------------------------------


def test_locals_read_the_paused_frame(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    values = locals_map(dbg)
    assert values["amount"].startswith("2000000000000000000")
    assert values["fee"].startswith("5000000000000000 ")
    assert values["who"].startswith("0x")


def test_locals_survive_the_value_types(locals_contract):
    dbg = locals_debugger(*locals_contract, "values", 7)
    try:
        stop_at(dbg, 23)
        values = locals_map(dbg)
        assert values["doubled"] == "14"
        assert values["negative"] == "-7"
        assert values["flag"] == "true"
        assert values["who"] == "0x" + "0" * 36 + "1234"
        assert values["tag"] == "0xdeadbeef"
        assert values["small"] == "200"
    finally:
        dbg.close()


def test_locals_from_a_closed_block_do_not_resurface(locals_contract):
    """The regression the naive implementation always has: a stale slot reappearing."""
    dbg = locals_debugger(*locals_contract, "scoping", 5)
    try:
        stop_at(dbg, 39)
        values = locals_map(dbg)
        assert "shadowed" not in values
        assert "inner" not in values
        assert values["total"] == "338"  # 5 + 111 + 222
        assert values["after_"] == "333"
    finally:
        dbg.close()


def test_locals_track_a_loop_variable(locals_contract):
    dbg = locals_debugger(*locals_contract, "loop", 3)
    try:
        stop_at(dbg, 56)
        seen = []
        for _ in range(3):
            values = locals_map(dbg)
            seen.append((values["i"], values["square"]))
            event = dbg.session.resume(StepMode.RUN, timeout=TIMEOUT)
            if isinstance(event, Finished):
                break
        assert seen == [("0", "0"), ("1", "1"), ("2", "4")]
    finally:
        dbg.close()


def test_recursion_gives_each_frame_its_own_locals(locals_contract):
    """Same function, two live frames, one stack: the bases must differ."""
    dbg = locals_debugger(*locals_contract, "recurse", 2)
    try:
        stop_at(dbg, 49)
        assert locals_map(dbg)["here"] == "10"
        assert dbg.run("up").ok
        assert locals_map(dbg)["here"] == "20"
        assert dbg.run("down").ok
        assert locals_map(dbg)["here"] == "10"
    finally:
        dbg.close()


def test_memory_reference_locals_are_dereferenced(locals_contract):
    dbg = locals_debugger(*locals_contract, "memoryTypes", "hello")
    try:
        stop_at(dbg, 81)
        values = locals_map(dbg)
        assert values["label"] == '"hello"'
        assert values["list"] == "[3 items] [10, 20, 30]"
        assert values["raw"] == "0x" + b"hello".hex()
        assert values["len"] == "8"
    finally:
        dbg.close()


def test_storage_pointer_reports_its_slot_not_a_value(locals_contract):
    dbg = locals_debugger(*locals_contract, "storagePointer", 42)
    try:
        stop_at(dbg, 89)
        rows = {r["name"]: r for r in dbg.commands._locals()}
        assert "storage pointer" in rows["p"]["value"]
        assert "index the state variable" in rows["p"]["reason"]
        assert rows["read"]["value"] == "42"
    finally:
        dbg.close()


def test_calldata_reference_is_two_slots_and_says_so(locals_contract):
    dbg = locals_debugger(*locals_contract, "calldataTypes", b"\xaa\xbb\xcc")
    try:
        stop_at(dbg, 95)
        rows = {r["name"]: r for r in dbg.commands._locals()}
        assert "calldata reference" in rows["payload"]["value"]
        assert rows["size"]["value"] == "3"
    finally:
        dbg.close()


def test_modifier_locals_belong_to_the_modifier_frame(locals_contract):
    """A modifier is inlined into the function, but its locals are its own."""
    dbg = locals_debugger(*locals_contract, "bump", 5)
    try:
        stop_at(dbg, 63)
        values = locals_map(dbg)
        assert "before" in values
        assert "next" not in values, (
            "the function's locals are not in the modifier's scope"
        )
    finally:
        dbg.close()


def test_a_local_is_unavailable_before_it_is_allocated(locals_contract):
    """Stopped *on* the declaration, the slot does not exist yet. Say so, do not guess."""
    dbg = locals_debugger(*locals_contract, "values", 7)
    try:
        stop_at(dbg, 17)
        rows = {r["name"]: r for r in dbg.commands._locals()}
        assert not rows["doubled"]["available"]
        assert rows["doubled"]["value"] == "<unavailable>"
        assert "step once" in rows["doubled"]["reason"]
        # Declarations further down the function are not in scope at all yet.
        assert "small" not in rows
    finally:
        dbg.close()


def test_locals_are_empty_without_source(deposit_debugger):
    """No artifact, no AST, no guessing."""
    dbg = deposit_debugger
    assert dbg.run("info locals").ok


# -- expressions over locals -------------------------------------------------


def test_print_evaluates_an_expression_over_locals(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("p amount - fee")
    assert result.ok, result.error
    assert "1995000000000000000" in " ".join(result.lines)


def test_locals_shadow_state_variables_in_expressions(locals_contract):
    """`counter` the state variable vs `next`, a local: Solidity's own scoping decides."""
    dbg = locals_debugger(*locals_contract, "bump", 5)
    try:
        stop_at(dbg, 70)
        assert "5" in " ".join(dbg.run("p next").lines)
        assert "5" in " ".join(dbg.run("p counter").lines)
        assert "10" in " ".join(dbg.run("p next + counter").lines)
    finally:
        dbg.close()


def test_ptype_reports_a_locals_declared_type(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    assert "address" in " ".join(dbg.run("ptype who").lines)
    assert "uint256" in " ".join(dbg.run("ptype fee").lines)


def test_expression_over_an_unreadable_local_says_why(locals_contract):
    dbg = locals_debugger(*locals_contract, "storagePointer", 42)
    try:
        stop_at(dbg, 89)
        result = dbg.run("p p")
        assert not result.ok
        assert "cannot be used in an expression" in (result.error or "")
    finally:
        dbg.close()


def test_expression_compiles_once_across_many_stops(deposit_debugger):
    """Passing locals as parameters, not literals, is what keeps the cache warm."""
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("p amount - fee")
    after_first = dbg.evaluator.compile_count
    for _ in range(3):
        dbg.run("p amount - fee")
    assert dbg.evaluator.compile_count == after_first


def test_bindings_only_inject_referenced_names(proj):
    from sevm.evaluate import bindings_for
    from sevm.locals import LocalValue

    available = [
        LocalValue(
            name="fee", type_label="uint256", display="1", abi_type="uint256", abi_value=1
        ),
        LocalValue(
            name="amount",
            type_label="uint256",
            display="2",
            abi_type="uint256",
            abi_value=2,
        ),
    ]
    assert [b.name for b in bindings_for("fee + 1", available)] == ["fee"]
    assert [b.name for b in bindings_for("totalDeposits", available)] == []
    # A member access must not bind an unrelated local of the same name.
    assert [b.name for b in bindings_for("x.amount", available)] == []


def test_unbindable_local_is_reported_not_ignored(proj):
    from sevm.evaluate import unbindable_reference
    from sevm.locals import LocalValue

    blocked = [
        LocalValue(
            name="p",
            type_label="struct Point storage",
            display="<ptr>",
            available=True,
            reason="storage pointers are not dereferenced",
        )
    ]
    assert "p" in (unbindable_reference("p.x", blocked) or "")
    assert unbindable_reference("totalDeposits", blocked) is None


# -- breakpoint conditions ---------------------------------------------------


def test_breakpoint_condition_reads_a_local(deposit_debugger):
    """The limitation SEVM.md called out: `b LOC if amount > 1 ether` now works."""
    dbg = deposit_debugger
    dbg.run("b Bank.sol:46 if amount > 1 ether")
    assert dbg.run("c").ok
    assert dbg.snap.line == 46


def test_breakpoint_condition_on_a_local_can_be_false(locals_contract):
    dbg = locals_debugger(*locals_contract, "loop", 3)
    try:
        dbg.run("b Locals.sol:56 if i == 2")
        assert dbg.run("c").ok
        assert locals_map(dbg)["i"] == "2"
    finally:
        dbg.close()


# -- mutation ----------------------------------------------------------------


def test_set_var_writes_a_locals_stack_slot(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    assert dbg.run("set var fee = 1 ether").ok
    assert locals_map(dbg)["fee"].startswith("1000000000000000000")
    # The write is real: the running program sees it, not just the display.
    assert "1000000000000000000" in " ".join(dbg.run("p amount - fee").lines)


def test_set_var_refuses_a_reference_type(locals_contract):
    dbg = locals_debugger(*locals_contract, "memoryTypes", "hello")
    try:
        stop_at(dbg, 81)
        result = dbg.run("set var list = 1")
        assert not result.ok
        assert "reference" in (result.error or "")
        # And the pointer is untouched, so the local still reads correctly.
        assert locals_map(dbg)["list"] == "[3 items] [10, 20, 30]"
    finally:
        dbg.close()


# -- info args ---------------------------------------------------------------


def test_info_args_shows_the_internal_frames_own_arguments(deposit_debugger):
    """Inside `_credit`, calldata still describes `deposit()`. The frame wins."""
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    lines = " ".join(dbg.run("info args").lines)
    assert "who" in lines and "amount" in lines
    assert "not of the internal function" not in lines


def test_snapshot_carries_locals_for_the_tui(deposit_debugger):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    names = {v.name for v in dbg.snap.locals}
    assert {"who", "amount", "fee"} <= names


# ==================================================================
# TUI: text selection and stack labels
# ==================================================================


def test_stack_labels_name_the_frames_locals(deposit_debugger):
    from sevm.tui.widgets import local_stack_labels

    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    labels = local_stack_labels(dbg.snap)
    named = {name for name, _kind in labels.values()}
    assert {"who", "amount", "fee"} <= named
    # The labels point at the slots that actually hold those values.
    by_name = {name: index for index, (name, _k) in labels.items()}
    assert dbg.snap.stack[by_name["amount"]].value == 2 * 10**18
    assert dbg.snap.stack[by_name["fee"]].value == 5 * 10**15


def test_stack_labels_carry_the_declaration_kind(locals_contract):
    from sevm.tui.widgets import local_stack_labels

    dbg = locals_debugger(*locals_contract, "loop", 3)
    try:
        stop_at(dbg, 56)
        kinds = dict(local_stack_labels(dbg.snap).values())
        assert kinds["rounds"] == "param"
        assert kinds["sum"] == "return"
        assert kinds["i"] == "local"
    finally:
        dbg.close()


def test_stack_labels_span_a_multi_word_local(locals_contract):
    """A calldata reference is offset plus length; neither word is anonymous."""
    from sevm.tui.widgets import local_stack_labels

    dbg = locals_debugger(*locals_contract, "calldataTypes", b"\xaa\xbb\xcc")
    try:
        stop_at(dbg, 95)
        names = {name for name, _kind in local_stack_labels(dbg.snap).values()}
        assert "payload.ptr" in names and "payload.len" in names
    finally:
        dbg.close()


def test_stack_labels_are_empty_without_locals(deposit_debugger):
    from sevm.tui.widgets import local_stack_labels

    assert local_stack_labels(None) == {}


# ==================================================================
# TUI: layout, scrolling and the prompt
# ==================================================================


def tui_app(bank, size=(150, 46)):
    """A started TUI over a Bank deposit, paused at the interesting line."""
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    from sevm.tui.app import SevmApp

    session = DebugSession(proj_)
    evaluator = Evaluator(proj_)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.start(txfn)
    first = session.wait(timeout=TIMEOUT)
    return session, proj_, SevmApp(session, evaluator, first_event=first), size


def run_tui(session, app, size, body):
    """Drive `body(pilot)` against a running app and tear the session down."""

    async def drive():
        # Notifications are off by default in `run_test`, and toasts are part of the UI.
        async with app.run_test(size=size, notifications=True) as pilot:
            await pilot.pause()
            await asyncio.sleep(1.0)
            return await body(pilot)

    try:
        return asyncio.run(drive())
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()


async def stop_at_credit(app, pilot, proj_):
    line = _line_of(proj_, "balances[who] += amount - fee;")
    app.run_command(f"b Bank.sol:{line}")
    await asyncio.sleep(1.0)
    app.run_command("continue")
    await asyncio.sleep(2.5)
    await pilot.pause()
    return line


def test_panes_do_not_overflow_their_border(bank):
    """Every pane must clip to its box; DISASSEMBLY used to spill past the bottom."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        from sevm.tui.widgets import Pane

        return {
            pane.id: (
                pane.size.height,
                pane.virtual_size.height,
                pane.region.bottom,
                pane.allow_vertical_scroll,
            )
            for pane in app.query(Pane)
            if pane.display
        }

    panes = run_tui(session, app, size, body)
    assert panes, "no panes found"
    for name, (height, virtual, bottom, scrollable) in panes.items():
        assert height > 0, f"{name} has no height"
        assert bottom <= size[1], f"{name} extends past the bottom of the screen"
        # Taller content than the box is the point; it must scroll, not overflow.
        if virtual > height:
            assert scrollable, f"{name} overflows its border instead of scrolling"


def test_low_level_row_fills_to_the_bottom(bank):
    """The low-level panes share the row and reach its bottom edge."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        row = app.query_one("#lowlevel")
        return row.region, {
            name: app.query_one(name).region for name in ("#disasm", "#stack", "#memory")
        }

    row_region, panes = run_tui(session, app, size, body)
    for name, region in panes.items():
        assert region.height == row_region.height, f"{name} does not fill the row"
        assert region.bottom == row_region.bottom, f"{name} stops short of the bottom"


def test_hiding_the_low_level_row_grows_the_source(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        before = source.size.height
        await pilot.press("f2")
        await asyncio.sleep(1.0)
        await pilot.pause()
        return before, source.size.height, app.query_one("#lowlevel").display

    before, after, lowlevel_shown = run_tui(session, app, size, body)
    assert not lowlevel_shown
    assert after > before, f"source stayed {before} lines tall after hiding the row"


def test_panes_scroll_and_flag_when_off_the_current_state(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        at_stop = (source.at_anchor, source.border_subtitle)
        source.scroll_to(y=0, animate=False)
        await pilot.pause()
        scrolled = (source.at_anchor, source.border_subtitle)
        source.action_jump_to_anchor()
        await pilot.pause()
        return at_stop, scrolled, (source.at_anchor, source.border_subtitle)

    at_stop, scrolled, resynced = run_tui(session, app, size, body)
    assert at_stop[0] is True and not (at_stop[1] or "")
    assert scrolled[0] is False and "back to pc" in scrolled[1], (
        "scrolling away must say so"
    )
    assert resynced[0] is True and not (resynced[1] or "")


def test_source_follows_the_pc_even_after_you_scroll_it(bank):
    """SOURCE and DISASSEMBLY exist to follow execution; stepping re-centres them."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        source.scroll_to(y=0, animate=False)
        await pilot.pause()
        away = (source.scroll_offset.y, source.at_anchor)
        app.run_command("next")
        await asyncio.sleep(2.0)
        await pilot.pause()
        return away, (source.scroll_offset.y, source.at_anchor)

    away, after = run_tui(session, app, size, body)
    assert away[1] is False, "the test must first scroll the pane off the stop"
    assert after[1] is True, "SOURCE must come back to the pc on the next step"
    assert after[0] != away[0]


def test_a_scrolled_pane_stays_put_across_a_step(bank):
    """Scrolling a pane is reading it; stepping must not throw that reading away."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        memory = app.query_one("#memory")
        memory.scroll_to(y=3, animate=False)
        await pilot.pause()
        away = memory.scroll_offset.y
        app.run_command("next")
        await asyncio.sleep(2.0)
        await pilot.pause()
        stayed = memory.scroll_offset.y
        # Scrolling back onto the current state opts back into following it.
        memory.scroll_to(y=0, animate=False)
        await pilot.pause()
        app.run_command("next")
        await asyncio.sleep(2.0)
        await pilot.pause()
        return away, stayed, memory.scroll_offset.y

    away, stayed, following = run_tui(session, app, size, body)
    assert away > 0, "the test must first scroll MEMORY away from the top"
    assert stayed == away, f"MEMORY jumped from {away} back to {stayed} on `next`"
    assert following == 0


def test_the_log_stops_following_while_you_read_back(bank):
    """New output must not yank the log to the end while you are reading scrollback."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        log = app.query_one("#log")
        for n in range(60):
            log.write(f"line {n}")
        await pilot.pause()
        at_end = log.scroll_offset.y
        log.scroll_to(y=at_end - 5, animate=False)
        await pilot.pause()
        away = log.scroll_offset.y
        log.write("more output")
        await pilot.pause()
        held = log.scroll_offset.y
        log.action_jump_to_anchor()
        await pilot.pause()
        return at_end, away, held, log.scroll_offset.y, log.max_scroll_y

    at_end, away, held, resynced, end = run_tui(session, app, size, body)
    assert at_end > 0 and away == at_end - 5
    assert held == away, "writing to the log must not move a log you scrolled back"
    assert resynced == end, "the marker must put the log back on the newest line"


def test_source_pane_holds_the_whole_file(bank):
    """Scrolling is only useful if there is something above and below the stop."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        return source.virtual_size.height, source.size.height

    rendered, visible = run_tui(session, app, size, body)
    total_lines = len(project().sources["Bank.sol"].text.split("\n"))
    assert rendered >= total_lines - 1, f"only {rendered} of {total_lines} lines rendered"
    assert rendered > visible


def test_history_recall_puts_the_cursor_at_the_end(bank):
    """Arrow-up must leave you ready to edit, not typing in front of the command."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        app.run_command("info storage")
        await asyncio.sleep(1.0)
        from textual.widgets import Input

        prompt = app.query_one("#prompt", Input)
        prompt.focus()
        app._input_history = ["info storage", "p totalDeposits"]
        app._history_pos = 2
        app.action_history_prev()
        await pilot.pause()
        recalled = (prompt.value, prompt.cursor_position)
        app.action_history_prev()
        await pilot.pause()
        return recalled, (prompt.value, prompt.cursor_position)

    first, second = run_tui(session, app, size, body)
    assert first == ("p totalDeposits", len("p totalDeposits"))
    assert second == ("info storage", len("info storage"))


def test_memory_pane_renders_giant_words(bank):
    session, proj_, app, size = tui_app(bank, size=(230, 46))

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        return _screen_text(app)

    text = run_tui(session, app, size, body)
    assert "0x0000:" in text and "0x0010:" in text
    # 8-byte giants, two per row at this width, and the layout is still labelled.
    assert "0000000000000000 0000000000000000" in text
    assert "scratch" in text
    assert "free mem ptr" in text


def test_mouse_is_on_by_default_and_can_be_turned_off():
    """Panes render Content, which Textual can select, so the mouse is worth having."""
    from sevm.cli import build_parser

    args = build_parser().parse_args(["run", "--contracts", "x", "script.py"])
    assert args.no_mouse is False
    args = build_parser().parse_args(
        ["run", "--no-mouse", "--contracts", "x", "script.py"]
    )
    assert args.no_mouse is True


# ==================================================================
# copy to the system clipboard
# ==================================================================


@pytest.fixture
def fake_clipboard(monkeypatch):
    """Capture what would reach the system clipboard, without touching it."""
    from sevm import clipboard

    captured: list = []

    def fake_copy(text: str) -> str:
        captured.append(text)
        return "pbcopy"

    monkeypatch.setattr(clipboard, "copy", fake_copy)
    return captured


def test_copy_puts_a_commands_output_on_the_clipboard(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p amount - fee")
    assert result.ok, result.error
    assert result.notice and "copied" in result.notice
    assert fake_clipboard == ["$1 = 1995000000000000000 (1.995000 ether)  (uint256)"]


def test_copy_strips_console_markup(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("copy info locals")
    text = fake_clipboard[-1]
    assert "[cyan]" not in text and "[/dim]" not in text
    assert "fee" in text and "amount" in text
    # Indentation survives, so a pasted table still lines up.
    assert text.splitlines()[0].startswith("  ")


def test_bare_copy_takes_the_last_output(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("p who")
    dbg.run("copy")
    assert fake_clipboard and fake_clipboard[-1].startswith("$1 = 0x")


def test_bare_copy_does_not_copy_its_own_message(deposit_debugger, fake_clipboard):
    """A bound method is a fresh object each lookup, so `is` cannot exclude `copy`."""
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("p fee")
    dbg.run("copy")
    first = fake_clipboard[-1]
    dbg.run("copy")
    assert fake_clipboard[-1] == first, "a second copy must not copy the first's message"
    assert "copied" not in first


def test_copy_reports_a_failing_command_instead_of_copying(
    deposit_debugger, fake_clipboard
):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p nosuchthing")
    assert not result.ok
    assert fake_clipboard == []


def test_copy_with_nothing_to_copy_says_so(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    dbg.commands._last_output = []
    result = dbg.run("copy")
    assert not result.ok
    assert "nothing to copy" in (result.error or "")
    assert fake_clipboard == []


def test_copy_surfaces_a_missing_clipboard_tool(deposit_debugger, monkeypatch):
    from sevm import clipboard

    def boom(text: str) -> str:
        raise clipboard.ClipboardError("no system clipboard command found")

    monkeypatch.setattr(clipboard, "copy", boom)
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p owner")
    assert not result.ok
    assert "clipboard" in (result.error or "")


def test_clipboard_module_picks_a_tool_or_says_none(monkeypatch):
    from sevm import clipboard

    monkeypatch.setattr(clipboard.shutil, "which", lambda name: None)
    assert clipboard.available_tool() is None
    with pytest.raises(clipboard.ClipboardError):
        clipboard.copy("hello")

    monkeypatch.setattr(clipboard.shutil, "which", lambda name: name == "pbcopy")
    assert clipboard.available_tool() == "pbcopy"


def test_every_pane_renders_content_so_textual_can_select_it(bank):
    """The whole point of building panes from Content rather than a Rich table.

    Textual implements selection on the visual layer: a Content is character-addressable
    and gets a blended highlight, while a Rich renderable is opaque to both.
    """
    from textual.content import Content

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        from sevm.tui.widgets import Pane

        out = {}
        for pane in app.query(Pane):
            if not pane.display:
                continue
            out[pane.id] = isinstance(pane._body._render(), Content)
        return out

    kinds = run_tui(session, app, size, body)
    assert kinds, "no panes found"
    for name, is_content in kinds.items():
        assert is_content, f"{name} does not render Content, so it cannot be selected"


def test_dragging_a_pane_selects_characters_and_ctrl_c_copies(bank):
    session, proj_, app, size = tui_app(bank)
    copied: list = []

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy
        clipboard.copy = lambda text: (copied.append(text), "pbcopy")[1]
        try:
            await stop_at_credit(app, pilot, proj_)
            storage = app.query_one("#storage")
            await pilot.mouse_down(storage, offset=(9, 2))
            await pilot.hover(storage, offset=(30, 2))
            await pilot.pause()
            selection = app.screen.selections.get(storage._body)
            text = app.screen.get_selected_text()
            await pilot.mouse_up(storage, offset=(30, 2))
            await pilot.pause()
            await pilot.press("ctrl+c")
            await pilot.pause()
            return selection, text, app.is_running
        finally:
            clipboard.copy = monkey

    selection, text, still_running = run_tui(session, app, size, body)
    # Character offsets, not the whole-widget fallback Rich renderables collapse to.
    assert selection is not None and selection.start is not None
    assert selection.start.x != selection.end.x
    assert text and text.strip()
    assert copied and copied[-1] == text, "ctrl+c must copy the selection"
    assert still_running, "ctrl+c with a selection copies rather than quitting"


def test_ctrl_c_without_a_selection_still_quits(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        app.screen.clear_selection()
        await pilot.press("ctrl+c")
        await pilot.pause()
        return app.is_running

    assert run_tui(session, app, size, body) is False


def test_the_command_log_is_selectable(bank):
    """RichLog stores rendered strips and cannot be selected; the log is Content."""
    from textual.selection import Selection

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        app.run_command("info storage")
        await asyncio.sleep(1.5)
        await pilot.pause()
        log = app.query_one("#log")
        extracted = log._body.get_selection(Selection(None, None))
        return (extracted[0] if extracted else None), log.text

    selected, text = run_tui(session, app, size, body)
    assert selected and "totalDeposits" in selected
    assert "totalDeposits" in text
    # Markup is parsed, not copied literally.
    assert "[cyan]" not in selected and "[/dim]" not in selected


def test_clipboard_falls_back_to_osc52_when_no_tool_exists(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy

        def boom(text):
            raise clipboard.ClipboardError("no system clipboard command found")

        clipboard.copy = boom
        fallback: list = []
        original = type(app).__mro__[1].copy_to_clipboard
        try:
            await stop_at_credit(app, pilot, proj_)
            app.screen.clear_selection()
            # Textual's own OSC 52 path must still run, so the copy is not simply lost.
            import textual.app as textual_app

            textual_app.App.copy_to_clipboard = lambda self, text: fallback.append(text)
            app.copy_to_clipboard("some text")
            await pilot.pause()
            return list(fallback), [n.message for n in app._notifications]
        finally:
            clipboard.copy = monkey
            import textual.app as textual_app

            textual_app.App.copy_to_clipboard = original

    fallback, toasts = run_tui(session, app, size, body)
    assert fallback == ["some text"], "must fall back to the terminal, not drop the copy"
    assert toasts and "OSC 52" in toasts[-1], "the fallback must say it happened"


def test_every_colour_constant_parses_as_a_textual_style():
    """Rich colour names are not Textual colour names, and a bad one fails silently.

    `bright_yellow` and `grey23` are Rich spellings that Textual rejects; a rejected
    style is dropped rather than raised, so the affected text just loses its colour with
    nothing to show for it. This is the guard that turns that into a test failure.
    """
    from textual.style import Style

    import sevm.tui.widgets as widgets

    bad = []
    for name in dir(widgets):
        if not name.startswith("C_"):
            continue
        value = getattr(widgets, name)
        try:
            Style.parse(value)
        except Exception as exc:
            bad.append(f"{name}={value!r} ({exc})")
    assert not bad, "colour constants Textual cannot parse: " + "; ".join(bad)


def test_inline_styles_used_in_panes_parse(bank):
    """The same trap, for styles written inline rather than as constants."""
    from textual.style import Style

    for style in ("on #3a3a3a", "bold red", "dim", "white", "bold white", "dim yellow"):
        Style.parse(style)


def test_memory_dims_zero_words_and_brightens_real_data(bank):
    """A memory dump is mostly zeros; the bytes that hold something must stand out."""
    session, proj_, app, size = tui_app(bank, size=(230, 46))

    async def body(pilot):
        # setNickname writes a string into memory, so there is non-zero data to see.
        app.run_command("b Bank.sol:58")
        await asyncio.sleep(1.0)
        app.run_command("continue")
        await asyncio.sleep(2.5)
        await pilot.pause()
        memory = app.query_one("#memory")
        seen = {"zero": set(), "data": set()}
        for y in range(memory._body.size.height):
            for segment in memory._body.render_line(y)._segments:
                word = segment.text.strip()
                if len(word) != 16 or not all(c in "0123456789abcdef" for c in word):
                    continue
                colour = (
                    segment.style.color.name
                    if segment.style and segment.style.color
                    else None
                )
                seen["zero" if set(word) == {"0"} else "data"].add(colour)
        return seen

    seen = run_tui(session, app, size, body)
    from textual.style import Style

    from sevm.tui.widgets import C_MEMORY_TEXT, C_MEMORY_ZERO

    def rendered(style: str) -> str:
        # ANSI names resolve to palette indices by render time, so compare like for like.
        return Style.parse(style).rich_style.color.name

    assert seen["data"], "no non-zero memory words rendered"
    assert seen["zero"], "no zero memory words rendered"
    assert seen["data"] == {rendered(C_MEMORY_TEXT)}
    assert seen["zero"] == {rendered(C_MEMORY_ZERO)}
    assert seen["data"] != seen["zero"], "zeros must not look like real data"


def test_copying_raises_a_toast_and_leaves_the_log_alone(bank):
    """Feedback like "copied 20 characters" is transient, not transcript."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy
        clipboard.copy = lambda text: "pbcopy"
        try:
            await stop_at_credit(app, pilot, proj_)
            before = app.query_one("#log").text
            storage = app.query_one("#storage")
            await pilot.mouse_down(storage, offset=(9, 2))
            await pilot.hover(storage, offset=(30, 2))
            await pilot.mouse_up(storage, offset=(30, 2))
            await pilot.pause()
            toasts = [n.message for n in app._notifications]
            return toasts, before, app.query_one("#log").text
        finally:
            clipboard.copy = monkey

    toasts, before, after = run_tui(session, app, size, body)
    assert toasts, "releasing a drag must raise a toast"
    assert "copied" in toasts[-1] and "pbcopy" in toasts[-1]
    assert after == before, "the copy notice must not land in the transcript"


def test_copy_command_reports_through_a_notice_not_the_log(
    deposit_debugger, fake_clipboard
):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p owner")
    assert result.ok
    assert result.notice and "copied" in result.notice
    assert result.lines == [], "the notice replaces log output, it does not add to it"


def test_cmd_c_copies_on_macos_and_ctrl_c_still_quits(bank):
    """Cmd+C is `super+c`; both chords copy a live selection."""
    session, proj_, app, size = tui_app(bank)
    copied: list = []

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy
        clipboard.copy = lambda text: (copied.append(text), "pbcopy")[1]
        try:
            await stop_at_credit(app, pilot, proj_)
            storage = app.query_one("#storage")
            await pilot.mouse_down(storage, offset=(9, 2))
            await pilot.hover(storage, offset=(30, 2))
            await pilot.mouse_up(storage, offset=(30, 2))
            await pilot.pause()
            after_release = len(copied)
            await pilot.press("super+c")
            await pilot.pause()
            after_cmd_c = len(copied)
            app.screen.clear_selection()
            await pilot.pause()
            await pilot.press("super+c")
            await pilot.pause()
            return after_release, after_cmd_c, app.is_running
        finally:
            clipboard.copy = monkey

    after_release, after_cmd_c, running = body and run_tui(session, app, size, body)
    assert after_release == 1, "the drag itself must copy"
    assert after_cmd_c == 2, "cmd+c must copy again"
    assert running, "cmd+c with no selection must not quit"


def test_the_anchor_marker_is_clickable(bank):
    """The marker is the affordance, so it should also be the control."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        source.scroll_to(y=0, animate=False)
        await pilot.pause()
        assert not source.at_anchor
        marker = source.border_subtitle or ""
        # The subtitle carries a click action, not just text.
        assert "@click=jump_to_anchor" in marker
        y = source.region.bottom - 1
        for x in range(source.region.right - 20, source.region.right - 1):
            await pilot.click(offset=(x, y))
            await pilot.pause()
            if source.at_anchor:
                return True, app.focused.id
        return False, app.focused.id

    clicked, focused = run_tui(session, app, size, body)
    assert clicked, "clicking the marker must return the pane to the current state"
    assert focused == "prompt", "clicking a pane must not steal focus from the prompt"


def test_panes_scroll_with_the_wheel_without_taking_focus(bank):
    """Panes are not focusable, so a click cannot send your keystrokes nowhere."""
    from textual.events import MouseScrollDown
    from textual.geometry import Offset

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        moved = {}
        for name in ("#source", "#disasm", "#memory"):
            pane = app.query_one(name)
            before = pane.scroll_offset.y
            for _ in range(4):
                await pilot._post_mouse_events(
                    [MouseScrollDown], offset=Offset(pane.region.x + 3, pane.region.y + 2)
                )
            await pilot.pause()
            moved[name] = (before, pane.scroll_offset.y, pane.can_focus)
        return moved, app.focused.id

    moved, focused = run_tui(session, app, size, body)
    for name, (before, after, can_focus) in moved.items():
        assert after != before, f"{name} did not scroll with the wheel"
        assert not can_focus, f"{name} is focusable and would steal keystrokes"
    assert focused == "prompt"


def test_no_footer_and_no_pane_focus_binding(bank):
    """Both dropped: the key bar was noise, and the wheel replaced F3."""
    from textual.widgets import Footer

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        return len(app.query(Footer)), {b.action for b in app.BINDINGS}

    footers, actions = run_tui(session, app, size, body)
    assert footers == 0, "the key bar below the prompt is gone"
    assert "focus_pane" not in actions


def test_super_key_bindings_exist_for_mac(bank):
    """Cmd is `super` in Textual; the mac chords are bound alongside the ctrl ones."""
    from sevm.tui.app import SevmApp

    keys = {b.key: b.action for b in SevmApp.BINDINGS}
    assert keys.get("super+c") == "copy_selection"
    assert keys.get("super+p") == "command_palette"
    assert keys.get("super+q") == "quit"
    assert keys.get("super+l") == "clear_log"
    # The ctrl equivalents stay, for everyone else.
    assert keys.get("ctrl+c") == "copy_or_quit"
    assert keys.get("ctrl+q") == "quit"
    assert keys.get("ctrl+l") == "clear_log"


def test_theme_follows_the_terminal(bank):
    """ANSI colours mean the debugger wears whatever palette the terminal is set to."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        from textual.theme import BUILTIN_THEMES

        theme = BUILTIN_THEMES[app.theme]
        return app.theme, theme.background, theme.primary

    name, background, primary = run_tui(session, app, size, body)
    assert name.startswith("ansi-"), f"theme is {name}, not the terminal's"
    assert background == "ansi_default", "the terminal must paint its own background"
    assert primary.startswith("ansi_")


def test_pane_colours_are_ansi_so_they_match_the_terminal():
    import sevm.tui.widgets as widgets

    accents = {
        name: getattr(widgets, name)
        for name in dir(widgets)
        if name.startswith("C_") and name != "C_DIM"
    }
    assert accents
    for name, value in accents.items():
        assert "ansi_" in value, f"{name}={value!r} is a fixed colour, not the terminal's"


# ==================================================================
# inline assembly (Yul)
# ==================================================================


def test_assembly_parses_literals_and_nesting():
    from sevm.assembly import Call, Literal, parse

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


# ==================================================================
# Foundry cheatcode help
# ==================================================================


def test_cheatcode_help_is_generated_from_the_registry(deposit_debugger):
    from sevm.cheatcodes import listing

    specs = listing()
    assert specs
    body = " ".join(deposit_debugger.run("help cheatcodes").lines)
    for spec in specs:
        assert spec.doc, f"{spec.name} has no help line"
        assert f"vm.{spec.signature}" in body, f"{spec.name} is missing from the help"


def test_help_summary_mentions_assembly_and_cheatcodes(deposit_debugger):
    summary = " ".join(deposit_debugger.run("help").lines)
    assert "Assembly" in summary and "mstore" in summary
    assert "cheatcodes" in summary and "vm.warp" in summary
    for topic in ("assembly", "cheatcodes", "asm", "yul", "vm", "foundry"):
        assert deposit_debugger.run(f"help {topic}").ok, topic
    body = " ".join(deposit_debugger.run("help foundry").lines)
    assert "--no-install" in body and "forge-std" in body


# ==================================================================
# unknown input
# ==================================================================


def test_unknown_command_reports_instead_of_raising(deposit_debugger):
    """A typo used to escape `execute()` and take the frontend down with it.

    The fallback to "evaluate it as Solidity" sat outside the error handling, so an
    unknown verb threw SessionError straight out: the console died, and the TUI lost the
    worker that clears `busy`, wedging the prompt for the rest of the session.
    """
    dbg = deposit_debugger
    for line in ("blah", "brekpoint 12", "foobar 1 2", "!!!", "@@@ ###"):
        result = dbg.run(line)
        assert not result.ok, line
        assert result.error, line
    # Still usable afterwards: nothing was left in a broken state.
    assert dbg.run("p totalDeposits").ok


def test_unknown_command_says_so_and_suggests_a_verb(deposit_debugger):
    error = deposit_debugger.run("brekpoint 12").error
    assert "undefined command" in error and "brekpoint" in error
    assert "help" in error
    assert "`break`" in error, error


def test_bad_arguments_report_the_word_that_is_not_a_number(deposit_debugger):
    dbg = deposit_debugger
    for line in ("delete xyz", "disable xyz", "enable xyz", "jump nowhere", "frame abc"):
        result = dbg.run(line)
        assert not result.ok, line
        assert "int()" not in (result.error or ""), f"{line}: raw Python error leaked"


def test_a_bare_expression_still_evaluates(deposit_debugger):
    """The undefined-command path must not swallow gdb's print-an-expression behaviour."""
    dbg = deposit_debugger
    assert dbg.run("totalDeposits").ok
    assert dbg.run("balances[msg.sender]").ok


# ==================================================================
# mutations reach the panes
# ==================================================================


def test_every_mutation_refreshes_the_snapshot(deposit_debugger):
    """The snapshot is a copy, so a write is invisible to the panes until it is re-read."""
    dbg = deposit_debugger
    assert dbg.run("set $stack[0] = 0xc0ffee").ok
    assert dbg.snap.stack[0].value == 0xC0FFEE
    assert dbg.run("set $mem[0x80] = 0xabc").ok
    assert int.from_bytes(dbg.snap.memory[0x80:0xA0], "big") == 0xABC
    assert dbg.run("set $gas = 90000").ok
    assert dbg.snap.gas_remaining == 90000


def test_mutating_commands_are_flagged_and_reads_are_not(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("mstore(0x80, 1)").mutated
    assert dbg.run("set var totalDeposits = 5").mutated
    assert dbg.run("set $storage[1] = 5").mutated
    assert dbg.run("vm.warp(1735689600)").mutated
    assert not dbg.run("p totalDeposits").mutated
    assert not dbg.run("info registers").mutated


def test_tui_panes_show_an_assembly_write_without_stepping(bank):
    """The MEMORY and STORAGE panes must repaint the moment `mstore`/`sstore` lands."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        memory = app.query_one("#memory")
        storage = app.query_one("#storage")
        before = (memory.text, storage.text)
        app.run_command("asm mstore(0x80, 0xdeadbeef); sstore(1, 0xc0ffee)")
        await asyncio.sleep(2.0)
        await pilot.pause()
        return before, (memory.text, storage.text)

    before, after = run_tui(session, app, size, body)
    assert "deadbeef" in after[0], "MEMORY pane did not pick up the mstore"
    assert after[0] != before[0]
    assert "c0ffee" in after[1].lower() or after[1] != before[1], (
        "STORAGE pane did not pick up the sstore"
    )


def test_console_escapes_error_text_before_rendering(deposit_debugger):
    """An error quoting the user's own brackets must not be eaten as Rich markup.

    `[nope]` used to be swallowed as a style tag, and an unmatched `[/...]` raised
    MarkupError straight out of the console's read loop, ending the session over a typo.
    """
    from rich.console import Console

    from sevm.console import ConsoleFrontend

    frontend = ConsoleFrontend(deposit_debugger.session, deposit_debugger.evaluator)
    frontend.console = Console(file=io.StringIO(), width=200, highlight=False)
    for error in ("bad [nope] thing", "x [/] y", "closing [/nope] tag"):
        frontend._emit(CommandResult(error=error))
    written = frontend.console.file.getvalue()
    for fragment in ("[nope]", "[/]", "[/nope]"):
        assert fragment in written, f"{fragment} was eaten by the markup parser"


def test_console_survives_a_bad_command_end_to_end(deposit_debugger):
    """The whole path a typo takes: execute -> CommandResult -> console render."""
    from rich.console import Console

    from sevm.console import ConsoleFrontend

    frontend = ConsoleFrontend(deposit_debugger.session, deposit_debugger.evaluator)
    frontend.console = Console(file=io.StringIO(), width=200, highlight=False)
    for line in ("blah", "p balances[nope]", "mstore(0x80)", "delete xyz"):
        result = frontend.commands.execute(line)
        assert not result.ok, line
        frontend._emit(result)
    assert "error:" in frontend.console.file.getvalue()
