"""Verbs that let the program run: continue, step, next, finish, until."""

from __future__ import annotations

from typing import TYPE_CHECKING

from ..session import StepMode
from .parsing import _count
from .result import CommandResult

if TYPE_CHECKING:
    from .processor import CommandProcessor


def cmd_continue(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return proc.resume(StepMode.RUN)


def cmd_next(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return proc.resume(StepMode.NEXT, _count(args))


def cmd_step(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return proc.resume(StepMode.STEP, _count(args))


def cmd_stepi(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return proc.resume(StepMode.STEPI, _count(args))


def cmd_nexti(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return proc.resume(StepMode.NEXTI, _count(args))


def cmd_finish(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return proc.resume(StepMode.FINISH)


def cmd_until(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    if not args:
        return proc.resume(StepMode.NEXT)
    target = args[0]
    snap = proc.require_stop()
    if target.startswith("*"):
        pc = int(target[1:], 0)
    else:
        source_key, line = proc.parse_location(target, snap)
        file_id = proc.session.file_id_for(source_key)
        if file_id is None:
            return CommandResult(error=f"no source file matching {source_key!r}")
        _snapped, pcs = proc.session.resolve_line(file_id, line)
        if not pcs:
            return CommandResult(error=f"no code at {source_key}:{line}")
        pc = min(pcs)
    return proc.resume(StepMode.UNTIL, target_pc=pc)


# ==================================================================
# breakpoint commands
# ==================================================================


VERBS = {
    "continue": cmd_continue,
    "c": cmd_continue,
    "cont": cmd_continue,
    "next": cmd_next,
    "n": cmd_next,
    "step": cmd_step,
    "s": cmd_step,
    "stepi": cmd_stepi,
    "si": cmd_stepi,
    "nexti": cmd_nexti,
    "ni": cmd_nexti,
    "finish": cmd_finish,
    "fin": cmd_finish,
    "until": cmd_until,
    "u": cmd_until,
    "advance": cmd_until,
}
