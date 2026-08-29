"""What a pause looks like to the controller.

A `FrameSnapshot` is an immutable copy taken on the VM thread, so the UI can render it
without touching a live Py-EVM object. `live_view` is the subset a mutation can change
under the UI's feet, shared with the `resnapshot` inspect op so a refresh after a write
cannot drift from what the pause produced.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from ..frames import BacktraceRow, EvmFrame, FrameSnapshot, StackEntry, stack_int
from ..srcmap import Location

# How much memory travels with each snapshot. The rest is available on request.
SNAPSHOT_MEMORY_LIMIT = 4096


def live_view(session: Any, frame: EvmFrame, computation: Any) -> dict[str, Any]:
    """The parts of a snapshot that a mutation can change under the UI's feet.

    Shared by `_build_snapshot` and the `resnapshot` inspect op, so a refresh after a
    write produces the same fields the pause did rather than a second, drifting copy.
    """
    raw_stack = list(computation._stack.values)
    meter = computation._gas_meter
    return {
        "stack": tuple(
            StackEntry(index=i, value=stack_int(v), raw=v)
            for i, v in enumerate(reversed(raw_stack))
        ),
        "memory": bytes(computation._memory._bytes[:SNAPSHOT_MEMORY_LIMIT]),
        "memory_size": len(computation._memory),
        "gas_remaining": meter.gas_remaining,
        "gas_used": meter.start_gas - meter.gas_remaining,
        "gas_refund": meter.gas_refunded,
        "locals": tuple(session.frame_locals(frame, computation)),
        # `reseat` rewrites the internal call stack, so the backtrace is a mutable field
        # too: a refresh after it must rebuild the CALL STACK, not just the VARIABLES.
        "backtrace": tuple(build_backtrace(session)),
    }


def build_snapshot(
    session: Any,
    frame: EvmFrame,
    computation: Any,
    pc: int,
    opcode: int,
    mnemonic: str,
    loc: Location | None,
    reason: str,
    hits: Sequence[int],
    annotation: str,
) -> FrameSnapshot:
    live = live_view(session, frame, computation)
    meter = computation._gas_meter

    source_key = None
    if loc is not None and not loc.is_generated:
        src = session.project.source_by_id(loc.file_id)
        source_key = src.key if src else None

    static_gas = None
    opcode_obj = computation.opcodes.get(opcode)
    if opcode_obj is not None:
        static_gas = getattr(opcode_obj, "gas_cost", None)

    return FrameSnapshot(
        step=session.step_index,
        pc=pc,
        opcode=opcode,
        mnemonic=mnemonic,
        depth=frame.depth,
        gas_limit=meter.start_gas,
        address=frame.address,
        code_address=frame.code_address,
        sender=frame.sender,
        origin=bytes(computation.transaction_context.origin),
        value=frame.value,
        calldata=frame.calldata,
        is_static=frame.is_static,
        contract_name=frame.artifact_name,
        source_key=source_key,
        file_id=loc.file_id if loc else -1,
        line=loc.line if loc and not loc.is_generated else 0,
        col=loc.col if loc and not loc.is_generated else 0,
        end_line=loc.end_line if loc and not loc.is_generated else 0,
        jump=loc.jump if loc else "-",
        function=session.functions.at_location(loc),
        stop_reason=reason,
        **live,
        hit_breakpoints=tuple(hits),
        static_gas=static_gas,
        annotation=annotation,
    )


def build_backtrace(session: Any) -> list[BacktraceRow]:
    """Interleaved EVM and internal frames, innermost first, gdb ordering.

    Each Solidity frame is shown at the line it is *currently executing*, which for
    an outer frame is the call site of the frame it called. Compiler-generated
    helper frames (solc's ABI encode/decode routines) are collapsed unless execution
    is actually inside one, since a backtrace full of `<compiler-generated>` hides
    the program the user wrote.
    """
    rows: list[BacktraceRow] = []
    index = 0
    for evm_index in range(len(session._frames) - 1, -1, -1):
        frame = session._frames[evm_index]
        src = session.project.sources.get(
            frame.artifact.source_key if frame.artifact else ""
        )
        source_key = src.key if src else None
        pc_here = max(0, frame.computation.code.program_counter - 1)
        internals = frame.internal
        for k in range(len(internals) - 1, -1, -1):
            innermost = k == len(internals) - 1
            if internals[k].is_generated and not innermost:
                continue
            show_pc = pc_here if innermost else internals[k + 1].call_site_pc
            loc = frame.location(show_pc)
            rows.append(
                BacktraceRow(
                    index=index,
                    name=internals[k].name,
                    line=loc.line if loc and not loc.is_generated else 0,
                    pc=show_pc,
                    kind="solidity",
                    detail="" if not internals[k].is_generated else "compiler-generated",
                    address=frame.address,
                    evm_index=evm_index,
                    internal_index=k,
                    source_key=source_key,
                )
            )
            index += 1

        # The EVM frame boundary itself. When internal frames are present the
        # outermost of them already named the function, so this row is the call.
        loc = frame.location(pc_here)
        if internals:
            name = frame.artifact_name or "0x" + frame.address.hex()[:8]
            line = 0
            pc_show = internals[0].call_site_pc
        else:
            fn = session.functions.at_location(loc)
            name = (
                fn.signature
                if fn
                else (frame.artifact_name or "0x" + frame.address.hex()[:8])
            )
            line = loc.line if loc and not loc.is_generated else 0
            pc_show = pc_here
        rows.append(
            BacktraceRow(
                index=index,
                name=name,
                line=line,
                pc=pc_show,
                kind="evm",
                detail=f"{frame.kind} depth={frame.depth}",
                address=frame.address,
                evm_index=evm_index,
                source_key=source_key,
            )
        )
        index += 1
    return rows
