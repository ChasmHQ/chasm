"""The gdb-compatible command layer.

Design rule: if gdb has a verb for it, we use gdb's verb and gdb's abbreviation. A user
who knows gdb should need to learn only the Solidity-specific parts. New verbs exist only
for things gdb has no concept of (`info storage`, `info gas`, `info logs`).

The processor is shared by both frontends: the plain console calls `execute()` directly,
and the TUI calls it from a worker thread because `continue` blocks until the VM stops.

Output is Rich markup so the console and the TUI's log pane render it identically.
"""

from __future__ import annotations

import contextlib
import difflib
import re
import shlex
import textwrap
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from rich.markup import escape as rich_escape
from rich.text import Text

from . import clipboard
from .assembly import AsmError, has_builtin_head, lexes
from .assembly import listing as assembly_listing
from .breakpoints import WATCH_ACCESS, WATCH_READ, WATCH_WRITE
from .cheatcodes import (
    CheatError,
    encode_cheat_call,
    format_cheat_result,
    parse_cheat_arg,
)
from .cheatcodes import all_specs as cheat_specs
from .cheatcodes import listing as cheat_listing
from .decode import StorageDecoder, decode_calldata
from .evaluate import EvalError, EvalResult, Evaluator
from .frames import FrameSnapshot
from .session import DebugSession, Finished, Paused, SessionError, StepMode

# Convenience variables, gdb-style. These bypass solc entirely so they work even when a
# contract has no source.
_CONVENIENCE = re.compile(
    r"\$(pc|gas|gasused|depth|sp|step|stack\[(\d+)\]|mem\[(0x[0-9a-fA-F]+|\d+)\]"
    r"|storage\[(0x[0-9a-fA-F]+|\d+)\]|(\d+))"
)

# Only the formats we actually implement. gdb's `i` (instruction) and `f` (float) have no
# meaning over EVM memory, so they are rejected with a message rather than silently
# falling through to hex.
_X_FORMAT = re.compile(r"^/(\d*)([xduotc s]?)([bhwg]?)$".replace(" ", ""))

_UNIT_SIZES = {"b": 1, "h": 2, "w": 4, "g": 8}


@dataclass
class CommandResult:
    lines: list[str] = field(default_factory=list)
    # A short, transient message: the TUI raises it as a toast rather than adding it to
    # the transcript, since "copied 20 characters" is feedback, not output worth keeping.
    notice: str | None = None
    error: str | None = None
    resumed: bool = False
    quit: bool = False
    event: Any = None
    # The command wrote to the live VM (storage, memory, the stack, a local, a cheatcode).
    # `execute` re-reads the snapshot afterwards so the panes show the write immediately.
    mutated: bool = False

    def add(self, text: str = "") -> CommandResult:
        self.lines.append(text)
        return self

    @property
    def ok(self) -> bool:
        return self.error is None


def _split_top_level(text: str) -> list[str]:
    """Split on commas that are not inside quotes or parentheses (for `vm.x(a, b)`)."""
    parts: list[str] = []
    depth = 0
    quote: str | None = None
    current: list[str] = []
    for ch in text:
        if quote:
            current.append(ch)
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
            current.append(ch)
        elif ch in "([{":
            depth += 1
            current.append(ch)
        elif ch in ")]}":
            depth -= 1
            current.append(ch)
        elif ch == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(ch)
    if current:
        parts.append("".join(current))
    return [p.strip() for p in parts if p.strip()]


def _addr(raw: bytes | None) -> str:
    if not raw:
        return "0x0"
    return "0x" + bytes(raw).hex()


def _short(raw: bytes | None, keep: int = 4) -> str:
    text = _addr(raw)
    if len(text) <= 2 + keep * 2 + 2:
        return text
    return f"{text[: 2 + keep]}..{text[-keep:]}"


def _wei(value: int) -> str:
    if value == 0:
        return "0"
    if 10**15 <= value < 10**27:
        whole, frac = divmod(value, 10**18)
        if frac == 0:
            return f"{value} ({whole} ether)"
        return f"{value} ({value / 10**18:.6f} ether)"
    return str(value)


