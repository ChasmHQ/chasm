"""Recovering a Solidity frame's local variables from the EVM stack.

Locals have no runtime representation: their stack position is inferred from where solc's
source map says each declaration executed, which `DebugSession._observe_declaration`
records as the program runs. This module turns those positions back into values.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from ..frames import EvmFrame, stack_int
from ..locals import LocalsIndex, LocalValue, read_local


def read_frame_locals(
    index: LocalsIndex,
    frame: EvmFrame,
    computation: Any,
    internal_index: int | None = None,
) -> list[LocalValue]:
    """Every local visible in one Solidity frame, decoded.

    Three things have to line up before a value is shown, and each one is a way the
    naive version reads the wrong word:

      * the frame must have been observed from its entry, so the base is real;
      * the slot must lie below the current stack top, which is what retires a
        variable whose block has already been popped;
      * the current instruction must be inside the declaration's scope, which is
        what stops a slot recorded in an exited block from resurfacing under a
        temporary that happens to sit at the same height.

    Anything that fails reports `<unavailable>` with the reason.
    """
    internals = frame.internal
    if not internals:
        return []
    if internal_index is None or not 0 <= internal_index < len(internals):
        internal_index = len(internals) - 1
    internal = internals[internal_index]
    fn = internal.function
    if fn is None:
        return []
    layout = index.for_function(fn.ast_id)
    if layout is None or not layout.all:
        return []

    innermost = internal_index == len(internals) - 1
    pc_here = max(0, computation.code.program_counter - 1)
    show_pc = pc_here if innermost else internals[internal_index + 1].call_site_pc
    loc = frame.location(show_pc)
    if loc is None or loc.is_generated:
        return []
    offset = loc.entry.start

    stack = computation._stack.values
    sp = len(stack)
    memory = computation._memory._bytes

    def read_memory(start: int, size: int) -> bytes:
        data = bytes(memory[start : start + size])
        return data + b"\x00" * (size - len(data))  # unwritten memory reads as zero

    positions = _param_positions(layout.params, internal.entry_sp)
    for var in layout.returns + layout.body:
        recorded = internal.slots.get(var.ast_id)
        if recorded is not None:
            positions[var.ast_id] = recorded

    # A modifier's locals sit in this same frame, so anything recorded here that the
    # function does not own is a modifier's, and the scope check below decides
    # whether the user is currently standing inside that modifier's body.
    extra: list[Any] = []
    for ast_id in internal.slots:
        var = index.by_ast_id(ast_id)
        if var is not None and var.function_id != fn.ast_id and var.visible_at(offset):
            positions[var.ast_id] = internal.slots[ast_id]
            extra.append(var)

    out: list[LocalValue] = []
    candidates = [v for v in index.visible(fn.ast_id, offset) if v.name]
    for var in candidates + extra:
        base = positions.get(var.ast_id)
        width = var.slots
        if base is None:
            out.append(_unavailable(var, "not allocated yet at this instruction"))
            continue
        if width is None:
            out.append(_unavailable(var, f"unknown stack width for {var.display_type}"))
            continue
        if base < 0 or base + width > sp:
            # Two different situations look identical from the stack alone, and the
            # user needs to be told which one they are in.
            pending = var.start <= offset < var.end
            out.append(
                _unavailable(
                    var,
                    "this instruction allocates it; step once to see it"
                    if pending
                    else "out of scope: its stack slot has been popped",
                )
            )
            continue
        words = tuple(stack_int(stack[base + i]) for i in range(width))
        value = read_local(var, words, read_memory)
        value.position = base
        if var.statement_start >= 0 and var.statement_start <= offset < var.statement_end:
            value.reason = value.reason or "still inside its own initialiser"
        out.append(value)
    return out


def _param_positions(params: Sequence[Any], entry_sp: int | None) -> dict[int, int]:
    """Place parameters below the frame base, from the top down.

    Walking in reverse matters: a parameter of unknown width only invalidates the
    ones *deeper* than it, so one exotic type does not blind the whole frame.
    """
    positions: dict[int, int] = {}
    if entry_sp is None:
        return positions
    cursor = entry_sp
    for var in reversed(list(params)):
        width = var.slots
        if width is None:
            break
        cursor -= width
        positions[var.ast_id] = cursor
    return positions


def _unavailable(var: Any, reason: str) -> LocalValue:
    return LocalValue(
        name=var.name or f"<{var.kind}>",
        type_label=var.display_type,
        display="<unavailable>",
        available=False,
        reason=reason,
        kind=var.kind,
    )
