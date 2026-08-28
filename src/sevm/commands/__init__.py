"""The gdb-style command layer, shared by both frontends.

Where things live:

  processor.py   `CommandProcessor`: state, dispatch, and the helpers verbs call
  result.py      `CommandResult`, what every verb hands back
  execution.py   continue, next, step, finish, until
  breaking.py    break, tbreak, delete, watch and friends
  inspecting.py  print, call, x, backtrace, frame, list, disassemble
  info.py        `info <topic>`
  mutation.py    set, asm, jump
  misc.py        copy, help, quit
  render.py      value -> Rich markup
  parsing.py     prompt line -> verb and arguments
  help.py        the `help` text
"""

from __future__ import annotations

from .processor import CommandProcessor
from .render import describe_amount, escape_markup
from .result import CommandResult

__all__ = [
    "CommandProcessor",
    "CommandResult",
    "describe_amount",
    "escape_markup",
]
