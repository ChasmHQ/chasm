"""Writing to the live VM: storage, the stack, memory, gas and the pc."""

from __future__ import annotations

import pytest
from harness import (
    Debugger,
)

from sevm.session import Paused, SessionError, StepMode


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