class CommandProcessor:
    """Parses and runs one command line against a live DebugSession."""

    def __init__(self, session: DebugSession, evaluator: Evaluator) -> None:
        self.session = session
        self.evaluator = evaluator
        self.project = session.project
        self.history: list[EvalResult] = []
        self.displays: list[tuple[int, str]] = []
        self._display_counter = 0
        # `selected_row` indexes the backtrace as the user sees it; `selected_frame` is
        # the EVM frame it lives in, which inspect commands target. Not the same number:
        # several Solidity frames share one EVM frame.
        self.selected_row: int = 0
        self.selected_frame: int | None = None  # None means the innermost
        # Which internal (Solidity) frame inside that EVM frame is selected. Several
        # Solidity frames share one EVM frame and one stack, so locals need both.
        self.selected_internal: int | None = None
        self.last_list_line = 0
        self._decoders: dict[str, StorageDecoder] = {}
        # What a bare `copy` picks up: the most recent command that produced output.
        self._last_output: list[str] = []
        # How many console.log lines have already been shown, so a resume only prints new
        # output produced while the program ran.
        self._console_seen = 0

        self._verbs: dict[str, Callable[[list[str], str], CommandResult]] = {}
        self._register()

    # ==================================================================
    # registration
    # ==================================================================

    def _register(self) -> None:
        table: dict[str, Callable[[list[str], str], CommandResult]] = {
            # execution
            "continue": self.cmd_continue,
            "c": self.cmd_continue,
            "cont": self.cmd_continue,
            "next": self.cmd_next,
            "n": self.cmd_next,
            "step": self.cmd_step,
            "s": self.cmd_step,
            "stepi": self.cmd_stepi,
            "si": self.cmd_stepi,
            "nexti": self.cmd_nexti,
            "ni": self.cmd_nexti,
            "finish": self.cmd_finish,
            "fin": self.cmd_finish,
            "until": self.cmd_until,
            "u": self.cmd_until,
            "advance": self.cmd_until,
            # breakpoints
            "break": self.cmd_break,
            "b": self.cmd_break,
            "br": self.cmd_break,
            "tbreak": self.cmd_tbreak,
            "delete": self.cmd_delete,
            "d": self.cmd_delete,
            "disable": self.cmd_disable,
            "enable": self.cmd_enable,
            "watch": self.cmd_watch,
            "rwatch": self.cmd_rwatch,
            "awatch": self.cmd_awatch,
            # inspection
            "print": self.cmd_print,
            "p": self.cmd_print,
            "inspect": self.cmd_print,
            "call": self.cmd_call,
            "ptype": self.cmd_ptype,
            "display": self.cmd_display,
            "undisplay": self.cmd_undisplay,
            "x": self.cmd_examine,
            "backtrace": self.cmd_backtrace,
            "bt": self.cmd_backtrace,
            "where": self.cmd_backtrace,
            "frame": self.cmd_frame,
            "f": self.cmd_frame,
            "up": self.cmd_up,
            "down": self.cmd_down,
            "list": self.cmd_list,
            "l": self.cmd_list,
            "disassemble": self.cmd_disassemble,
            "disas": self.cmd_disassemble,
            "info": self.cmd_info,
            "i": self.cmd_info,
            # mutation
            "set": self.cmd_set,
            "asm": self.cmd_asm,
            "assembly": self.cmd_asm,
            "yul": self.cmd_asm,
            "jump": self.cmd_jump,
            "copy": self.cmd_copy,
            "y": self.cmd_copy,
            # misc
            "help": self.cmd_help,
            "h": self.cmd_help,
            "?": self.cmd_help,
            "quit": self.cmd_quit,
            "q": self.cmd_quit,
            "exit": self.cmd_quit,
        }
        self._verbs = table

    # ==================================================================
    # entry point
    # ==================================================================

    def execute(self, line: str) -> CommandResult:
        """Run one prompt line. Never raises: a failure comes back as `result.error`.

        Load-bearing, not defensive: the TUI calls this from a worker thread owning the
        `busy` flag, so an escaped exception would end the console session, or in the TUI
        kill the worker with `busy` still set and wedge the prompt for the rest of the run.
        """
        line = line.strip()
        if not line:
            return CommandResult()
        try:
            result = self._dispatch(line)
        except (SessionError, EvalError, AsmError) as exc:
            return CommandResult(error=str(exc))
        except Exception as exc:  # a bug in a handler must not take the debugger with it
            return CommandResult(error=f"{type(exc).__name__}: {exc}")
        if result.mutated:
            # The snapshot was copied at the pause, so a write to memory, the stack or a
            # local is invisible until it is re-read. Do that here, once, for every
            # mutating command rather than in each of them.
            self.session.refresh_snapshot()
        return result

    def _dispatch(self, line: str) -> CommandResult:
        """Route a line to its handler. May raise; `execute` is the error boundary."""
        # Interactive Foundry cheatcode: `vm.warp(12345)`, `vm.deal(alice, 1 ether)`.
        if line.startswith("vm.") and "(" in line and line.endswith(")"):
            return self._remember(self.cmd_cheat(line))
        # Inline assembly: `mstore(0x80, 1)`, `sstore(3, add(sload(3), 1))`. Substitute
        # first so `$storage[1]` is a number before the line is judged as Yul; a line
        # that still won't lex is Solidity that merely starts with a builtin's name, and
        # falls through to the verb table below.
        if has_builtin_head(line):
            substituted = self._substitute(line)
            if lexes(substituted):
                return self._remember(self._assemble(substituted))
        parts = line.split(None, 1)
        verb = parts[0]
        rest = parts[1] if len(parts) > 1 else ""
        # gdb glues the format onto the verb: `x/32xb 0x40`, `disas/r`. Split it back off
        # so the verb table stays simple and the format reaches the handler as an argument.
        if "/" in verb and not verb.startswith("/"):
            verb, _, spec = verb.partition("/")
            rest = f"/{spec} {rest}".strip()
        handler = self._verbs.get(verb)
        if handler is None:
            return self._remember(self._not_a_command(line, verb))
        try:
            args = shlex.split(rest) if rest else []
        except ValueError:
            args = rest.split()
        result = handler(args, rest)
        # `copy` is excluded from the "last output" memo, or a bare `copy` would re-copy
        # its own "copied N characters" message. Compare `__func__`, not `is`: every
        # `self.cmd_copy` lookup builds a fresh bound method, so `is` would never match.
        if getattr(handler, "__func__", None) is CommandProcessor.cmd_copy:
            return result
        return self._remember(result)

    def _remember(self, result: CommandResult) -> CommandResult:
        """`copy` with no argument copies the last thing you looked at."""
        if result.lines:
            self._last_output = list(result.lines)
        return result

    def _not_a_command(self, line: str, verb: str) -> CommandResult:
        """gdb prints an expression's value when the verb is not a command; so do we.

        A typo is still a typo, though: `Undeclared identifier` for `brekapoint 12` sends
        the user looking for a variable they never wrote. When the line reads like a
        command, say so and offer the nearest verb instead.
        """
        try:
            return self.cmd_print([], line)
        except (SessionError, EvalError) as exc:
            if not _looks_like_a_command(line):
                # `bogusop(1)` is a call solc couldn't resolve; likeliest a misremembered
                # Yul builtin, so point at the list instead of leaving it at "Undeclared".
                if _CALL_SHAPED.match(line) and "Undeclared identifier" in str(exc):
                    raise SessionError(
                        f"{exc}; if you meant inline assembly, `help assembly` lists "
                        "the Yul builtins"
                    ) from exc
                raise
            suggestion = _did_you_mean(verb, self._verbs)
            return CommandResult(
                error=f'undefined command: "{_escape(verb)}".{suggestion} '
                "Try `help` for the command list, or `p <expr>` to evaluate Solidity."
            )

    def cmd_cheat(self, line: str) -> CommandResult:
        """Apply a Foundry cheatcode entered live: `vm.warp(12345)`, `vm.deal(a, 1 ether)`.

        Arguments are simple literals (ints, `N ether`, 0x addresses/bytes, quoted
        strings), not full Solidity expressions. Runs against the frame in view.
        """
        match = re.match(r"vm\.(\w+)\s*\((.*)\)\s*$", line, re.DOTALL)
        if not match:
            return CommandResult(error="usage: vm.<name>(arg, ...)")
        name, argstr = match.group(1), match.group(2).strip()
        arg_texts = _split_top_level(argstr) if argstr else []
        try:
            values = [parse_cheat_arg(t) for t in arg_texts]
            calldata = encode_cheat_call(name, values)
        except (CheatError, ValueError) as exc:
            return CommandResult(error=str(exc))
        try:
            output = self.session.inspect("cheat", calldata)
        except SessionError as exc:
            return CommandResult(error=str(exc))
        return CommandResult(mutated=True).add(
            f"vm.{name} -> {format_cheat_result(name, output)}"
        )

    def cmd_asm(self, args: list[str], rest: str) -> CommandResult:
        """`asm <yul>`: run inline assembly against the frame we are stopped in."""
        source = rest.strip()
        if not source:
            return CommandResult(
                error="usage: asm <yul>, e.g. `asm mstore(0x80, 1)`; "
                "`help assembly` lists the builtins"
            )
        return self._assemble(self._substitute(source))

    def _assemble(self, source: str) -> CommandResult:
        """Execute `;`-separated Yul statements on the live frame and report each one.

        `source` has already been through `_substitute`, so `mstore(0x80, $storage[1])`
        and `sstore(0, $stack[0])` arrive here as plain numbers.

        Unlike `p`, which runs on a throwaway snapshot, this writes for real: the
        low-level twin of `set var`, reaching places Solidity has no syntax for.
        """
        rows = self._inspect("assembly", source)
        result = CommandResult(mutated=True)
        for row in rows:
            cost = f"  [dim](gas {row['gas']:,})[/dim]"
            text = _escape(row["text"])
            if row["value"] is None:
                result.add(f"[yellow]{text}[/yellow] -> [green]ok[/green]{cost}")
                continue
            # Results join the value history, so `mload(0x40)` can be reused as `$1`
            # in the next expression exactly as a `p` result can.
            self.history.append(
                EvalResult(
                    expression=row["text"],
                    type_name="uint256",
                    abi_type="uint256",
                    value=row["value"],
                    display=_word(row["value"]),
                    gas_used=row["gas"],
                )
            )
            result.add(
                f"[yellow]{text}[/yellow] -> [bold]${len(self.history)}[/bold] = "
                f"{_word(row['value'])}{cost}"
            )
        return result

    def _emit_console(self, result: CommandResult) -> None:
        """Append any console.log output produced since the last resume."""
        lines = self.session.cheats.console_lines
        for line in lines[self._console_seen :]:
            result.add(f"[dim][console][/dim] {_escape(line)}")
        self._console_seen = len(lines)

    # ==================================================================
    # helpers
    # ==================================================================

    @property
    def snapshot(self) -> FrameSnapshot | None:
        return self.session.last_snapshot

    def _require_stop(self) -> FrameSnapshot:
        snap = self.snapshot
        if snap is None or self.session.finished:
            raise SessionError("the program is not running")
        return snap

    def _inspect(self, op: str, *args: Any, **kwargs: Any) -> Any:
        return self.session.inspect(op, *args, frame_index=self.selected_frame, **kwargs)

    def _decoder(self, contract: str | None) -> StorageDecoder | None:
        if not contract:
            return None
        if contract not in self._decoders:
            art = self.project.artifact(contract)
            self._decoders[contract] = StorageDecoder(art.storage_layout if art else None)
        return self._decoders[contract]

    def _source_lines(self, source_key: str | None) -> list[str]:
        if not source_key:
            return []
        src = self.project.sources.get(source_key)
        return src.text.split("\n") if src else []

    def _resume(
        self, mode: StepMode, count: int = 1, target_pc: int | None = None
    ) -> CommandResult:
        if self.session.finished:
            return CommandResult(error="the program has finished")
        self.selected_frame = None
        self.selected_row = 0
        event = self.session.resume(mode, count=count, target_pc=target_pc)
        result = CommandResult(resumed=True, event=event)
        self._emit_console(result)
        if isinstance(event, Finished):
            if event.ok:
                result.add("[green]program finished[/green]")
            else:
                result.add(f"[red]program raised {event.error}[/red]")
            return result
        if isinstance(event, Paused):
            result.lines.extend(self.describe_stop(event.snapshot))
            result.lines.extend(self._render_displays())
        elif event is None:
            result.error = "timed out waiting for the VM"
        return result

    # -- stop rendering -----------------------------------------------------

    def describe_stop(self, snap: FrameSnapshot) -> list[str]:
        """The two-or-three lines gdb prints when it stops."""
        out: list[str] = []
        if snap.stop_reason == "breakpoint" and snap.hit_breakpoints:
            nums = ", ".join(str(n) for n in snap.hit_breakpoints)
            out.append(f"[bold red]Breakpoint {nums}[/bold red], " + self._where(snap))
        elif snap.stop_reason == "watchpoint":
            nums = ", ".join(str(n) for n in snap.hit_breakpoints) or "?"
            detail = f" {_escape(snap.annotation)}" if snap.annotation else ""
            out.append(f"[bold yellow]Watchpoint {nums}[/bold yellow]{detail}")
            out.append("  " + self._where(snap))
        elif snap.stop_reason == "error":
            out.append(f"[bold red]Stopped on error:[/bold red] {snap.annotation}")
            out.append("  " + self._where(snap))
        else:
            out.append(self._where(snap))
        if snap.has_source:
            lines = self._source_lines(snap.source_key)
            if 0 < snap.line <= len(lines):
                out.append(f"[dim]{snap.line:>4}[/dim]  {_escape(lines[snap.line - 1])}")
        else:
            out.append(
                f"[dim]  no source for this contract; "
                f"pc=0x{snap.pc:04x} {snap.mnemonic}[/dim]"
            )
        return out

    def _where(self, snap: FrameSnapshot) -> str:
        fn = snap.function.signature if snap.function else (snap.contract_name or "?")
        if snap.has_source:
            return f"[bold cyan]{fn}[/bold cyan] at [green]{snap.source_key}:{snap.line}[/green]"
        return f"[bold cyan]{fn}[/bold cyan] at [green]pc 0x{snap.pc:04x}[/green]"

    def _render_displays(self) -> list[str]:
        out: list[str] = []
        for num, expr in self.displays:
            try:
                res = self._eval(expr)
                out.append(
                    f"[dim]{num}:[/dim] {_escape(expr)} = [bold]{_escape(res.display)}[/bold]"
                )
            except Exception as exc:
                out.append(f"[dim]{num}:[/dim] {_escape(expr)} = [red]<{exc}>[/red]")
        return out

    # -- expression evaluation ---------------------------------------------

    def _substitute(self, expression: str) -> str:
        """Resolve gdb convenience variables into Solidity literals.

        `$pc`, `$gas`, `$stack[0]`, `$mem[0x40]`, `$storage[1]` and value history `$1`
        are all read straight off the paused VM, so they work with no source at all and
        can be mixed into a Solidity expression: `p $storage[1] + 1 ether`.
        """
        snap = self._require_stop()

        def replace(match: re.Match) -> str:
            token = match.group(1)
            if token == "pc":
                return str(snap.pc)
            if token == "gas":
                return str(snap.gas_remaining)
            if token == "gasused":
                return str(snap.gas_used)
            if token == "depth":
                return str(snap.depth)
            if token == "step":
                return str(snap.step)
            if token == "sp":
                return str(len(snap.stack))
            if match.group(2) is not None:
                index = int(match.group(2))
                if index >= len(snap.stack):
                    raise EvalError(f"stack has only {len(snap.stack)} items")
                return str(snap.stack[index].value)
            if match.group(3) is not None:
                offset = int(match.group(3), 0)
                data = self._inspect("read_memory", offset, 32)
                return str(int.from_bytes(data, "big"))
            if match.group(4) is not None:
                slot = int(match.group(4), 0)
                return str(self._inspect("read_storage", slot))
            if match.group(5) is not None:
                index = int(match.group(5))
                if not 1 <= index <= len(self.history):
                    raise EvalError(f"no value history entry ${index}")
                past = self.history[index - 1]
                if past.abi_type == "address":
                    return f"address({past.value})"
                if isinstance(past.value, bool):
                    return "true" if past.value else "false"
                if isinstance(past.value, bytes):
                    return f"bytes32(0x{past.value.hex()})"
                return str(past.value)
            return match.group(0)

        return _CONVENIENCE.sub(replace, expression)

    def _eval(self, expression: str, keep: bool = False) -> EvalResult:
        return self._inspect(
            "evaluate",
            self._substitute(expression),
            keep=keep,
            internal_index=self.selected_internal,
        )

    def _locals(self) -> list[dict]:
        return self._inspect("locals", internal_index=self.selected_internal)

    def _is_local(self, name: str) -> bool:
        try:
            return any(row["name"] == name for row in self._locals())
        except Exception:
            return False

    # ==================================================================
    # execution commands
    # ==================================================================

    def cmd_continue(self, args: list[str], rest: str) -> CommandResult:
        return self._resume(StepMode.RUN)

    def cmd_next(self, args: list[str], rest: str) -> CommandResult:
        return self._resume(StepMode.NEXT, _count(args))

    def cmd_step(self, args: list[str], rest: str) -> CommandResult:
        return self._resume(StepMode.STEP, _count(args))

    def cmd_stepi(self, args: list[str], rest: str) -> CommandResult:
        return self._resume(StepMode.STEPI, _count(args))

    def cmd_nexti(self, args: list[str], rest: str) -> CommandResult:
        return self._resume(StepMode.NEXTI, _count(args))

    def cmd_finish(self, args: list[str], rest: str) -> CommandResult:
        return self._resume(StepMode.FINISH)

    def cmd_until(self, args: list[str], rest: str) -> CommandResult:
        if not args:
            return self._resume(StepMode.NEXT)
        target = args[0]
        snap = self._require_stop()
        if target.startswith("*"):
            pc = int(target[1:], 0)
        else:
            source_key, line = self._parse_location(target, snap)
            file_id = self.session.file_id_for(source_key)
            if file_id is None:
                return CommandResult(error=f"no source file matching {source_key!r}")
            _snapped, pcs = self.session.resolve_line(file_id, line)
            if not pcs:
                return CommandResult(error=f"no code at {source_key}:{line}")
            pc = min(pcs)
        return self._resume(StepMode.UNTIL, target_pc=pc)

    # ==================================================================
    # breakpoint commands
    # ==================================================================

    def _parse_location(self, spec: str, snap: FrameSnapshot | None) -> tuple[str, int]:
        if ":" in spec:
            source_key, line_text = spec.rsplit(":", 1)
            return source_key, int(line_text)
        if spec.isdigit():
            key = (
                snap.source_key
                if snap and snap.source_key
                else next(iter(self.project.sources))
            )
            return key, int(spec)
        raise SessionError(f"cannot parse location {spec!r}")

    def _make_break(self, args: list[str], rest: str, temporary: bool) -> CommandResult:
        condition = None
        if " if " in rest:
            rest, condition = rest.split(" if ", 1)
            condition = condition.strip()
            args = rest.split()
        result = CommandResult()
        if not args:
            snap = self._require_stop()
            if not snap.has_source:
                return CommandResult(error="no source here; use `break *0xPC`")
            bp, line = self.session.break_at_line(
                snap.source_key, snap.line, temporary=temporary, condition=condition
            )
            return result.add(f"Breakpoint {bp.number} at {snap.source_key}:{line}")

        spec = args[0]
        if spec.startswith("*"):
            bp = self.session.break_at_pc(
                int(spec[1:], 0), temporary=temporary, condition=condition
            )
            return result.add(f"Breakpoint {bp.number} at pc {spec[1:]}")
        if spec.upper() in _known_opcodes():
            bp = self.session.break_at_opcode(
                spec.upper(), temporary=temporary, condition=condition
            )
            return result.add(f"Breakpoint {bp.number} on every {spec.upper()}")
        if ":" in spec or spec.isdigit():
            source_key, line = self._parse_location(spec, self.snapshot)
            bp, snapped = self.session.break_at_line(
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
        bp, line = self.session.break_at_function(
            spec, temporary=temporary, condition=condition
        )
        return result.add(f"Breakpoint {bp.number} at {bp.location} (line {line})")

    def cmd_break(self, args: list[str], rest: str) -> CommandResult:
        return self._make_break(args, rest, temporary=False)

    def cmd_tbreak(self, args: list[str], rest: str) -> CommandResult:
        return self._make_break(args, rest, temporary=True)

    def cmd_delete(self, args: list[str], rest: str) -> CommandResult:
        if not args:
            self.session.breakpoints.clear()
            return CommandResult().add("all breakpoints deleted")
        numbers = _breakpoint_numbers(args, "delete")
        removed = [n for n in numbers if self.session.breakpoints.remove(n)]
        if not removed:
            return CommandResult(error="no such breakpoint")
        return CommandResult().add(f"deleted {', '.join(str(n) for n in removed)}")

    def cmd_disable(self, args: list[str], rest: str) -> CommandResult:
        n = _breakpoint_numbers(args, "disable")[0] if args else None
        count = self.session.breakpoints.set_enabled(n, False)
        return CommandResult().add(f"disabled {count}")

    def cmd_enable(self, args: list[str], rest: str) -> CommandResult:
        n = _breakpoint_numbers(args, "enable")[0] if args else None
        count = self.session.breakpoints.set_enabled(n, True)
        return CommandResult().add(f"enabled {count}")

    def cmd_watch(self, args: list[str], rest: str) -> CommandResult:
        """Break when a storage value changes."""
        return self._watch(rest, WATCH_WRITE)

    def cmd_rwatch(self, args: list[str], rest: str) -> CommandResult:
        """Break when a storage slot is READ (an SLOAD of it)."""
        return self._watch(rest, WATCH_READ)

    def cmd_awatch(self, args: list[str], rest: str) -> CommandResult:
        """Break on either a read or a write."""
        return self._watch(rest, WATCH_ACCESS)

    def _watch(self, rest: str, mode: str) -> CommandResult:
        """`watch <state var or mapping element>` or `watch *0xOFFSET` for memory."""
        snap = self._require_stop()
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
            wp = self.session.watch_memory(expr, int(expr[1:], 0))
            return CommandResult().add(f"Watchpoint {wp.number}: memory at {expr[1:]}")
        slot = self._slot_of(expr, snap)
        if slot is None:
            return CommandResult(
                error=f"cannot resolve {expr!r} to a storage slot; "
                "watch a state variable, a mapping element, or *0xOFFSET for memory"
            )
        wp = self.session.watch_storage(expr, slot, address=snap.address, mode=mode)
        return CommandResult().add(
            f"{verb} {wp.number}: {expr} (slot 0x{slot:x} of {_short(snap.address)})"
        )

    def _slot_of(self, expr: str, snap: FrameSnapshot) -> int | None:
        """Resolve a Solidity lvalue to its storage slot.

        State variables come straight from `storageLayout`. Mapping and array elements
        use the layout rules (keccak256(key . slot) and keccak256(slot) + i), with the
        key itself evaluated through solc so `balances[msg.sender]` works.
        """
        decoder = self._decoder(snap.contract_name)
        if decoder is None:
            return None
        base = expr.split("[")[0].split(".")[0].strip()
        var = decoder.get(base)
        if var is None:
            return None
        if "[" not in expr and "." not in expr:
            return var.slot
        # mapping/array element: compute the slot with the same helper solc would.
        from .decode import dynamic_array_slot, mapping_slot

        match = re.match(r"^\w+\[(.+?)\]$", expr.strip())
        if not match:
            return None
        key_expr = match.group(1)
        type_info = decoder.types.get(var.type_id, {})
        try:
            key_value = self._eval(key_expr).value
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

    def cmd_print(self, args: list[str], rest: str) -> CommandResult:
        expr = rest.strip()
        if not expr:
            return CommandResult(error="usage: print <solidity expression>")
        res = self._eval(expr)
        self.history.append(res)
        idx = len(self.history)
        return CommandResult().add(
            f"[bold]${idx}[/bold] = {_escape(res.display)}  [dim]({res.type_name})[/dim]"
        )

    def cmd_call(self, args: list[str], rest: str) -> CommandResult:
        """gdb's `call`: evaluate and KEEP the side effects."""
        expr = rest.strip()
        if not expr:
            return CommandResult(error="usage: call <solidity expression>")
        res = self._eval(expr, keep=True)
        if res.void:
            return CommandResult(mutated=True).add(
                f"[green]done[/green] [dim](gas {res.gas_used})[/dim]"
            )
        self.history.append(res)
        return CommandResult(mutated=True).add(
            f"[bold]${len(self.history)}[/bold] = {_escape(res.display)}  [dim]({res.type_name})[/dim]"
        )

    def cmd_ptype(self, args: list[str], rest: str) -> CommandResult:
        expr = rest.strip()
        if not expr:
            return CommandResult(error="usage: ptype <expression>")
        type_name = self._inspect(
            "evaluate", self._substitute(expr), internal_index=self.selected_internal
        ).type_name
        return CommandResult().add(f"type = [bold]{type_name}[/bold]")

    def cmd_display(self, args: list[str], rest: str) -> CommandResult:
        expr = rest.strip()
        if not expr:
            return CommandResult(lines=self._render_displays())
        self._display_counter += 1
        self.displays.append((self._display_counter, expr))
        return CommandResult(lines=self._render_displays()[-1:])

    def cmd_undisplay(self, args: list[str], rest: str) -> CommandResult:
        if not args:
            self.displays.clear()
            return CommandResult().add("all displays removed")
        targets = set(_breakpoint_numbers(args, "undisplay"))
        self.displays = [(n, e) for n, e in self.displays if n not in targets]
        return CommandResult().add("removed")

    def cmd_examine(self, args: list[str], rest: str) -> CommandResult:
        """gdb's `x/NFU addr` over EVM memory."""
        snap = self._require_stop()
        spec, addr_text = "/8xg", ""
        tokens = rest.strip().split(None, 1)
        if tokens and tokens[0].startswith("/"):
            spec = tokens[0]
            addr_text = tokens[1] if len(tokens) > 1 else "0"
        elif tokens:
            addr_text = rest.strip()
        match = _X_FORMAT.match(spec)
        if not match:
            return CommandResult(error=f"bad format {spec!r}; try x/32xb 0x40")
        count = int(match.group(1) or 8)
        fmt = match.group(2) or "x"
        unit = _UNIT_SIZES.get(match.group(3) or "g", 8)

        try:
            offset = int(self._eval(addr_text or "0").value) if addr_text else 0
        except Exception:
            offset = int(addr_text, 0) if addr_text else 0

        total = count * unit
        data = self._inspect("read_memory", offset, total)
        result = CommandResult()
        if fmt == "s":
            text = bytes(data).split(b"\x00", 1)[0].decode("utf-8", "replace")
            return result.add(f'0x{offset:04x}: "{_escape(text)}"')
        per_row = max(1, min(count, 32 // unit if unit <= 32 else 1))
        if unit == 1:
            per_row = 16
        for row_start in range(0, total, per_row * unit):
            chunk = data[row_start : row_start + per_row * unit]
            cells = []
            for i in range(0, len(chunk), unit):
                word = chunk[i : i + unit]
                value = int.from_bytes(word, "big")
                if fmt == "d":
                    cells.append(str(int.from_bytes(word, "big", signed=True)))
                elif fmt in ("u", "o", "t"):
                    cells.append({"u": str(value), "o": oct(value), "t": bin(value)}[fmt])
                elif fmt == "c":
                    cells.append("".join(chr(b) if 32 <= b < 127 else "." for b in word))
                else:
                    cells.append(f"{value:0{unit * 2}x}")
            annotation = _memory_region(offset + row_start)
            result.add(
                f"[cyan]0x{offset + row_start:04x}[/cyan]: {' '.join(cells)}"
                + (f"  [dim]{annotation}[/dim]" if annotation else "")
            )
        if offset + total > snap.memory_size:
            result.add(
                f"[dim](memory is {snap.memory_size} bytes; the rest reads as zero)[/dim]"
            )
        return result

    def cmd_backtrace(self, args: list[str], rest: str) -> CommandResult:
        snap = self._require_stop()
        result = CommandResult()
        for row in snap.backtrace:
            marker = (
                "[bold yellow]->[/bold yellow]"
                if row.index == self.selected_row
                else "  "
            )
            colour = "cyan" if row.kind == "solidity" else "magenta"
            key = row.source_key or snap.source_key or "?"
            where = f"{key}:{row.line}" if row.line else f"pc 0x{row.pc:x}"
            detail = f" [dim][{row.detail}][/dim]" if row.detail else ""
            result.add(
                f"{marker} [dim]#{row.index}[/dim] [{colour}]{_escape(row.name)}[/{colour}] at [green]{where}[/green]{detail}"
            )
        return result

    def cmd_frame(self, args: list[str], rest: str) -> CommandResult:
        snap = self._require_stop()
        if not args:
            return CommandResult(lines=self.describe_stop(snap))
        if not args[0].lstrip("-").isdigit():
            raise SessionError(f"frame takes a number, not {args[0]!r}; see `bt`")
        index = int(args[0])
        rows = snap.backtrace
        if not 0 <= index < len(rows):
            return CommandResult(error=f"no frame #{index}")
        self.selected_row = index
        self.selected_frame = rows[index].evm_index
        self.selected_internal = (
            rows[index].internal_index if rows[index].internal_index >= 0 else None
        )
        row = rows[index]
        where = (
            f"{row.source_key or snap.source_key}:{row.line}"
            if row.line
            else f"pc 0x{row.pc:x}"
        )
        return CommandResult().add(
            f"#{row.index}  [cyan]{_escape(row.name)}[/cyan] at [green]{where}[/green]"
        )

    def cmd_up(self, args: list[str], rest: str) -> CommandResult:
        return self._move_frame(+1)

    def cmd_down(self, args: list[str], rest: str) -> CommandResult:
        return self._move_frame(-1)

    def _move_frame(self, delta: int) -> CommandResult:
        """`up` moves toward the caller, `down` toward the callee, as in gdb."""
        snap = self._require_stop()
        rows = snap.backtrace
        target = self.selected_row + delta
        if not 0 <= target < len(rows):
            return CommandResult(
                error="already at the outermost frame"
                if delta > 0
                else "already at the innermost frame"
            )
        return self.cmd_frame([str(target)], str(target))

    def cmd_list(self, args: list[str], rest: str) -> CommandResult:
        snap = self.snapshot
        source_key = (
            snap.source_key
            if snap and snap.source_key
            else next(iter(self.project.sources), None)
        )
        centre = snap.line if snap and snap.line else 1
        if args:
            if ":" in args[0]:
                source_key, line_text = args[0].rsplit(":", 1)
                centre = int(line_text)
            elif args[0].isdigit():
                centre = int(args[0])
        lines = self._source_lines(source_key)
        if not lines:
            return CommandResult(error=f"no source for {source_key}")
        start = max(1, centre - 5)
        end = min(len(lines), start + 10)
        result = CommandResult().add(f"[dim]{source_key}[/dim]")
        exec_lines = self._executable_lines(snap)
        for n in range(start, end + 1):
            here = snap is not None and n == snap.line
            gutter = (
                "[bold yellow]->[/bold yellow]"
                if here
                else ("[dim] .[/dim]" if n in exec_lines else "  ")
            )
            body = _escape(lines[n - 1])
            result.add(
                f"{gutter} [dim]{n:>4}[/dim]  "
                + (f"[bold]{body}[/bold]" if here else body)
            )
        self.last_list_line = end
        return result

    def _executable_lines(self, snap: FrameSnapshot | None) -> set:
        if snap is None or not snap.contract_name:
            return set()
        art = self.project.artifact(snap.contract_name)
        if art is None or not art.deployed_source_map:
            return set()
        from .srcmap import PcMap

        pcmap = PcMap(
            art.deployed_bytecode, art.deployed_source_map, self.session.line_indexes
        )
        return set(pcmap.executable_lines(snap.file_id))

    def cmd_disassemble(self, args: list[str], rest: str) -> CommandResult:
        self._require_stop()
        rows = self._inspect("disassembly", 8, 20)
        result = CommandResult()
        for row in rows:
            marker = "[bold yellow]=>[/bold yellow]" if row["current"] else "  "
            label = "[magenta]" if row["jumpdest"] else ""
            close = "[/magenta]" if row["jumpdest"] else ""
            line = f" [dim]L{row['line']}[/dim]" if row["line"] else ""
            result.add(
                f"{marker} [cyan]{row['pc']:04x}[/cyan]  {label}{row['text']}{close}{line}"
            )
        return result

    def cmd_jump(self, args: list[str], rest: str) -> CommandResult:
        if not args:
            return CommandResult(error="usage: jump <pc>")
        pc = _integer(args[0], "jump")
        self._inspect("set_pc", pc)
        return CommandResult(mutated=True).add(
            f"[yellow]program counter set to 0x{pc:x}[/yellow]"
        )

    # ==================================================================
    # info subcommands
    # ==================================================================

    def cmd_info(self, args: list[str], rest: str) -> CommandResult:
        if not args:
            return CommandResult(
                error="usage: info <registers|breakpoints|frame|args|locals|storage|gas|logs|sources|functions|watchpoints>"
            )
        topic = args[0]
        table = {
            "registers": self._info_registers,
            "r": self._info_registers,
            "reg": self._info_registers,
            "breakpoints": self._info_breakpoints,
            "b": self._info_breakpoints,
            "break": self._info_breakpoints,
            "watchpoints": self._info_breakpoints,
            "frame": self._info_frame,
            "args": self._info_args,
            "locals": self._info_locals,
            "storage": self._info_storage,
            "gas": self._info_gas,
            "logs": self._info_logs,
            "sources": self._info_sources,
            "functions": self._info_functions,
        }
        handler = table.get(topic)
        if handler is None:
            return CommandResult(error=f"unknown info topic {topic!r}")
        return handler(args[1:])

    def _info_registers(self, args: list[str]) -> CommandResult:
        snap = self._require_stop()
        result = CommandResult()
        rows = [
            ("pc", f"0x{snap.pc:04x}"),
            ("opcode", f"{snap.mnemonic} (0x{snap.opcode:02x})"),
            (
                "gas",
                f"{snap.gas_remaining:,} remaining / {snap.gas_used:,} used of {snap.gas_limit:,}",
            ),
            ("depth", str(snap.depth)),
            ("sp", f"{len(snap.stack)} items"),
            ("address", _addr(snap.address)),
            ("code", _addr(snap.code_address)),
            ("msg.sender", _addr(snap.sender)),
            ("msg.value", _wei(snap.value)),
            ("tx.origin", _addr(snap.origin)),
            ("static", "yes" if snap.is_static else "no"),
            ("step", str(snap.step)),
        ]
        for name, value in rows:
            result.add(f"[cyan]{name:<12}[/cyan] {value}")
        return result

    def _info_breakpoints(self, args: list[str]) -> CommandResult:
        rows = self.session.breakpoints.listing()
        if not rows:
            return CommandResult().add("[dim]no breakpoints or watchpoints[/dim]")
        result = CommandResult().add("[dim]Num Type     What[/dim]")
        for row in rows:
            result.add(_escape(row))
        return result

    def _info_frame(self, args: list[str]) -> CommandResult:
        info = self._inspect("frame_info")
        result = CommandResult()
        for key in ("depth", "kind", "artifact", "is_static", "gas_remaining"):
            result.add(f"[cyan]{key:<14}[/cyan] {info[key]}")
        if self.session.estimations:
            result.add(
                f"[cyan]{'estimates':<14}[/cyan] {self.session.estimations} gas-estimation "
                "pass(es) skipped [dim](they re-run the tx; not debugged)[/dim]"
            )
        for key in ("address", "code_address", "sender"):
            result.add(f"[cyan]{key:<14}[/cyan] {_addr(info[key])}")
        result.add(f"[cyan]{'value':<14}[/cyan] {_wei(info['value'])}")
        result.add(
            f"[cyan]{'calldata':<14}[/cyan] 0x{bytes(info['calldata']).hex()[:128]}"
        )
        if info["internal"]:
            result.add(
                f"[cyan]{'internal':<14}[/cyan] "
                + " <- ".join(reversed(info["internal"]))
            )
        return result

    def _info_args(self, args: list[str]) -> CommandResult:
        snap = self._require_stop()
        params = [row for row in self._locals() if row["kind"] == "param"]
        if params:
            # The arguments of the frame we are *in*, which for an internal call is not
            # what calldata holds.
            rows = snap.backtrace
            name = (
                rows[self.selected_row].name if 0 <= self.selected_row < len(rows) else ""
            )
            result = CommandResult().add(f"[bold cyan]{_escape(name)}[/bold cyan]")
            for row in params:
                body = (
                    f"[bold]{_escape(str(row['value']))}[/bold]"
                    if row["available"]
                    else f"[dim]{_escape(str(row['value']))}[/dim]"
                )
                result.add(
                    f"  [cyan]{row['name']:<14}[/cyan] [dim]{row['type']:<18}[/dim] = {body}"
                )
            return result
        info = self._inspect("frame_info")
        art = self.project.artifact(snap.contract_name) if snap.contract_name else None
        if art is None:
            return CommandResult().add("[dim]no ABI for this frame[/dim]")
        decoded = decode_calldata(art.abi, bytes(info["calldata"]))
        if decoded is None:
            return CommandResult().add(
                "[dim]calldata does not match any function in the ABI[/dim]"
            )
        signature, params = decoded
        result = CommandResult().add(f"[bold cyan]{signature}[/bold cyan]")
        if not params:
            result.add("[dim](no arguments)[/dim]")
        for type_name, name, value in params:
            shown = (
                _addr(value) if isinstance(value, bytes) and len(value) == 20 else value
            )
            result.add(
                f"  [cyan]{name or '_':<14}[/cyan] [dim]{type_name}[/dim] = {_escape(str(shown))}"
            )
        if (
            snap.backtrace
            and snap.backtrace[0].kind == "solidity"
            and snap.function
            and snap.function.name not in signature
        ):
            result.add(
                "[dim]note: these are the arguments of the external call, not of the "
                "internal function you are stopped in[/dim]"
            )
        return result

    def _info_locals(self, args: list[str]) -> CommandResult:
        snap = self._require_stop()
        rows = self._locals()
        result = CommandResult()
        if not rows:
            return result.add(f"[dim]{self._no_locals_reason(snap)}[/dim]")
        for row in rows:
            name = row["name"]
            kind = "" if row["kind"] == "local" else f" [dim]({row['kind']})[/dim]"
            value = row["value"]
            body = (
                f"[bold]{_escape(str(value))}[/bold]"
                if row["available"]
                else f"[dim]{_escape(str(value))}[/dim]"
            )
            note = f"  [dim]{_escape(row['reason'])}[/dim]" if row["reason"] else ""
            result.add(
                f"  [cyan]{name:<14}[/cyan] [dim]{row['type']:<18}[/dim] = {body}{kind}{note}"
            )
        return result

    def _no_locals_reason(self, snap: FrameSnapshot) -> str:
        """gdb prints "No symbol table info available"; say which of the two it is."""
        rows = snap.backtrace
        if 0 <= self.selected_row < len(rows):
            row = rows[self.selected_row]
            if row.kind == "solidity" and "compiler-generated" not in row.detail:
                return f"no locals in {row.name}"
        return (
            "no locals here: this frame has no Solidity source, or execution is in "
            "compiler-generated code"
        )

    def _info_storage(self, args: list[str]) -> CommandResult:
        snap = self._require_stop()
        contract = args[0] if args else snap.contract_name
        decoder = self._decoder(contract)
        if not decoder:
            return CommandResult().add("[dim]no storage layout for this contract[/dim]")
        reader = lambda slot: self._inspect("read_storage", slot)  # noqa: E731
        result = CommandResult().add(f"[dim]{contract} at {_addr(snap.address)}[/dim]")
        for var, value in decoder.read_all(reader):
            warm = ""
            with contextlib.suppress(Exception):
                warm = (
                    " [dim](warm)[/dim]"
                    if self._inspect("is_warm", var.slot)
                    else " [dim](cold)[/dim]"
                )
            result.add(
                f"  [dim]slot {var.slot:>3}+{var.offset:<2}[/dim] "
                f"[cyan]{var.name:<16}[/cyan] [dim]{var.type_label:<22}[/dim] "
                f"= [bold]{_escape(value.display[:60])}[/bold]{warm}"
            )
        return result

    def _info_gas(self, args: list[str]) -> CommandResult:
        snap = self._require_stop()
        result = CommandResult()
        result.add(f"[cyan]{'limit':<12}[/cyan] {snap.gas_limit:,}")
        result.add(f"[cyan]{'used':<12}[/cyan] {snap.gas_used:,}")
        result.add(f"[cyan]{'remaining':<12}[/cyan] {snap.gas_remaining:,}")
        result.add(f"[cyan]{'refund':<12}[/cyan] {snap.gas_refund:,}")
        if snap.static_gas is not None:
            result.add(
                f"[cyan]{'this op':<12}[/cyan] {snap.mnemonic} base cost {snap.static_gas}"
            )
        by_line = sorted(self.session.gas_by_line.items(), key=lambda kv: -kv[1])[:12]
        if by_line:
            result.add("")
            result.add("[dim]gas by source line (highest first)[/dim]")
            lines = self._source_lines(snap.source_key)
            for (_file_id, line), spent in by_line:
                text = lines[line - 1].strip()[:46] if 0 < line <= len(lines) else ""
                result.add(
                    f"  [magenta]{spent:>9,}[/magenta] [dim]L{line:<4}[/dim] {_escape(text)}"
                )
        by_op = sorted(self.session.gas_by_opcode.items(), key=lambda kv: -kv[1])[:8]
        if by_op:
            result.add("")
            result.add("[dim]gas by opcode[/dim]")
            for name, spent in by_op:
                result.add(f"  [magenta]{spent:>9,}[/magenta] {name}")
        return result

    def _info_logs(self, args: list[str]) -> CommandResult:
        snap = self._require_stop()
        entries = self._inspect("logs")
        if not entries:
            return CommandResult().add("[dim]no events emitted yet in this frame[/dim]")
        art = self.project.artifact(snap.contract_name) if snap.contract_name else None
        result = CommandResult()
        for address, topics, data in entries:
            name = _event_name(art.abi if art else [], topics)
            result.add(f"[cyan]{name}[/cyan] [dim]from {_short(address)}[/dim]")
            for i, topic in enumerate(topics):
                result.add(f"    [dim]topic{i}[/dim] 0x{topic:064x}")
            if data:
                result.add(f"    [dim]data  [/dim] 0x{bytes(data).hex()}")
        return result

    def _info_sources(self, args: list[str]) -> CommandResult:
        result = CommandResult()
        for key, src in self.project.sources.items():
            result.add(
                f"[cyan]{key}[/cyan] [dim]({len(src.text.splitlines())} lines, id {src.file_id})[/dim]"
            )
        return result

    def _info_functions(self, args: list[str]) -> CommandResult:
        pattern = args[0] if args else None
        result = CommandResult()
        for fn in self.session.functions.functions:
            if fn.kind == "modifier":
                continue
            if pattern and pattern not in fn.display_name:
                continue
            index = self.session.line_indexes.get(fn.file_id)
            line = index.line_col(fn.start)[0] if index else 0
            visibility = f"[dim]{fn.visibility}[/dim] " if fn.visibility else ""
            result.add(
                f"{visibility}[cyan]{_escape(fn.signature)}[/cyan] [dim]line {line}[/dim]"
            )
        return result

    # ==================================================================
    # mutation
    # ==================================================================

    def cmd_set(self, args: list[str], rest: str) -> CommandResult:
        """`set var <lvalue> = <value>` plus the low-level `set $pc/$gas/$stack[n]`."""
        body = rest.strip()
        if body.startswith("var "):
            body = body[4:].strip()
        if "=" not in body:
            return CommandResult(error="usage: set var <lvalue> = <expression>")
        lhs, rhs = body.split("=", 1)
        lhs, rhs = lhs.strip(), rhs.strip()

        if lhs.startswith("$"):
            return self._set_convenience(lhs, rhs)

        # A bare local name is a stack slot, not storage, and has to be written as one.
        if re.fullmatch(r"[A-Za-z_$][\w$]*", lhs) and self._is_local(lhs):
            value = int(self._eval(rhs).value)
            written = self._inspect(
                "write_local", lhs, value, internal_index=self.selected_internal
            )
            return CommandResult(mutated=True).add(
                f"[yellow]{_escape(lhs)}[/yellow] = {_escape(written['display'])}"
            )

        # Everything else goes through solc, so packed slots, mappings, structs and
        # arrays are all written correctly without reimplementing the layout rules.
        res = self._eval(f"{lhs} = ({rhs})", keep=True)
        try:
            now = self._eval(lhs)
            return CommandResult(mutated=True).add(
                f"[yellow]{_escape(lhs)}[/yellow] = {_escape(now.display)}"
            )
        except Exception:
            return CommandResult(mutated=True).add(
                f"[yellow]{_escape(lhs)}[/yellow] set  [dim](gas {res.gas_used})[/dim]"
            )

    def _set_convenience(self, lhs: str, rhs: str) -> CommandResult:
        value = int(self._eval(rhs).value)
        name = lhs[1:]
        if name == "pc":
            self._inspect("set_pc", value)
            return CommandResult(mutated=True).add(f"[yellow]$pc = 0x{value:x}[/yellow]")
        if name == "gas":
            self._inspect("set_gas", value)
            return CommandResult(mutated=True).add(f"[yellow]$gas = {value:,}[/yellow]")
        match = re.match(r"^stack\[(\d+)\]$", name)
        if match:
            index = int(match.group(1))
            self._inspect("write_stack", index, value)
            return CommandResult(mutated=True).add(
                f"[yellow]$stack[{index}] = 0x{value:x}[/yellow]"
            )
        match = re.match(r"^mem\[(0x[0-9a-fA-F]+|\d+)\]$", name)
        if match:
            offset = int(match.group(1), 0)
            self._inspect("write_memory", offset, value.to_bytes(32, "big"))
            return CommandResult(mutated=True).add(
                f"[yellow]memory[0x{offset:x}] = 0x{value:x}[/yellow]"
            )
        match = re.match(r"^storage\[(0x[0-9a-fA-F]+|\d+)\]$", name)
        if match:
            slot = int(match.group(1), 0)
            self._inspect("write_storage", slot, value)
            return CommandResult(mutated=True).add(
                f"[yellow]storage[0x{slot:x}] = 0x{value:x}[/yellow]"
            )
        return CommandResult(error=f"unknown convenience variable {lhs}")

    # ==================================================================
    # misc
    # ==================================================================

    def cmd_copy(self, args: list[str], rest: str) -> CommandResult:
        """`copy [command]`: put a command's output on the system clipboard.

        Mouse selection under tmux needs shift-held or a pipe config. This sidesteps it:
        `copy p balances[alice]` runs the command and puts plain text where Cmd+V finds
        it, nothing truncated to a pane width.
        """
        target = rest.strip()
        if target:
            result = self.execute(target)
            if result.error:
                return result
            lines = list(result.lines)
        elif self._last_output:
            lines = list(self._last_output)
        else:
            return CommandResult(error="nothing to copy yet; try `copy p owner`")

        # Strip trailing space per line but keep leading indentation, so a table pasted
        # into a report still lines up.
        plain = [_plain(line).rstrip() for line in lines]
        while plain and not plain[0].strip():
            plain.pop(0)
        while plain and not plain[-1].strip():
            plain.pop()
        text = "\n".join(plain)
        if not text:
            return CommandResult(error="that produced no text to copy")
        try:
            tool = clipboard.copy(text)
        except clipboard.ClipboardError as exc:
            return CommandResult(error=str(exc))
        return CommandResult(
            notice=f"copied {describe_amount(text)} to the clipboard ({tool})"
        )

    def cmd_quit(self, args: list[str], rest: str) -> CommandResult:
        return CommandResult(quit=True)

    def cmd_help(self, args: list[str], rest: str) -> CommandResult:
        if args:
            topic = args[0]
            body = HELP_TOPICS.get(topic)
            if body:
                return CommandResult(lines=body.strip().split("\n"))
            return CommandResult(error=f"no help for {topic!r}")
        return CommandResult(lines=HELP_SUMMARY.strip().split("\n"))


# ==================================================================
# static helpers
# ==================================================================


_ESCAPE = re.compile(r"\[(/?[a-zA-Z#][^\]]*)\]")


def describe_amount(text: str) -> str:
    """ "20 characters" or "3 lines", whichever describes the copy better."""
    lines = text.count("\n") + 1
    return f"{lines} lines" if lines > 1 else f"{len(text)} characters"


def _plain(markup: str) -> str:
    """Strip console markup so the clipboard gets text, not `[bold]tags[/bold]`."""
    try:
        return Text.from_markup(markup).plain
    except Exception:
        return markup


def _escape(text: str) -> str:
    """Keep user text and source code from being read as Rich markup."""
    return _ESCAPE.sub(lambda m: "\\[" + m.group(1) + "]", str(text))


def escape_markup(text: str) -> str:
    """Escape *every* bracket, for strings that are wholly the user's.

    `_escape` above is selective because it protects markup we built with user text
    spliced in. An error or notice is the whole string as quoted input, and a stray
    `[/]` in it is a MarkupError waiting to escape a frontend's render loop.
    """
    return rich_escape(str(text))


def _count(args: list[str]) -> int:
    if args and args[0].isdigit():
        return int(args[0])
    return 1


def _integer(text: str, verb: str) -> int:
    """Parse a decimal or 0x argument, or say which word was not a number.

    `int(text, 0)` raises `invalid literal for int() with base 10`, which names neither
    the command the user typed nor what it wanted instead.
    """
    try:
        return int(text, 0)
    except ValueError as exc:
        raise SessionError(f"{verb}: {text!r} is not a number") from exc


def _breakpoint_numbers(args: list[str], verb: str) -> list[int]:
    return [_integer(arg, verb) for arg in args]


def _word(value: int) -> str:
    """A 256-bit result, in hex plus decimal while decimal still means anything."""
    text = f"0x{value:x}"
    if value < 2**64:
        return f"{text} ({value:,})"
    return text


# A line with no operators, brackets or dots: it reads as a verb and its arguments, not as
# an expression. `bt`, `brekapoint 12` and `nonsense` match; `a + b`, `p.x` and `m[k]` do not.
_COMMAND_SHAPED = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(\s+[A-Za-z0-9_:*$/.\[\]-]+)*")

# `name(...)`, which at this prompt is as likely to be a Yul builtin as a Solidity call.
_CALL_SHAPED = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*\s*\(")


def _looks_like_a_command(line: str) -> bool:
    return _COMMAND_SHAPED.fullmatch(line.strip()) is not None


def _did_you_mean(verb: str, verbs: dict) -> str:
    """The nearest real verbs, gdb-style, or "" when nothing is close enough.

    One- and two-letter aliases are excluded: they match almost anything short, so
    suggesting `b` for `bkr` is noise. Ties on edit distance break by shared prefix,
    which puts `break` ahead of `print` for `brekpoint` (same score, one matches the
    typo's start).
    """
    candidates = [name for name in verbs if len(name) > 2]
    matches = difflib.get_close_matches(verb, candidates, n=4, cutoff=0.55)
    if not matches:
        return ""
    ranked = sorted(
        matches,
        key=lambda m: (
            -difflib.SequenceMatcher(None, verb, m).ratio(),
            -_shared_prefix(verb, m),
        ),
    )[:2]
    return " Did you mean " + ", ".join(f"`{m}`" for m in ranked) + "?"


def _shared_prefix(a: str, b: str) -> int:
    count = 0
    for left, right in zip(a, b, strict=False):
        if left != right:
            break
        count += 1
    return count


_OPCODE_NAMES: frozenset | None = None


def _known_opcodes() -> frozenset:
    global _OPCODE_NAMES
    if _OPCODE_NAMES is None:
        from .disasm import OPCODES

        _OPCODE_NAMES = frozenset(OPCODES.values())
    return _OPCODE_NAMES


def _memory_region(offset: int) -> str:
    """Solidity's fixed memory layout, annotated so beginners can orient themselves."""
    if offset < 0x40:
        return "scratch space"
    if offset < 0x60:
        return "free memory pointer"
    if offset < 0x80:
        return "zero slot"
    return ""


def _event_name(abi: Sequence[dict], topics: Sequence[int]) -> str:
    if not topics:
        return "<anonymous event>"
    from eth_utils import event_abi_to_log_topic

    topic0 = topics[0].to_bytes(32, "big")
    for entry in abi:
        if entry.get("type") != "event":
            continue
        try:
            if event_abi_to_log_topic(entry) == topic0:
                types = ",".join(i["type"] for i in entry.get("inputs", []))
                return f"{entry['name']}({types})"
        except Exception:
            continue
    return f"<unknown event 0x{topics[0]:064x}>"


HELP_SUMMARY = """
[bold]Execution[/bold]
  [cyan]c[/cyan]ontinue               run until a breakpoint
  [cyan]n[/cyan]ext [N]                  next Solidity line, stepping over calls
  [cyan]s[/cyan]tep [N]                  next Solidity line, stepping into calls
  [cyan]si[/cyan] / [cyan]ni[/cyan] [N]               one opcode, into / over calls
  [cyan]finish[/cyan]                 run to the end of this frame
  [cyan]u[/cyan]ntil LOC              run to a line or *PC

[bold]Breakpoints[/bold]
  [cyan]b[/cyan] FILE:LINE            break on a source line
  [cyan]b[/cyan] FUNC                 break on a function
  [cyan]b[/cyan] SSTORE               break on every occurrence of an opcode
  [cyan]b[/cyan] *0x108               break on a program counter
  [cyan]b[/cyan] LOC if EXPR          conditional, EXPR is Solidity
  [cyan]tbreak[/cyan] / [cyan]d[/cyan]elete N / [cyan]disable[/cyan] / [cyan]enable[/cyan]
  [cyan]watch[/cyan] EXPR             break when a storage value changes

[bold]Inspection[/bold]
  [cyan]p[/cyan] EXPR                 evaluate Solidity: [dim]p balances[msg.sender] + 100 ether[/dim]
  [cyan]call[/cyan] EXPR              evaluate and KEEP the side effects
  [cyan]ptype[/cyan] EXPR             report the Solidity type
  [cyan]display[/cyan] EXPR           re-evaluate at every stop
  [cyan]x[/cyan]/NFU ADDR             examine memory: [dim]x/32xb 0x40[/dim]
  [cyan]bt[/cyan] / [cyan]f[/cyan] N / [cyan]up[/cyan] / [cyan]down[/cyan]   call stack, EVM and Solidity frames
  [cyan]l[/cyan]ist [LINE]                  source listing
  [cyan]disas[/cyan]semble            disassembly around the pc
  [cyan]copy[/cyan] [CMD]                  put a command's output on the system clipboard
  [cyan]i[/cyan]nfo TOPIC             registers, breakpoints, frame, args, locals,
                         storage, gas, logs, sources, functions

[bold]Mutation[/bold]
  [cyan]set var[/cyan] X = V          write storage through Solidity: [dim]set var balances[a] = 5 ether[/dim]
                         a bare local name writes its stack slot: [dim]set var fee = 1 ether[/dim]
  [cyan]set[/cyan] $pc = 0x108        jump; [cyan]set[/cyan] $gas = N; [cyan]set[/cyan] $stack[0] = V; [cyan]set[/cyan] $storage[1] = V

[bold]Assembly[/bold]
  [cyan]mstore[/cyan](0x80, 1)        type a builtin call straight at the prompt
  [cyan]sload[/cyan](3)               reads print their value and enter the history as $N
  [cyan]sstore[/cyan](3, add(sload(3), 1))    calls nest, exactly as in `assembly { }`
  [cyan]asm[/cyan] YUL                the explicit form; takes `;`-separated statements
                         [dim]every write shows up in the panes at once[/dim]

[bold]Foundry cheatcodes[/bold]
  [cyan]vm.warp[/cyan](1735689600)    block.timestamp; [cyan]vm.roll[/cyan](N) block.number
  [cyan]vm.deal[/cyan](addr, 10 ether)   set a balance
  [cyan]vm.prank[/cyan](addr)         rewrite msg.sender for the next call
  [cyan]vm.store[/cyan] / [cyan]vm.load[/cyan] / [cyan]vm.etch[/cyan] / [cyan]vm.label[/cyan] / [cyan]vm.sign[/cyan] / [cyan]vm.addr[/cyan]
  [cyan]vm.assertEq[/cyan](a, b)      the assertions forge-std calls; see [cyan]help cheatcodes[/cyan]

[bold]Convenience variables[/bold]
  $pc $gas $gasused $depth $sp $step $stack[N] $mem[0x40] $storage[1] $1 $2 ...

[bold]In the TUI[/bold]
  [cyan]f2[/cyan]                    hide the low-level panes; SOURCE takes the space
  [cyan]copy[/cyan] [dim]CMD[/dim]              run CMD and put its output on the system clipboard
  [cyan]copy[/cyan]                  the last output again, untruncated

  STACK labels the slots that hold this frame's locals.
  A pane you scroll stays where you left it; scroll back, or click the marker in
  its border, to have it follow execution again.

[dim]help <topic> for detail. topics: breakpoints, print, memory, mutation, assembly, cheatcodes, foundry, gas, locals[/dim]
"""

HELP_TOPICS = {
    "breakpoints": """
[bold]Breakpoints[/bold]
  b Bank.sol:46             a source line (snapped forward to the next line with code)
  b deposit                 a function, by name or Contract.name
  b SSTORE                  every SSTORE, in any contract
  b *0x108                  a raw program counter
  b Bank.sol:46 if totalDeposits > 1 ether
                            the condition is real Solidity, evaluated at the stop

Conditions can read local variables, state variables, msg/tx/block and call view
functions. A condition that fails to evaluate still breaks, as gdb does, and
`info breakpoints` says why.

  watch totalDeposits       stop when the value changes, reporting old -> new
  watch balances[0xabc..]   mapping elements work too
  watch *0x80               a 32-byte window of memory
""",
    "print": """
[bold]print[/bold]
`p EXPR` compiles EXPR as real Solidity against the paused contract and runs it on a state
snapshot that is thrown away afterwards, so it cannot disturb the run.

  p owner                            a state variable
  p balances[msg.sender] + 100 ether units and arithmetic
  p accounts[owner].nickname         structs and mappings
  p _fee(msg.value)                  internal and private functions
  p keccak256(abi.encode(owner))     any builtin
  p address(this).balance
  p $storage[1] + 1 ether            mix in low-level convenience variables

Results enter the value history as $1, $2 ... and can be reused in later expressions.
`call EXPR` is the same but KEEPS the effects, which is how you mutate through Solidity.
""",
    "memory": """
[bold]x, examine memory[/bold]
Same syntax as gdb: x/NFU ADDR.
  N  count      F  format x d u o t c s   U  unit b h w g (1, 2, 4, 8 bytes)

  x/32xb 0x40    32 bytes in hex
  x/4xg 0x80     4 eight-byte words
  x/s 0xa0       a string

Solidity's fixed layout is annotated for you: 0x00-0x3f scratch, 0x40 free memory
pointer, 0x60 zero slot.
""",
    "mutation": """
[bold]Changing state mid-execution[/bold]
  set var owner = msg.sender          writes storage through Solidity, so packed slots,
  set var balances[alice] = 5 ether   mappings and structs are all encoded correctly
  call deposit()                      run a function and keep the effects

  set $stack[0] = 0xc0ffee            rewrite an operand before the opcode consumes it
  set $gas = 100                      force an out-of-gas at an exact instruction
  set $mem[0x80] = 1
  set $storage[0] = 0xdead            raw slot write, bypassing the layout
  jump 0x108                          move the program counter (JUMPDESTs only)

  mstore(0x80, 1)                     raw assembly; see `help assembly`
  vm.deal(alice, 10 ether)            Foundry cheatcodes; see `help cheatcodes`

Every one of these re-reads the machine as soon as it lands, so the STACK, MEMORY,
STORAGE and VARIABLES panes show the change without stepping first.
""",
    "gas": """
[bold]info gas[/bold]
Shows the frame's limit, used, remaining and refund, the base cost of the current opcode,
then a profile: gas attributed to each source line and to each opcode, measured as the
real meter delta per instruction rather than from a cost table.
""",
    "locals": """
[bold]Local variables[/bold]
  info locals            the frame's locals, named, typed and decoded
  p amount - fee         expressions over them, in real Solidity
  b LOC if amount > 1     conditions over them
  set var fee = 1 ether  writes the local's stack slot

solc emits no location info for locals; sevm reconstructs it from the AST (name, type,
scope) plus the stack slot at the current pc. A local shadows a state variable of the
same name, as in the contract.

Not readable, reported as <unavailable> rather than guessed at:
  assembly variables     no AST declaration exists
  storage pointers       the slot number is shown; index the state variable instead
  calldata references    use `info args`
  a local on its own declaration line   step once, then read it
""",
}


def _assembly_help() -> str:
    """`help assembly`, generated from the builtin table so the two cannot disagree."""
    rows = [
        f"  {builtin.signature:<44} {builtin.summary}" for builtin in assembly_listing()
    ]
    return (
        """
[bold]Inline assembly (Yul)[/bold]
A Yul builtin typed at the prompt runs for real, on the frame you are stopped in. Unlike
`p`, which evaluates on a throwaway snapshot, assembly writes to the live machine.

  mstore(0x80, 1)                     write memory
  sstore(3, add(sload(3), 1))         read-modify-write a slot, nested as in Yul
  mload(0x40)                         reads print, and enter the value history as $N
  keccak256(0x80, 0x40)               hash a memory range
  asm mstore(0x40, 0xa0); mstore8(0xa0, 0x61)     `asm` also takes several statements

Arguments: decimal or hex literals, `1 ether`, `true`/`false`, a right-padded string
literal ("hi"), a nested call, or a convenience variable, e.g. `mstore(0x80, $storage[1])`.

Gas is metered and refunded, so this can't turn a passing run into an out-of-gas. Memory
expansion sticks, since the write really happened.

Refused: jump/jumpi/pc/push*/dup*/swap* (Yul excludes these itself) and
stop/return/revert/invalid/selfdestruct (would end the frame; use `finish` instead).

`mstore(...)` at the prompt is assembly; `p mstore(...)` is Solidity, so a contract
function of the same name stays reachable.

[bold]Builtins[/bold]
"""
        + "\n".join(rows)
        + "\n"
    )


def _cheatcode_help() -> str:
    """`help cheatcodes`, generated from the registry: documented set == implemented set."""
    rows = [
        f"  vm.{spec.signature:<42} {spec.doc}" for spec in cheat_listing() if spec.doc
    ]
    names = sorted({spec.name for spec in cheat_specs() if spec.family == "assert"})
    asserts = textwrap.wrap(
        "  ".join(names), width=84, initial_indent="  ", subsequent_indent="  "
    )
    return (
        """
[bold]Foundry cheatcodes[/bold]
The same cheatcodes a `.t.sol` calls, available at the prompt against the live state of
the frame you are stopped in. Arguments are plain literals: an integer, `1 ether`, a 0x
address or bytes value, `true`/`false`, or a quoted string.

  vm.warp(1735689600)
  vm.deal(0xf39F..2266, 10 ether)
  vm.startPrank(0xf39F..2266)
  vm.load(0xf39F..2266, 0x00)      returning cheats print their result

`vm.prank(...)` at the prompt and inside the test hit the same intercept. An unimplemented
selector reverts with a clear message instead of doing nothing.

[bold]Implemented[/bold]
"""
        + "\n".join(rows)
        + "\n\n[bold]Assertions[/bold]\n"
        + "\n".join(asserts)
        + """

forge-std's own `assertEq(a, b)` calls these, so a failed assertion in a test reverts with
`assertion failed: 1 != 2` (or your own message, when you pass one).
"""
    )


HELP_TOPICS["assembly"] = _assembly_help()
HELP_TOPICS["asm"] = HELP_TOPICS["assembly"]
HELP_TOPICS["yul"] = HELP_TOPICS["assembly"]
HELP_TOPICS["foundry"] = """
[bold]Foundry projects and libraries[/bold]
sevm compiles a `.t.sol` the way forge does, and installs what is missing first.

  sevm run test/Counter.t.sol        inside a project: its foundry.toml, remappings.txt
                                     and lib/ are used as they are
  sevm run /tmp/scratch/Demo.t.sol   outside one: writes foundry.toml, clones forge-std
                                     into lib/, appends remappings.txt
  sevm run -y ...                    skip the confirmation prompt
  sevm run --no-install ...          resolve from disk only; refuse a missing import

An unresolved import is looked up in sevm's table, then on npm, and cloned from its git
repository at the newest release tag:

  import "@openzeppelin/contracts/token/ERC20/ERC20.sol";
  -> lib/openzeppelin-contracts, and the remapping to reach it

Libraries are cloned, never updated: the pin stays until you change it, as with
`forge install`. git is required; the `forge` binary is not.
"""

HELP_TOPICS["cheatcodes"] = _cheatcode_help()
HELP_TOPICS["vm"] = HELP_TOPICS["cheatcodes"]
