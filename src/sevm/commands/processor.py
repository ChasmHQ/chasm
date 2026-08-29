"""The gdb-compatible command layer.

Design rule: if gdb has a verb for it, we use gdb's verb and gdb's abbreviation. A user who
knows gdb should need to learn only the Solidity-specific parts. New verbs exist only for
things gdb has no concept of (`info storage`, `info gas`, `info logs`).

`CommandProcessor` holds the session, the selected frame and the value history; the verbs
themselves live in the group modules and are called with the processor passed in. Its
un-underscored methods are that shared surface, so anything a verb module needs is visible
from the call site.

The processor is shared by both frontends: the plain console calls `execute()` directly,
and the TUI calls it from a worker thread because `continue` blocks until the VM stops.
Output is Rich markup so both render it identically.
"""

from __future__ import annotations

import re
import shlex
from collections.abc import Callable
from functools import partial
from typing import Any

from ..assembly import AsmError, has_builtin_head, lexes
from ..cheatcodes import (
    CheatError,
    encode_cheat_call,
    format_cheat_result,
    parse_cheat_arg,
)
from ..decode import StorageDecoder
from ..evaluate import EvalError, EvalResult, Evaluator
from ..frames import FrameSnapshot
from ..session import DebugSession, Finished, Paused, SessionError, StepMode
from . import breaking, execution, info, inspecting, misc, mutation
from .parsing import (
    _CALL_SHAPED,
    _CONVENIENCE,
    _did_you_mean,
    _looks_like_a_command,
    _split_top_level,
)
from .render import _escape, _word
from .result import CommandResult


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
        # The stop before the command currently running. `describe_stop` diffs the
        # stack height against it, so a step that executes a POP reports `sp 13->12`
        # instead of leaving the shrink invisible.
        self._prev_snap: FrameSnapshot | None = None
        # `x` continuation state: the last format used, and the address just past the
        # last examination — a bare `x` resumes there, as in gdb.
        self._last_x_spec: str | None = None
        self._last_x_next: int = 0

        self._verbs: dict[str, Callable[[list[str], str], CommandResult]] = {}
        self._register()

    # ==================================================================
    # the command table
    # ==================================================================

    # Each group module owns its own verb names next to the implementations, so adding a
    # command is one function plus one line, in one file. Order decides who wins a
    # duplicate; there are none today.
    _GROUPS = (execution, breaking, inspecting, info, mutation, misc)

    def _register(self) -> None:
        table: dict[str, Callable[[list[str], str], CommandResult]] = {}
        for group in self._GROUPS:
            for verb, handler in group.VERBS.items():
                table[verb] = partial(handler, self)
        self._verbs = table

    def execute(self, line: str) -> CommandResult:
        """Run one prompt line. Never raises: a failure comes back as `result.error`.

        Load-bearing, not defensive: the TUI calls this from a worker thread owning the
        `busy` flag, so an escaped exception would end the console session, or in the TUI
        kill the worker with `busy` still set and wedge the prompt for the rest of the run.
        """
        line = line.strip()
        if not line:
            return CommandResult()
        self._prev_snap = self.session.last_snapshot
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
            substituted = self.substitute(line)
            if lexes(substituted):
                return self._remember(self.assemble(substituted))
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
        # its own "copied N characters" message.
        if getattr(handler, "func", None) is misc.cmd_copy:
            return result
        return self._remember(result)

    def _remember(self, result: CommandResult) -> CommandResult:
        """`copy` with no argument copies the last thing you looked at."""
        if result.lines:
            self._last_output = list(result.lines)
        return result

    def _not_a_command(self, line: str, verb: str) -> CommandResult:
        """gdb prints an expression's value when the verb is not a command; so do we.

        But `Undeclared identifier` for `brekapoint 12` sends the user looking for a
        variable they never wrote, so a line that reads like a command gets told so, with
        the nearest verb offered.
        """
        try:
            return inspecting.cmd_print(self, [], line)
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

    def assemble(self, source: str) -> CommandResult:
        """Execute `;`-separated Yul statements on the live frame and report each one.

        `source` has already been through `_substitute`, so `mstore(0x80, $storage[1])`
        and `sstore(0, $stack[0])` arrive here as plain numbers.

        Unlike `p`, which runs on a throwaway snapshot, this writes for real: the
        low-level twin of `set var`, reaching places Solidity has no syntax for.
        """
        rows = self.inspect("assembly", source)
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

    def require_stop(self) -> FrameSnapshot:
        snap = self.snapshot
        if snap is None or self.session.finished:
            raise SessionError("the program is not running")
        return snap

    def inspect(self, op: str, *args: Any, **kwargs: Any) -> Any:
        return self.session.inspect(op, *args, frame_index=self.selected_frame, **kwargs)

    def decoder(self, contract: str | None) -> StorageDecoder | None:
        if not contract:
            return None
        if contract not in self._decoders:
            art = self.project.artifact(contract)
            self._decoders[contract] = StorageDecoder(art.storage_layout if art else None)
        return self._decoders[contract]

    def source_lines(self, source_key: str | None) -> list[str]:
        if not source_key:
            return []
        src = self.project.sources.get(source_key)
        return src.text.split("\n") if src else []

    def resume(
        self, mode: StepMode, count: int = 1, target_pc: int | None = None
    ) -> CommandResult:
        if self.session.finished:
            return CommandResult(error="the program has finished")
        self.selected_frame = None
        self.selected_row = 0
        event = self.session.resume(mode, count=count, target_pc=target_pc)
        return self.render_event(event)

    def render_event(self, event: Any) -> CommandResult:
        """The standard rendering of a resume's outcome, shared by resume/reset/run."""
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
            result.lines.extend(self.render_displays())
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
            lines = self.source_lines(snap.source_key)
            if 0 < snap.line <= len(lines):
                out.append(f"[dim]{snap.line:>4}[/dim]  {_escape(lines[snap.line - 1])}")
        out.append(self._machine_line(snap))
        return out

    def _machine_line(self, snap: FrameSnapshot) -> str:
        """The one-line machine echo every stop ends with, gdb's `0xADDR in ...`.

        At opcode granularity this is the only signal of what a step did to the
        machine: the sp field shows old->new whenever the stack height changed
        since the previous stop (a POP reads `sp 13->12`), suppressed across a
        depth change where the two heights belong to different frames.
        """
        sp = len(snap.stack)
        prev = self._prev_snap
        if prev is not None and prev.depth == snap.depth and len(prev.stack) != sp:
            sp_field = f"{len(prev.stack)}->{sp}"
        else:
            sp_field = f"{sp}"
        nosrc = "" if snap.has_source else "  (no source)"
        return (
            f"[dim]pc 0x{snap.pc:04x}  {snap.mnemonic:<7} sp {sp_field}  "
            f"gas {snap.gas_remaining:,}  step {snap.step}{nosrc}[/dim]"
        )

    def _where(self, snap: FrameSnapshot) -> str:
        fn = snap.function.signature if snap.function else (snap.contract_name or "?")
        if snap.has_source:
            return f"[bold cyan]{fn}[/bold cyan] at [green]{snap.source_key}:{snap.line}[/green]"
        return f"[bold cyan]{fn}[/bold cyan] at [green]pc 0x{snap.pc:04x}[/green]"

    def render_displays(self) -> list[str]:
        out: list[str] = []
        for num, expr in self.displays:
            try:
                res = self.evaluate(expr)
                out.append(
                    f"[dim]{num}:[/dim] {_escape(expr)} = [bold]{_escape(res.display)}[/bold]"
                )
            except Exception as exc:
                out.append(f"[dim]{num}:[/dim] {_escape(expr)} = [red]<{exc}>[/red]")
        return out

    # -- expression evaluation ---------------------------------------------

    def substitute(self, expression: str) -> str:
        """Resolve gdb convenience variables into Solidity literals.

        `$pc`, `$gas`, `$stack[0]`, `$mem[0x40]`, `$storage[1]` and value history `$1`
        are all read straight off the paused VM, so they work with no source at all and
        can be mixed into a Solidity expression: `p $storage[1] + 1 ether`.
        """
        snap = self.require_stop()

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
                data = self.inspect("read_memory", offset, 32)
                return str(int.from_bytes(data, "big"))
            if match.group(4) is not None:
                slot = int(match.group(4), 0)
                return str(self.inspect("read_storage", slot))
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

    def evaluate(self, expression: str, keep: bool = False) -> EvalResult:
        return self.inspect(
            "evaluate",
            self.substitute(expression),
            keep=keep,
            internal_index=self.selected_internal,
        )

    def read_locals(self) -> list[dict]:
        return self.inspect("locals", internal_index=self.selected_internal)

    def is_local(self, name: str) -> bool:
        try:
            return any(row["name"] == name for row in self.read_locals())
        except Exception:
            return False

    # ==================================================================
    # execution commands
    # ==================================================================

    def parse_location(self, spec: str, snap: FrameSnapshot | None) -> tuple[str, int]:
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
