"""Breakpoints and watchpoints.

A location is a line, a `*pc`, an opcode mnemonic, or a function name; a watch target is a
Solidity lvalue resolved to its storage slot, or `*0xOFFSET` for memory.
"""

from __future__ import annotations

import re
from typing import TYPE_CHECKING

from ..breakpoints import WATCH_ACCESS, WATCH_READ, WATCH_WRITE
from ..frames import FrameSnapshot
from .parsing import _breakpoint_numbers, _known_opcodes
from .render import _short
from .result import CommandResult

if TYPE_CHECKING:
    from .processor import CommandProcessor


def _make_break(
    proc: CommandProcessor, args: list[str], rest: str, temporary: bool
) -> CommandResult:
    condition = None
    if " if " in rest:
        rest, condition = rest.split(" if ", 1)
        condition = condition.strip()
        args = rest.split()
    result = CommandResult()
    if not args:
        snap = proc.require_stop()
        if not snap.has_source:
            return CommandResult(error="no source here; use `break *0xPC`")
        bp, line = proc.session.break_at_line(
            snap.source_key, snap.line, temporary=temporary, condition=condition
        )
        return result.add(f"Breakpoint {bp.number} at {snap.source_key}:{line}")

    spec = args[0]
    if spec.startswith("*"):
        bp = proc.session.break_at_pc(
            int(spec[1:], 0), temporary=temporary, condition=condition
        )
        return result.add(f"Breakpoint {bp.number} at pc {spec[1:]}")
    if spec.upper() in _known_opcodes():
        bp = proc.session.break_at_opcode(
            spec.upper(), temporary=temporary, condition=condition
        )
        return result.add(f"Breakpoint {bp.number} on every {spec.upper()}")
    if ":" in spec or spec.isdigit():
        source_key, line = proc.parse_location(spec, proc.snapshot)
        bp, snapped = proc.session.break_at_line(
            source_key, line, temporary=temporary, condition=condition
        )
        note = (
            ""
            if snapped == line
            else f" [dim](line {line} has no code; moved to {snapped})[/dim]"
        )
        if bp.pending:
            note += " [yellow]<pending: no compiled code at that line>[/yellow]"
        return result.add(f"Breakpoint {bp.number} at {source_key}:{snapped}{note}")
    bp, line = proc.session.break_at_function(
        spec, temporary=temporary, condition=condition
    )
    return result.add(f"Breakpoint {bp.number} at {bp.location} (line {line})")


def cmd_break(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return _make_break(proc, args, rest, temporary=False)


def cmd_tbreak(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return _make_break(proc, args, rest, temporary=True)


def cmd_delete(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    if not args:
        proc.session.breakpoints.clear()
        return CommandResult().add("all breakpoints deleted")
    numbers = _breakpoint_numbers(args, "delete")
    removed = [n for n in numbers if proc.session.breakpoints.remove(n)]
    if not removed:
        return CommandResult(error="no such breakpoint")
    return CommandResult().add(f"deleted {', '.join(str(n) for n in removed)}")


def cmd_disable(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    n = _breakpoint_numbers(args, "disable")[0] if args else None
    count = proc.session.breakpoints.set_enabled(n, False)
    return CommandResult().add(f"disabled {count}")


def cmd_enable(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    n = _breakpoint_numbers(args, "enable")[0] if args else None
    count = proc.session.breakpoints.set_enabled(n, True)
    return CommandResult().add(f"enabled {count}")


def cmd_watch(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """Break when a storage value changes."""
    return _watch(proc, rest, WATCH_WRITE)


def cmd_rwatch(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """Break when a storage slot is READ (an SLOAD of it)."""
    return _watch(proc, rest, WATCH_READ)


def cmd_awatch(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """Break on either a read or a write."""
    return _watch(proc, rest, WATCH_ACCESS)


def _watch(proc: CommandProcessor, rest: str, mode: str) -> CommandResult:
    """`watch <state var or mapping element>` or `watch *0xOFFSET` for memory."""
    snap = proc.require_stop()
    verb = {
        WATCH_WRITE: "Watchpoint",
        WATCH_READ: "Read watchpoint",
        WATCH_ACCESS: "Access watchpoint",
    }[mode]
    expr = rest.strip()
    if not expr:
        return CommandResult(error="usage: watch <expression>")
    if expr.startswith("*"):
        if mode != WATCH_WRITE:
            return CommandResult(
                error="read watchpoints apply to storage, not memory; use `watch *ADDR`"
            )
        wp = proc.session.watch_memory(expr, int(expr[1:], 0))
        return CommandResult().add(f"Watchpoint {wp.number}: memory at {expr[1:]}")
    slot = _slot_of(proc, expr, snap)
    if slot is None:
        return CommandResult(
            error=f"cannot resolve {expr!r} to a storage slot; "
            "watch a state variable, a mapping element, or *0xOFFSET for memory"
        )
    wp = proc.session.watch_storage(expr, slot, address=snap.address, mode=mode)
    return CommandResult().add(
        f"{verb} {wp.number}: {expr} (slot 0x{slot:x} of {_short(snap.address)})"
    )


def _slot_of(proc: CommandProcessor, expr: str, snap: FrameSnapshot) -> int | None:
    """Resolve a Solidity lvalue to its storage slot.

    State variables come straight from `storageLayout`. Mapping and array elements
    use the layout rules (keccak256(key . slot) and keccak256(slot) + i), with the
    key itself evaluated through solc so `balances[msg.sender]` works.
    """
    decoder = proc.decoder(snap.contract_name)
    if decoder is None:
        return None
    base = expr.split("[")[0].split(".")[0].strip()
    var = decoder.get(base)
    if var is None:
        return None
    if "[" not in expr and "." not in expr:
        return var.slot
    # mapping/array element: compute the slot with the same helper solc would.
    from ..decode import dynamic_array_slot, mapping_slot

    match = re.match(r"^\w+\[(.+?)\]$", expr.strip())
    if not match:
        return None
    key_expr = match.group(1)
    type_info = decoder.types.get(var.type_id, {})
    try:
        key_value = proc.evaluate(key_expr).value
    except Exception:
        return None
    if type_info.get("encoding") == "mapping":
        return mapping_slot(key_value, var.slot)
    if type_info.get("encoding") == "dynamic_array":
        return dynamic_array_slot(var.slot) + int(key_value)
    return var.slot + int(key_value)


# ==================================================================
# inspection commands
# ==================================================================


VERBS = {
    "break": cmd_break,
    "b": cmd_break,
    "br": cmd_break,
    "tbreak": cmd_tbreak,
    "delete": cmd_delete,
    "d": cmd_delete,
    "disable": cmd_disable,
    "enable": cmd_enable,
    "watch": cmd_watch,
    "rwatch": cmd_rwatch,
    "awatch": cmd_awatch,
}
