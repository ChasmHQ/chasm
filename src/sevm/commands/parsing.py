"""Reading a prompt line: its verb, its arguments, and what the user probably meant.

`_CONVENIENCE` and `_X_FORMAT` are the two gdb-borrowed grammars; the rest is argument
coercion and the did-you-mean fallback.
"""

from __future__ import annotations

import difflib
import re

from ..session import SessionError

# Convenience variables, gdb-style. These bypass solc entirely so they work even when a
# contract has no source.
_CONVENIENCE = re.compile(
    r"\$(pc|gas|gasused|depth|sp|step|stack\[(\d+)\]|mem\[(0x[0-9a-fA-F]+|\d+)\]"
    r"|storage\[(0x[0-9a-fA-F]+|\d+)\]|(\d+))"
)

# Only the formats we actually implement. gdb's `i` (instruction) and `f` (float) have no
# meaning over EVM memory, so they are rejected with a message rather than silently
# falling through to hex.
_X_FORMAT = re.compile(r"^/(\d*)([xduotc s]?)([bhwg]?)$".replace(" ", ""))

_UNIT_SIZES = {"b": 1, "h": 2, "w": 4, "g": 8}


def _split_top_level(text: str) -> list[str]:
    """Split on commas that are not inside quotes or parentheses (for `vm.x(a, b)`)."""
    parts: list[str] = []
    depth = 0
    quote: str | None = None
    current: list[str] = []
    for ch in text:
        if quote:
            current.append(ch)
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
            current.append(ch)
        elif ch in "([{":
            depth += 1
            current.append(ch)
        elif ch in ")]}":
            depth -= 1
            current.append(ch)
        elif ch == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(ch)
    if current:
        parts.append("".join(current))
    return [p.strip() for p in parts if p.strip()]


def _count(args: list[str]) -> int:
    if args and args[0].isdigit():
        return int(args[0])
    return 1


def _integer(text: str, verb: str) -> int:
    """Parse a decimal or 0x argument, or say which word was not a number.

    `int(text, 0)` raises `invalid literal for int() with base 10`, which names neither
    the command the user typed nor what it wanted instead.
    """
    try:
        return int(text, 0)
    except ValueError as exc:
        raise SessionError(f"{verb}: {text!r} is not a number") from exc


def _breakpoint_numbers(args: list[str], verb: str) -> list[int]:
    return [_integer(arg, verb) for arg in args]


# A line with no operators, brackets or dots: it reads as a verb and its arguments, not as
# an expression. `bt`, `brekapoint 12` and `nonsense` match; `a + b`, `p.x` and `m[k]` do not.
_COMMAND_SHAPED = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(\s+[A-Za-z0-9_:*$/.\[\]-]+)*")

# `name(...)`, which at this prompt is as likely to be a Yul builtin as a Solidity call.
_CALL_SHAPED = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*\s*\(")


def _looks_like_a_command(line: str) -> bool:
    return _COMMAND_SHAPED.fullmatch(line.strip()) is not None


def _did_you_mean(verb: str, verbs: dict) -> str:
    """The nearest real verbs, gdb-style, or "" when nothing is close enough.

    One- and two-letter aliases are excluded, since they match almost anything short.
    Ties on edit distance break by shared prefix, which puts `break` ahead of `print`
    for `brekpoint`.
    """
    candidates = [name for name in verbs if len(name) > 2]
    matches = difflib.get_close_matches(verb, candidates, n=4, cutoff=0.55)
    if not matches:
        return ""
    ranked = sorted(
        matches,
        key=lambda m: (
            -difflib.SequenceMatcher(None, verb, m).ratio(),
            -_shared_prefix(verb, m),
        ),
    )[:2]
    return " Did you mean " + ", ".join(f"`{m}`" for m in ranked) + "?"


def _shared_prefix(a: str, b: str) -> int:
    count = 0
    for left, right in zip(a, b, strict=False):
        if left != right:
            break
        count += 1
    return count


_OPCODE_NAMES: frozenset | None = None


def _known_opcodes() -> frozenset:
    global _OPCODE_NAMES
    if _OPCODE_NAMES is None:
        from ..disasm import OPCODES

        _OPCODE_NAMES = frozenset(OPCODES.values())
    return _OPCODE_NAMES
