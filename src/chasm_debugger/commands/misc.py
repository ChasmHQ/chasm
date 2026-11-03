"""Clipboard, help, and quitting."""

from __future__ import annotations

from typing import TYPE_CHECKING

from .. import clipboard
from .help import HELP_SUMMARY, HELP_TOPICS
from .render import _plain, describe_amount
from .result import CommandResult

if TYPE_CHECKING:
    from .processor import CommandProcessor


def cmd_copy(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """`copy [command]`: put a command's output on the system clipboard.

    Sidesteps mouse selection, which under tmux needs shift-held or a pipe config, and
    copies the full text rather than what fit in the pane.
    """
    target = rest.strip()
    if target:
        result = proc.execute(target)
        if result.error:
            return result
        lines = list(result.lines)
    elif proc._last_output:
        lines = list(proc._last_output)
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


def cmd_quit(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return CommandResult(quit=True)


def cmd_help(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    if args:
        topic = args[0]
        body = HELP_TOPICS.get(topic)
        if body:
            return CommandResult(lines=body.strip().split("\n"))
        return CommandResult(error=f"no help for {topic!r}")
    return CommandResult(lines=HELP_SUMMARY.strip().split("\n"))


VERBS = {
    "copy": cmd_copy,
    "y": cmd_copy,
    "help": cmd_help,
    "h": cmd_help,
    "?": cmd_help,
    "quit": cmd_quit,
    "q": cmd_quit,
    "exit": cmd_quit,
}
