"""Local variables: the AST says what they are, the run says where they are.

solc's standard JSON has no location info for locals, so the stack slot is recovered at run
time from the source map. Two observations carry it, and both are easy to get wrong:

  1. **Frame base.** An internal Solidity call is an `i`-marked JUMP. At the JUMPDEST it
     lands on, the stack holds the caller's frame plus the return label plus the arguments,
     so parameters occupy the slots directly below entry height.

  2. **Allocation site.** The instruction that allocates a local is attributed to the
     `VariableDeclaration` node's own source range (`uint256 fee`), distinct from the
     enclosing statement's range (`uint256 fee = _fee(amount)`). Stack height immediately
     before that instruction is the local's absolute position, and it does not move while
     the frame lives.

The session does the observing; this package owns the static and decoding halves:

  layout.py  how many stack slots a type occupies
  index.py   what solc declared, and where each name is in scope
  values.py  a stack word plus a type becomes a value
"""

from __future__ import annotations

from .index import (
    KIND_LOCAL,
    KIND_PARAM,
    KIND_RETURN,
    FunctionLocals,
    LocalsIndex,
    LocalVar,
    declaration_pcs,
    referenced_names,
)
from .layout import stack_slots
from .values import LocalValue, decode_value_type, read_local

__all__ = [
    "KIND_LOCAL",
    "KIND_PARAM",
    "KIND_RETURN",
    "FunctionLocals",
    "LocalValue",
    "LocalVar",
    "LocalsIndex",
    "declaration_pcs",
    "decode_value_type",
    "read_local",
    "referenced_names",
    "stack_slots",
]
