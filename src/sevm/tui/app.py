"""The fullscreen TUI.

`continue` and `next` block until the VM stops, so every command runs on a Textual
thread worker. The worker never touches a widget; it posts a message and the app's
message handler renders on the event loop. Standard Textual pattern, and the only one
that keeps the UI responsive while the EVM runs.
"""

from __future__ import annotations

import contextlib
import os
from collections.abc import Sequence
from typing import Any

from textual import events, on, work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.message import Message
from textual.widgets import Input

from .. import clipboard
from ..commands import CommandProcessor, CommandResult, describe_amount
from ..decode import decode_calldata
from ..evaluate import Evaluator
from ..frames import FrameSnapshot
from ..session import DebugSession, Finished, Paused
from .widgets import (
    CallStackPane,
    CommandLog,
    DisassemblyPane,
    MemoryPane,
    SourcePane,
    StackPane,
    StatusBar,
    StoragePane,
    VariablesPane,
    pending_storage_slot,
    storage_rows,
)

CSS_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sevm.tcss")

WELCOME = """[bold]sevm[/bold]  Solidity/EVM debugger on Py-EVM"""


class CommandDone(Message):
    """A command finished on the worker thread; render its result on the UI thread."""

    def __init__(self, result: CommandResult, echo: str | None) -> None:
        super().__init__()
        self.result = result
        self.echo = echo


class PaneData(Message):
    """VM data gathered off-thread for the panes."""

    def __init__(self, payload: dict[str, Any]) -> None:
        super().__init__()
        self.payload = payload


