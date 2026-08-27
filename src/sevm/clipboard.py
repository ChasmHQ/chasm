"""The system clipboard, through the platform's own tool.

Deliberately a subprocess (`pbcopy`, `wl-copy`, `xclip`) rather than the OSC 52 escape
sequence a TUI would normally use. OSC 52 has to survive every layer between the process
and the window manager: tmux drops it unless `set-clipboard` is on, ssh and screen have
their own rules, and Terminal.app ignores it outright. Piping to the platform tool either
works or fails loudly, and it puts the text where Cmd+V will find it.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from collections.abc import Sequence


class ClipboardError(RuntimeError):
    """No clipboard tool was available, or the one we found failed."""


# In preference order. The first whose binary exists is used.
_CANDIDATES: Sequence[tuple[str, list[str]]] = (
    ("pbcopy", ["pbcopy"]),  # macOS
    ("wl-copy", ["wl-copy"]),  # Wayland
    ("xclip", ["xclip", "-selection", "clipboard"]),  # X11
    ("xsel", ["xsel", "--clipboard", "--input"]),
    ("clip.exe", ["clip.exe"]),  # WSL
)


def available_tool() -> str | None:
    """Name of the clipboard tool that would be used, or None if there is none."""
    for name, command in _CANDIDATES:
        if shutil.which(command[0]):
            return name
    return None


def copy(text: str) -> str:
    """Put `text` on the system clipboard. Returns the tool used.

    Raises `ClipboardError` with something actionable rather than failing quietly: a
    clipboard command that silently does nothing is worse than one that says it cannot.
    """
    for name, command in _CANDIDATES:
        if not shutil.which(command[0]):
            continue
        try:
            process = subprocess.run(
                command,
                input=text.encode("utf-8"),
                capture_output=True,
                timeout=5,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            raise ClipboardError(f"{name} failed: {exc}") from exc
        if process.returncode != 0:
            detail = process.stderr.decode("utf-8", "replace").strip()
            raise ClipboardError(f"{name} exited {process.returncode}: {detail}")
        return name

    hint = "install xclip or wl-clipboard" if os.name == "posix" else "no clipboard tool"
    raise ClipboardError(f"no system clipboard command found ({hint})")
