"""What every command hands back."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


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
