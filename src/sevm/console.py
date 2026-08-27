"""The plain-text frontend.

Exists so the engine can be proved correct without a UI in the way, and so `sevm run
--console` works over ssh, in CI, and anywhere the TUI cannot draw. It is the same
CommandProcessor the TUI drives, so behaviour cannot drift between the two.
"""

from __future__ import annotations

from rich.console import Console

from .commands import CommandProcessor, CommandResult, escape_markup
from .evaluate import Evaluator
from .session import DebugSession, Finished, Paused

BANNER = """[bold]sevm[/bold] - Solidity/EVM debugger on Py-EVM. [dim]gdb commands; `help` for the list.[/dim]"""


class ConsoleFrontend:
    def __init__(self, session: DebugSession, evaluator: Evaluator | None = None) -> None:
        self.session = session
        self.evaluator = evaluator or Evaluator(session.project)
        self.commands = CommandProcessor(session, self.evaluator)
        self.console = Console(highlight=False)
        self.last_line = ""

    def _emit(self, result: CommandResult) -> None:
        # `lines` are markup we built, with user text already escaped in. `notice`/`error`
        # quote raw user input, so they're escaped here too: unescaped, `balances[nope]`
        # loses its bracket to a style tag, and an unmatched `[/...]` raises MarkupError
        # and kills the session over a typo.
        for line in result.lines:
            self.console.print(line)
        if result.notice:
            # No toasts in a plain terminal, so it prints inline.
            self.console.print(f"[dim]{escape_markup(result.notice)}[/dim]")
        if result.error:
            self.console.print(
                f"[bold red]error:[/bold red] {escape_markup(result.error)}"
            )

    def on_first_stop(self, event) -> None:
        self.console.print(BANNER)
        if isinstance(event, Paused):
            for line in self.commands.describe_stop(event.snapshot):
                self.console.print(line)
        elif isinstance(event, Finished):
            self.console.print(
                "[yellow]the program finished without hitting the debugger[/yellow]"
            )

    def run(self, first_event=None) -> None:
        """Read-eval-print until the user quits or the program ends."""
        self.on_first_stop(first_event)
        while True:
            if self.session.finished:
                self.console.print("[dim]program finished; nothing left to debug[/dim]")
                break
            try:
                raw = self.console.input("[bold green](sevm)[/bold green] ")
            except (EOFError, KeyboardInterrupt):
                self.console.print()
                break
            # Bare Enter repeats the last command, as gdb does.
            line = raw.strip() or self.last_line
            if not line:
                continue
            self.last_line = line
            result = self.commands.execute(line)
            self._emit(result)
            if result.quit:
                break
        self.session.detach()
