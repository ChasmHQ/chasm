"""Solidity expression evaluation at a breakpoint.

The trick is to borrow the real Solidity compiler rather than write an interpreter: splice
`function __sevm_eval() returns (T) { return (<expr>); }` into the paused contract's
source, compile it, swap the runtime code in on a state snapshot, run it, decode, revert.

  bindings.py    getting locals and `msg.data`/`msg.sig` into the injected function
  injection.py   splicing the expression in, and inferring its type from solc's complaint
  evaluator.py   `Evaluator`: the compile/run/decode/revert cycle, with its caches
"""

from __future__ import annotations

from .bindings import (
    MSG_FIELDS,
    Binding,
    bindings_for,
    msg_bindings,
    rewrite_msg,
    unbindable_reference,
)
from .evaluator import Evaluator, make_eval_hook
from .injection import EVAL_FUNCTION, EvalError, EvalResult

__all__ = [
    "EVAL_FUNCTION",
    "MSG_FIELDS",
    "Binding",
    "EvalError",
    "EvalResult",
    "Evaluator",
    "bindings_for",
    "make_eval_hook",
    "msg_bindings",
    "rewrite_msg",
    "unbindable_reference",
]
