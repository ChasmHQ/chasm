"""Build the source-level backtrace attached to a REVM pause."""

from __future__ import annotations

from typing import Any

from ..frames import BacktraceRow


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