class SevmApp(App):
    CSS_PATH = CSS_PATH
    TITLE = "sevm"

    BINDINGS = [
        Binding("f5", "cmd('continue')", "continue", show=True),
        Binding("f7", "cmd('step')", "step in", show=True),
        Binding("f8", "cmd('next')", "next", show=True),
        Binding("f10", "cmd('stepi')", "stepi", show=True),
        Binding("f11", "cmd('nexti')", "nexti", show=True),
        Binding("f6", "cmd('finish')", "finish", show=True),
        Binding("f9", "toggle_breakpoint", "breakpoint", show=True),
        Binding("f2", "toggle_lowlevel", "low level", show=True),
        Binding("f1", "cmd('help')", "help", show=True),
        # Ctrl+C means copy in a terminal and interrupt in a debugger; a live selection
        # decides which. `super+c` is Cmd+C, for terminals that forward it rather than
        # eating it first.
        Binding("ctrl+c", "copy_or_quit", "copy/quit", show=True, priority=True),
        Binding("super+c", "copy_selection", "copy", show=False, priority=True),
        Binding("ctrl+q", "quit", "quit", show=False, priority=True),
        Binding("escape", "focus_prompt", "prompt", show=False, priority=True),
        # Mac users reach for cmd; Textual spells it `super`. Both chords are bound so
        # the platform's own convention works without anyone learning a second one.
        Binding("super+p", "command_palette", "palette", show=False, priority=True),
        Binding("super+l", "clear_log", "clear log", show=False),
        Binding("super+q", "quit", "quit", show=False, priority=True),
        Binding("ctrl+l", "clear_log", "clear log", show=False),
        Binding("up", "history_prev", "", show=False),
        Binding("down", "history_next", "", show=False),
    ]

    def __init__(
        self,
        session: DebugSession,
        evaluator: Evaluator,
        first_event: Any = None,
        startup_commands: Sequence[str] | None = None,
    ) -> None:
        super().__init__()
        self.session = session
        self.evaluator = evaluator
        self.commands = CommandProcessor(session, evaluator)
        self.first_event = first_event
        self.startup_commands = list(startup_commands or [])
        self._pending_startup: list[str] = []
        self.show_lowlevel = True
        self.busy = False
        # Wear the terminal's own colours; Textual themes don't persist across runs anyway.
        self.theme = "ansi-dark"
        self._input_history: list[str] = []
        self._history_pos = 0
        self._last_command = ""

    # ==================================================================
    # layout
    # ==================================================================

    def compose(self) -> ComposeResult:
        yield StatusBar(id="status")
        with Horizontal(id="main"):
            with Vertical(id="left"):
                yield SourcePane(id="source")
                with Horizontal(id="lowlevel"):
                    yield DisassemblyPane(id="disasm")
                    yield StackPane(id="stack")
                    yield MemoryPane(id="memory")
            with Vertical(id="right"):
                yield CallStackPane(id="callstack")
                yield VariablesPane(id="variables")
                yield StoragePane(id="storage")
        yield CommandLog(id="log")
        yield Input(placeholder="(sevm) type a gdb command, or `help`", id="prompt")

    def on_mount(self) -> None:
        log = self.query_one("#log", CommandLog)
        log.write(WELCOME)
        if isinstance(self.first_event, Paused):
            for line in self.commands.describe_stop(self.first_event.snapshot):
                log.write(line)
        elif isinstance(self.first_event, Finished):
            log.write(
                "[yellow]the script finished without entering any known contract[/yellow]"
            )
        self.query_one("#prompt", Input).focus()
        # Startup commands run on an exclusive VM worker and clear `busy` only when that
        # worker finishes (see `_on_command_done`). Firing them in a loop here would drop
        # every command after the first to the `busy` guard, so dispatch them one at a
        # time, each after the previous one completes.
        self._pending_startup = list(self.startup_commands)
        if self._pending_startup:
            # Skip the pane refresh for this initial stop: its `_gather` worker inspects
            # the VM over the same queue a resuming startup command (`continue`) uses, and
            # the two overlapping crosses their replies. The stop is usually one the
            # startup commands step past anyway; refresh once the sequence drains.
            self._dispatch_next_startup()
        else:
            self.refresh_panes()

    def _dispatch_next_startup(self) -> None:
        if self._pending_startup and not self.busy:
            self.run_command(self._pending_startup.pop(0))

    # ==================================================================
    # command plumbing
    # ==================================================================

    def action_cmd(self, command: str) -> None:
        self.run_command(command, echo=True)

    def run_command(self, command: str, echo: bool = True) -> None:
        if self.busy:
            self.bell()
            return
        if not command.strip():
            return
        self.busy = True
        self._last_command = command
        self.query_one("#status", StatusBar).render_status(None, running=True)
        self._execute(command, command if echo else None)

    @work(thread=True, exclusive=True, group="vm")
    def _execute(self, command: str, echo: str | None) -> None:
        """Runs on a worker thread: `continue` can block for a long time.

        `CommandProcessor.execute` promises not to raise, and the belt-and-braces catch
        here is why that promise matters: a worker that dies never posts `CommandDone`,
        so `busy` stays set and the prompt ignores every subsequent keystroke. Turning
        any escape into an error line keeps the debugger usable.
        """
        try:
            result = self.commands.execute(command)
        except Exception as exc:  # the alternative is a permanently wedged prompt
            result = CommandResult(error=f"{type(exc).__name__}: {exc}")
        self.post_message(CommandDone(result, echo))

    @on(CommandDone)
    def _on_command_done(self, message: CommandDone) -> None:
        self.busy = False
        log = self.query_one("#log", CommandLog)
        if message.echo:
            log.write(f"[bold green](sevm)[/bold green] {_escape(message.echo)}")
        for line in message.result.lines:
            log.write(line)
        if message.result.notice:
            self.notify(message.result.notice)
        if message.result.error:
            log.write(f"[bold red]error:[/bold red] {_escape(message.result.error)}")
        if message.result.quit:
            self.exit()
            return
        # No pane refresh mid-sequence, for the queue-crossing reason in `on_mount`:
        # let the startup commands drain, then refresh once.
        if self._pending_startup:
            self._dispatch_next_startup()
            return
        self.refresh_panes()

    @on(Input.Submitted, "#prompt")
    def _on_submit(self, event: Input.Submitted) -> None:
        raw = event.value.strip()
        event.input.value = ""
        # Bare Enter repeats the last command, as gdb does.
        command = raw or self._last_command
        if raw:
            self._input_history.append(raw)
            self._history_pos = len(self._input_history)
        self.run_command(command)

    def _recall(self, text: str) -> None:
        """Put a history entry in the prompt with the cursor after it.

        Setting `value` alone leaves the cursor at column zero; every shell puts you at
        the end, ready to edit.
        """
        prompt = self.query_one("#prompt", Input)
        prompt.value = text
        prompt.cursor_position = len(text)

    def action_history_prev(self) -> None:
        if not self._input_history:
            return
        self._history_pos = max(0, self._history_pos - 1)
        self._recall(self._input_history[self._history_pos])

    def action_history_next(self) -> None:
        if not self._input_history:
            return
        self._history_pos = min(len(self._input_history), self._history_pos + 1)
        self._recall(
            ""
            if self._history_pos >= len(self._input_history)
            else self._input_history[self._history_pos]
        )

    def action_clear_log(self) -> None:
        self.query_one("#log", CommandLog).clear()

    def action_toggle_lowlevel(self) -> None:
        self.show_lowlevel = not self.show_lowlevel
        self.query_one("#lowlevel").display = self.show_lowlevel
        # SOURCE is `height: 1fr`, so hiding the low-level row hands it the whole left
        # column; it re-renders to fill the new height rather than keeping a short window.
        self.refresh_panes()
        self.query_one("#log", CommandLog).write(
            f"[dim]low-level panes {'shown' if self.show_lowlevel else 'hidden'}[/dim]"
        )

    def action_toggle_breakpoint(self) -> None:
        snap = self.session.last_snapshot
        if snap is None or not snap.has_source:
            self.bell()
            return
        existing = [
            bp.number
            for bp in self.session.breakpoints.breakpoints.values()
            if bp.file_id == snap.file_id and bp.line == snap.line
        ]
        self.run_command(
            f"delete {existing[0]}"
            if existing
            else f"break {snap.source_key}:{snap.line}"
        )

    # ==================================================================
    # panes
    # ==================================================================

    def refresh_panes(self) -> None:
        snap = self.session.last_snapshot
        self.query_one("#status", StatusBar).render_status(snap, running=False)
        if snap is None or self.session.finished:
            self.query_one("#log", CommandLog)
            return
        self._gather()

    @work(thread=True, exclusive=True, group="panes")
    def _gather(self) -> None:
        """Collect everything the panes need in one trip to the VM thread."""
        snap = self.session.last_snapshot
        payload: dict[str, Any] = {"snapshot": snap}
        if snap is None:
            self.post_message(PaneData(payload))
            return
        try:
            # A generous window, because the pane scrolls: you can walk forward through
            # code that has not run yet without leaving the debugger.
            payload["disassembly"] = self.session.inspect("disassembly", 40, 160)
        except Exception:
            payload["disassembly"] = []
        try:
            decoder = self.commands.decoder(snap.contract_name)
            reader = lambda slot: self.session.inspect("read_storage", slot)  # noqa: E731
            payload["storage"] = storage_rows(decoder, reader)
        except Exception:
            payload["storage"] = []
        payload["state_vars"] = [
            (name, type_name, value)
            for _slot, _offset, name, type_name, value in payload["storage"]
        ]
        payload["args"] = self._decode_args(snap)
        payload["locals"] = [
            (v.name, v.type_label, v.display if v.available else "<unavailable>")
            for v in snap.locals
            if v.kind != "param"
        ]
        if snap.locals:
            # Parameters of the Solidity frame we are actually in beat the calldata
            # decode, which describes the outermost external call.
            params = [
                (v.name, v.type_label, v.display if v.available else "<unavailable>")
                for v in snap.locals
                if v.kind == "param"
            ]
            if params:
                payload["args"] = params
        payload["displays"] = self._eval_displays()
        self.post_message(PaneData(payload))

    def _decode_args(self, snap: FrameSnapshot) -> list[tuple[str, str, str]]:
        art = (
            self.session.project.artifact(snap.contract_name)
            if snap.contract_name
            else None
        )
        if art is None:
            return []
        decoded = decode_calldata(art.abi, snap.calldata)
        if not decoded:
            return []
        _signature, params = decoded
        out = []
        for type_name, name, value in params:
            shown = value.hex() if isinstance(value, bytes) else str(value)
            out.append((name or "_", type_name, shown))
        return out

    def _eval_displays(self) -> list[tuple[str, str]]:
        out: list[tuple[str, str]] = []
        for _num, expr in self.commands.displays:
            try:
                out.append((expr, self.commands.evaluate(expr).display))
            except Exception as exc:
                out.append((expr, f"<{exc}>"))
        return out

    @on(PaneData)
    def _on_pane_data(self, message: PaneData) -> None:
        payload = message.payload
        snap: FrameSnapshot | None = payload.get("snapshot")

        source_text = None
        if snap is not None and snap.source_key:
            src = self.session.project.sources.get(snap.source_key)
            source_text = src.text if src else None

        breakpoint_lines = {
            bp.line
            for bp in self.session.breakpoints.breakpoints.values()
            if snap is not None and bp.file_id == snap.file_id and bp.line
        }
        gas_by_line = {
            line: spent
            for (file_id, line), spent in self.session.gas_by_line.items()
            if snap is not None and file_id == snap.file_id
        }

        self.query_one("#source", SourcePane).render_source(
            snap, source_text, breakpoint_lines, gas_by_line
        )
        self.query_one("#callstack", CallStackPane).render_stack(
            snap, selected=self.commands.selected_row
        )
        self.query_one("#variables", VariablesPane).render_variables(
            snap,
            payload.get("state_vars", []),
            payload.get("args", []),
            payload.get("displays", []),
            payload.get("locals", []),
        )
        self.query_one("#storage", StoragePane).render_storage(
            snap, payload.get("storage", []), pending_storage_slot(snap)
        )
        self.query_one("#disasm", DisassemblyPane).render_disassembly(
            payload.get("disassembly", []), snap
        )
        self.query_one("#stack", StackPane).render_stack_values(snap)
        self.query_one("#memory", MemoryPane).render_memory(snap)

    # ==================================================================
    # shutdown
    # ==================================================================

    def on_text_selected(self, event: events.TextSelected) -> None:
        """Copy on drag release, no key needed.

        Sidesteps macOS terminals swallowing Cmd+C before a full-screen program sees it.
        A plain click selects nothing, so only a real drag replaces the clipboard. The
        highlight is left up as the only feedback of what was copied; Textual clears it
        on the next click.
        """
        self.action_copy_selection()

    def action_copy_selection(self) -> None:
        selected = self.screen.get_selected_text()
        if selected:
            self.copy_to_clipboard(selected)

    def action_copy_or_quit(self) -> None:
        selected = self.screen.get_selected_text()
        if not selected:
            self.exit()
            return
        self.copy_to_clipboard(selected)

    def copy_to_clipboard(self, text: str) -> None:
        """Copy through the platform's own tool, falling back to Textual's OSC 52.

        Textual's OSC 52 has to survive every layer to the window manager: tmux drops it
        unless `set-clipboard` is on, and some terminals ignore it outright. Piping to
        `pbcopy`/`wl-copy`/`xclip` either works or reports why.
        """
        what = describe_amount(text)
        try:
            tool = clipboard.copy(text)
        except clipboard.ClipboardError as exc:
            super().copy_to_clipboard(text)
            self.notify(
                f"copied {what} via the terminal (OSC 52)\n{exc}",
                title="clipboard",
                severity="warning",
            )
            return
        self.notify(f"copied {what} to the clipboard ({tool})", timeout=3)

    def action_quit(self) -> None:
        self.exit()

    def on_unmount(self) -> None:
        with contextlib.suppress(Exception):
            self.session.detach(timeout=5.0)


def _escape(text: str) -> str:
    return str(text).replace("[", "\\[")
