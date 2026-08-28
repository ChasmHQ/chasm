"""Breakpoints, conditions, watchpoints, and stopping on a failure."""

from __future__ import annotations

import contextlib

from harness import (
    Debugger,
    line_of,
)

from sevm.breakpoints import BreakpointSet
from sevm.decode import (
    StorageDecoder,
)
from sevm.session import Finished, Paused, StepMode


def test_line_breakpoint_stops_there(deposit_debugger):
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "balances[who] += amount - fee;")
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
    line = line_of(dbg.session.project, "balances[who] += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line, condition="totalDeposits > 1000 ether")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Finished)


def test_conditional_breakpoint_that_is_true_stops(deposit_debugger):
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "balances[who] += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line, condition="totalDeposits > 0")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.line == line


def test_condition_over_a_local_is_evaluated_not_skipped(deposit_debugger):
    """This used to be the documented gap: conditions could not see locals."""
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "return (amount * feeBps) / 10000;")
    bp, _ = dbg.session.break_at_line("Bank.sol", line, condition="amount > 1 ether")
    event = dbg.step(StepMode.RUN)
    assert isinstance(event, Paused)
    assert event.snapshot.line == line
    assert bp.condition_error is None


def test_broken_condition_breaks_and_records_why(deposit_debugger):
    """An unevaluatable condition still breaks, as gdb does, and reports the reason."""
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "return (amount * feeBps) / 10000;")
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
