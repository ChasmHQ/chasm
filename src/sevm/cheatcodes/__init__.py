"""Foundry cheatcode engine.

A cheatcode is a call to the magic address `0x7109709E...`; `console.log` is the same idea
against `0x000000000000000000636F6e736F6c652e6c6f67`. sevm's patched opcode loop intercepts
both and dispatches here.

Importing the handler modules is load-bearing, not tidiness: their handlers register
themselves with `@_cheat` at import time, and without it `apply_cheat` finds an empty table.

  registry.py    the spec table, `@_cheat`, and `apply_cheat`
  cheats.py      block env, account state, nonce, prank, labelling, randomness
  env.py         the `vm.env*` family (process environment variables)
  convert.py     pure transforms: toString, parse*, base64, string ops, CREATE address
  wallet.py      key derivation, compact signing, createWallet
  assertions.py  the `vm.assert*` family, generated from an op x type matrix
  args.py        parsing and encoding arguments typed at the prompt
  console.py     decoding `console.log` payloads
"""

from __future__ import annotations

from . import assertions, cheats, convert, env, wallet  # noqa: F401  (registers handlers)
from .args import (
    encode_cheat_call,
    format_cheat_result,
    parse_cheat_arg,
)
from .console import CONSOLE_ADDRESS, decode_console_log
from .registry import (
    VM_ADDRESS,
    CheatContext,
    CheatError,
    CheatSpec,
    CheatState,
    Prank,
    all_specs,
    apply_cheat,
    cheat_name,
    listing,
    spec_by_name,
    specs_by_name,
)

__all__ = [
    "CONSOLE_ADDRESS",
    "VM_ADDRESS",
    "CheatContext",
    "CheatError",
    "CheatSpec",
    "CheatState",
    "Prank",
    "all_specs",
    "apply_cheat",
    "cheat_name",
    "decode_console_log",
    "encode_cheat_call",
    "format_cheat_result",
    "listing",
    "parse_cheat_arg",
    "spec_by_name",
    "specs_by_name",
]
