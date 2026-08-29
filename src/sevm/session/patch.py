"""The Py-EVM monkeypatch.

`apply_computation` is replaced with a copy of Py-EVM's own opcode loop that calls back
into the session around every instruction. The preamble is verbatim so create-message
accounting and precompile dispatch stay bit-identical; only the loop body differs.
"""

from __future__ import annotations

from typing import Any

from eth.chains.base import Chain
from eth.exceptions import Halt, VMError
from eth.vm.computation import NO_RESULT, BaseComputation
from eth.vm.logic.invalid import InvalidOpcode

from ..cheatcodes import CONSOLE_ADDRESS, VM_ADDRESS

# The raw classmethod DESCRIPTOR, never the bound method. Restoring a bound method pins
# `cls` to BaseComputation, and the next real call gets opcodes=None and dies.
ORIGINAL_APPLY = BaseComputation.__dict__["apply_computation"]

# eth_estimateGas binary-searches the gas limit by re-running the tx, so early probes fail
# with OutOfGas by design. Run estimation with the hook suspended, or the debugger stops on
# a bogus failure mid-search.
ORIGINAL_ESTIMATE = Chain.__dict__["estimate_gas"]

# Call opcodes a prank applies to (the child's msg.sender / value source is the caller's
# storage_address): CALL, CALLCODE, STATICCALL. DELEGATECALL (0xF4) keeps the real caller.
_PRANK_CALL_OPCODES = frozenset({0xF1, 0xF2, 0xFA})


def make_apply_patch(session: Any) -> Any:
    @classmethod  # type: ignore[misc]
    def patched(cls, state, message, transaction_context, parent_computation=None):
        # Preamble copied verbatim from BaseComputation.apply_computation so that
        # create-message accounting and precompile dispatch stay bit-identical.
        with cls(state, message, transaction_context) as computation:
            if computation.is_origin_computation:
                computation.contracts_created = []
                if message.is_create:
                    cls.consume_initcode_gas_cost(computation)
                if session.foundry_mode:
                    session._ensure_cheat_code(computation.state)
            if parent_computation is not None:
                computation.contracts_created = parent_computation.contracts_created
            if message.is_create:
                computation.contracts_created.append(message.storage_address)

            precompile = computation.precompiles.get(message.code_address, NO_RESULT)
            if precompile is not NO_RESULT:
                if not message.is_delegation:
                    precompile(computation)
                return computation

            # Foundry cheatcodes: a call to the magic VM address is interpreted here
            # instead of executing (empty) code. console.log is the same idea.
            if message.code_address == VM_ADDRESS:
                session._run_cheatcode(computation, message)
                return computation
            if message.code_address == CONSOLE_ADDRESS:
                session._run_console(computation, message)
                return computation

            tracing = session.armed and not getattr(session._local, "suspended", False)
            frame = session._enter_frame(computation, message) if tracing else None

            opcode_lookup = computation.opcodes
            try:
                for opcode in computation.code:
                    try:
                        opcode_fn = opcode_lookup[opcode]
                    except KeyError:
                        opcode_fn = InvalidOpcode(opcode)

                    if frame is not None and session.armed:
                        pc = max(0, computation.code.program_counter - 1)
                        try:
                            mnemonic = opcode_fn.mnemonic
                        except AttributeError:
                            mnemonic = opcode_fn.__wrapped__.mnemonic  # type: ignore[attr-defined]
                        session._on_opcode(frame, computation, pc, opcode, mnemonic)
                        gas_before = computation._gas_meter.gas_remaining
                        # A failing opcode may already have popped its operands
                        # (py-evm charges some costs after the pops), so keep a copy
                        # to restore if the instruction is retried after a gas
                        # rescue.
                        stack_backup = list(computation._stack.values)
                        try:
                            exec_opcode(session, opcode, opcode_fn, computation)
                        except Halt:
                            session._account_gas(
                                frame, computation, pc, mnemonic, gas_before
                            )
                            break
                        except VMError as error:
                            # Pause on the failing instruction with the stack, memory
                            # and gas still intact. Normally the error then propagates;
                            # but if the user topped the meter up during the pause
                            # (`set $gas = N`), the opcode never ran to completion
                            # and is retried on the restored stack.
                            paused = session._on_vm_error(
                                frame, computation, pc, mnemonic, error
                            )
                            if not (paused and session._gas_rescued):
                                raise
                            session._gas_rescued = False
                            computation._stack.values[:] = stack_backup
                            try:
                                exec_opcode(session, opcode, opcode_fn, computation)
                            except Halt:
                                session._account_gas(
                                    frame, computation, pc, mnemonic, gas_before
                                )
                                break
                        session._account_gas(frame, computation, pc, mnemonic, gas_before)
                        session._after_opcode(frame, computation, pc, mnemonic)
                    else:
                        try:
                            exec_opcode(session, opcode, opcode_fn, computation)
                        except Halt:
                            break
            finally:
                if frame is not None:
                    session._exit_frame(frame)
        # A gas rescue can leave the meter above the message's own ceiling, which
        # would later serialize as negative gas_used; clamp so the receipt stays
        # constructible. Legitimate execution can never exceed the ceiling.
        if computation._gas_meter.gas_remaining > message.gas:
            computation._gas_meter.gas_remaining = message.gas
        if computation.is_origin_computation and computation.is_error:
            session._record_transaction_revert(computation)
        return computation

    return patched


def make_estimate_patch(session: Any) -> Any:
    def estimate_gas(chain, transaction, at_header=None):  # type: ignore[no-untyped-def]
        session.estimations += 1
        with session.suspended():
            return ORIGINAL_ESTIMATE(chain, transaction, at_header)

    return estimate_gas


def exec_opcode(session: Any, opcode: int, opcode_fn: Any, computation: Any) -> None:
    """Run one opcode, applying an active prank to a call it makes.

    A prank must take effect at the calling opcode, not when the child frame starts:
    the EVM sources the call's value and gas, and the child's msg.sender, from the
    caller's `storage_address`. Temporarily setting that to the pranked address for the
    duration of the call makes value, gas and msg.sender all follow the prank, exactly
    as forge does. DELEGATECALL is excluded: it preserves the original caller.
    """
    prank = session.cheats.prank
    if (
        prank is None
        or opcode not in _PRANK_CALL_OPCODES
        or (
            prank.caller is not None
            and bytes(computation.msg.storage_address) != prank.caller
        )
    ):
        opcode_fn(computation=computation)
        return

    saved = computation.msg.storage_address
    computation.msg.storage_address = prank.new_sender
    try:
        opcode_fn(computation=computation)
    finally:
        computation.msg.storage_address = saved
        if not prank.persistent:
            session.cheats.prank = None
