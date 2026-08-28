"""When a step stops, and when a watchpoint fires.

The one part of the engine with real rules rather than plumbing, and the part most likely
to need changing. Everything here runs on the VM thread, inside the opcode hook, and reads
the `_mode_*` baseline that `set_baseline` writes at each resume.
"""

from __future__ import annotations

from typing import Any

from ..breakpoints import WATCH_ACCESS, WATCH_READ, Watchpoint
from ..frames import EvmFrame, stack_int
from ..srcmap import Location
from .events import StepMode


def should_stop(
    session: Any,
    frame: EvmFrame,
    computation: Any,
    pc: int,
    opcode: int,
    mnemonic: str,
    loc: Location | None,
) -> tuple[list[int], str | None]:
    # Breakpoints fire regardless of step mode, as in gdb.
    hits: list[int] = []
    if not session.breakpoints.is_empty:
        contract = frame.artifact_name
        file_id = loc.file_id if loc else -1
        line = loc.line if loc else 0
        for bp in session.breakpoints.match(pc, mnemonic, file_id, line, contract):
            if bp.condition and not _condition_holds(session, bp, frame, computation):
                continue
            bp.hit_count += 1
            if bp.ignore_count > 0:
                bp.ignore_count -= 1
                continue
            hits.append(bp.number)
            if bp.temporary:
                session.breakpoints.remove(bp.number)
    if hits:
        return hits, "breakpoint"

    read_hit = _read_watch_hit(session, frame, computation, mnemonic)
    if read_hit is not None:
        return [read_hit], "watchpoint"
    session._pending_annotation = ""

    mode = session._mode
    if mode is StepMode.RUN:
        return [], None

    depth = frame.depth
    internal = frame.internal_depth

    if mode is StepMode.STEPI:
        return [], _consume_count(session, "step")
    if mode is StepMode.NEXTI:
        if depth <= session._mode_depth:
            return [], _consume_count(session, "step")
        return [], None
    if mode is StepMode.UNTIL:
        if pc == session._mode_target_pc and depth <= session._mode_depth:
            return [], "until"
        return [], None
    if mode is StepMode.FINISH:
        if depth < session._mode_depth:
            return [], "finish"
        if depth == session._mode_depth and internal < session._mode_internal:
            return [], "finish"
        return [], None

    # STEP / NEXT: source-line granularity.
    if loc is None or loc.is_generated or loc.line <= 0:
        return [], None
    if _is_dispatcher_location(session, loc):
        return [], None

    if mode is StepMode.NEXT:
        if depth > session._mode_depth:
            return [], None
        if depth == session._mode_depth and internal > session._mode_internal:  # noqa: SIM102 - the nested `if` keeps the solc self-jump exemption below readable
            # Deeper in the internal (JUMP-based) call stack, so ordinarily this is a
            # call we are stepping over. The exception is the compiler's own jump from
            # a function's declaration into its body, which solc also marks 'i'. If we
            # treated that as a nested call, `next` at a function's opening line would
            # skip the entire function.
            if not _entering_own_body(session, loc, internal):
                return [], None

    if depth != session._mode_depth or loc.key() != session._mode_key:
        return [], _consume_count(session, "step", loc)
    return [], None


def _entering_own_body(session: Any, loc: Location, internal: int) -> bool:
    """True when the only thing that got deeper is the jump into our own body.

    Requires that the previous stop sat exactly on a function's declaration range, so
    a recursive call (whose call site is a statement inside the body, not the
    declaration) is still correctly stepped over.
    """
    if not session._mode_at_function_entry or internal != session._mode_internal + 1:
        return False
    current = session.functions.at_location(loc)
    return current is not None and current == session._mode_function


def _consume_count(session: Any, reason: str, loc: Location | None = None) -> str | None:
    """Support `next 5`: only actually stop on the last repetition."""
    session._pending_count -= 1
    if session._pending_count > 0:
        frame = session.current_frame
        if frame is not None:
            set_baseline(session, frame, loc)
        return None
    return reason


