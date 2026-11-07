"""Yul / inline-assembly execution against the paused frame.

Unlike `p <expr>`, which compiles Solidity and runs it on a throwaway state snapshot, Yul
here runs via the *real* opcode implementations against the *live* computation, so
`sstore(3, 1)` writes the slot the running transaction will read.

  builtins.py  the builtin table, and which names are refused and why
  parser.py    lexing and parsing, plus the Yul-vs-Solidity decision at the prompt
  execute.py   running a parsed statement against the live frame
"""

from __future__ import annotations

from .builtins import AsmError, Builtin, listing
from .execute import run
from .parser import has_builtin_head, lexes, parse

__all__ = [
    "AsmError",
    "Builtin",
    "has_builtin_head",
    "lexes",
    "listing",
    "parse",
    "run",
]