def set_baseline(session: Any, frame: EvmFrame, loc: Location | None) -> None:
    """Record where a step started, which is what every stop test compares against."""
    session._mode_depth = frame.depth
    session._mode_internal = frame.internal_depth
    mapped = loc is not None and not loc.is_generated and loc.line > 0
    session._mode_key = loc.key() if mapped else None
    fn = session.functions.at_location(loc) if mapped else None
    session._mode_function = fn
    session._mode_at_function_entry = bool(
        fn is not None
        and loc is not None
        and loc.entry.start == fn.start
        and loc.entry.length == fn.length
    )


def _is_dispatcher_location(session: Any, loc: Location) -> bool:
    """Suppress the selector dispatcher, which maps to the whole contract range.

    Without this, every `step` into a contract stops first on `contract Foo {`,
    which teaches the user nothing and costs them a keystroke.
    """
    for start, end, _name in session.functions.contracts.get(loc.file_id, []):
        if loc.entry.start == start and loc.entry.start + loc.entry.length == end:
            return True
    return False


def _condition_holds(session: Any, bp: Any, frame: EvmFrame, computation: Any) -> bool:
    """Evaluate a breakpoint condition.

    A condition that cannot be evaluated breaks anyway, as gdb does, but the reason
    is recorded on the breakpoint so the UI can say so. Silently treating a broken
    condition as "always true" would leave the user guessing why they stopped.
    """
    if session._eval_hook is None:
        return True
    try:
        result = session._eval_hook(
            session,
            frame,
            computation,
            bp.condition,
            want_bool=True,
            bindings=session.frame_locals(frame, computation),
        )
    except Exception as exc:
        bp.condition_error = str(exc)
        return True
    bp.condition_error = None
    return bool(result)


# -- watchpoints --------------------------------------------------------


def _read_watch_hit(
    session: Any, frame: EvmFrame, computation: Any, mnemonic: str
) -> int | None:
    """Read watchpoints fire on the SLOAD itself, before the value is fetched.

    Write watchpoints cannot work that way: a write is only observable by comparing
    the slot before and after, which is why the two live in different places.
    """
    if mnemonic != "SLOAD" or not session.breakpoints.has_watchpoints:
        return None
    values = computation._stack.values
    if not values:
        return None
    slot = stack_int(values[-1])
    for wp in session.breakpoints.active_watchpoints():
        if wp.kind != "storage" or wp.mode not in (WATCH_READ, WATCH_ACCESS):
            continue
        if wp.slot == slot and (wp.address is None or wp.address == frame.address):
            wp.hit_count += 1
            current = computation.state.get_storage(frame.address, slot)
            session._pending_annotation = f"{wp.expression}: read 0x{current:x}"
            return wp.number
    return None


def check_watchpoints(
    session: Any, frame: EvmFrame, computation: Any, pc: int, mnemonic: str
) -> None:
    triggered: list[Watchpoint] = []
    for wp in session.breakpoints.active_watchpoints():
        if wp.mode == WATCH_READ:
            continue  # handled before the SLOAD, in _read_watch_hit
        try:
            current = _read_watch_value(wp, frame, computation)
        except Exception:
            continue
        if not wp.initialised:
            wp.old_value = current
            wp.initialised = True
            continue
        if current != wp.old_value:
            wp.hit_count += 1
            wp.old_value_previous = wp.old_value  # type: ignore[attr-defined]
            wp.old_value = current
            triggered.append(wp)
    if triggered:
        wp = triggered[0]
        old = getattr(wp, "old_value_previous", None)
        note = f"{wp.expression}: {_fmt_value(old)} -> {_fmt_value(wp.old_value)}"
        new_pc = computation.code.program_counter
        session._pause(
            frame,
            computation,
            new_pc,
            0,
            mnemonic,
            frame.location(new_pc),
            "watchpoint",
            [wp.number],
            annotation=note,
        )


def _read_watch_value(wp: Watchpoint, frame: EvmFrame, computation: Any) -> int | None:
    if wp.kind == "storage":
        address = wp.address or frame.address
        if wp.slot is None:
            return None
        return computation.state.get_storage(address, wp.slot)
    if wp.kind == "memory" and wp.offset is not None:
        data = bytes(computation._memory.read_bytes(wp.offset, wp.size))
        return int.from_bytes(data, "big")
    return None


def _fmt_value(value: int | None) -> str:
    if value is None:
        return "<unset>"
    return f"0x{value:x}"
