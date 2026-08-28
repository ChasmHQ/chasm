"""Lexing and parsing Yul at the prompt.

`has_builtin_head` plus `lexes` are the two halves of the assembly-vs-Solidity decision:
the head must name a known builtin, and every character must be one Yul can read (Yul has
no `.`, so `keccak256(abi.encode(owner))` stays on the Solidity path).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from .builtins import BLOCKED, BUILTINS, AsmError


@dataclass(frozen=True)
class Literal:
    value: int
    text: str


@dataclass(frozen=True)
class Call:
    name: str
    args: tuple[Any, ...]
    text: str


_ETHER_UNITS = {
    "wei": 1,
    "gwei": 10**9,
    "szabo": 10**12,
    "finney": 10**15,
    "ether": 10**18,
}

_TOKEN = re.compile(
    r"""
      (?P<space>\s+)
    | (?P<hex>0[xX][0-9a-fA-F_]+)
    | (?P<number>\d[\d_]*)
    | (?P<string>"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')
    | (?P<name>[A-Za-z_][A-Za-z0-9_]*)
    | (?P<punct>[(),;])
    """,
    re.VERBOSE,
)


@dataclass(frozen=True)
class _Token:
    kind: str
    text: str
    pos: int


def _tokenize(source: str) -> list[_Token]:
    tokens: list[_Token] = []
    pos = 0
    while pos < len(source):
        match = _TOKEN.match(source, pos)
        if match is None:
            raise AsmError(f"cannot read {source[pos]!r} at position {pos}")
        pos = match.end()
        kind = match.lastgroup or ""
        if kind == "space":
            continue
        tokens.append(_Token(kind, match.group(), match.start()))
    return tokens


class _Parser:
    """Recursive descent over the Yul expression grammar, which is all of two rules."""

    def __init__(self, source: str) -> None:
        self.source = source
        self.tokens = _tokenize(source)
        self.index = 0

    # -- token helpers ------------------------------------------------------

    @property
    def current(self) -> _Token | None:
        return self.tokens[self.index] if self.index < len(self.tokens) else None

    def take(self) -> _Token:
        token = self.current
        if token is None:
            raise AsmError("unexpected end of input")
        self.index += 1
        return token

    def accept(self, text: str) -> bool:
        token = self.current
        if token is not None and token.text == text:
            self.index += 1
            return True
        return False

    def expect(self, text: str) -> _Token:
        token = self.current
        if token is None:
            raise AsmError(f"expected {text!r} but the input ended")
        if token.text != text:
            raise AsmError(f"expected {text!r} but found {token.text!r}")
        self.index += 1
        return token

    # -- grammar ------------------------------------------------------------

    def parse_program(self) -> list[Call | Literal]:
        statements: list[Call | Literal] = []
        while self.current is not None:
            if self.accept(";"):
                continue
            statements.append(self.parse_expression())
            if self.current is not None and not self.accept(";"):
                token = self.take()
                raise AsmError(
                    f"expected ';' between statements but found {token.text!r}"
                )
        if not statements:
            raise AsmError("nothing to run")
        return statements

    def parse_expression(self) -> Call | Literal:
        token = self.take()
        if token.kind in ("hex", "number"):
            return self._number(token)
        if token.kind == "string":
            return self._string(token)
        if token.kind != "name":
            raise AsmError(f"expected a value or a builtin but found {token.text!r}")
        if token.text in ("true", "false"):
            return Literal(int(token.text == "true"), token.text)
        return self._call(token)

    def _number(self, token: _Token) -> Literal:
        text = token.text.replace("_", "")
        value = int(text, 16) if text[:2].lower() == "0x" else int(text)
        # `1 ether` is not Yul, but the unit suffix is what makes `set var` pleasant to use.
        unit = self.current
        if unit is not None and unit.kind == "name" and unit.text in _ETHER_UNITS:
            self.index += 1
            value *= _ETHER_UNITS[unit.text]
            return Literal(value, f"{token.text} {unit.text}")
        return Literal(value, token.text)

    def _string(self, token: _Token) -> Literal:
        body = token.text[1:-1].encode("utf-8").decode("unicode_escape").encode("latin-1")
        if len(body) > 32:
            raise AsmError("a string literal is at most 32 bytes")
        # Yul pads a string literal on the right, which is what makes
        # `mstore(0x80, "hi")` land the characters at the start of the word.
        return Literal(int.from_bytes(body.ljust(32, b"\x00"), "big"), token.text)

    def _call(self, token: _Token) -> Call:
        name = token.text
        lowered = name.lower()
        if lowered in BLOCKED:
            raise AsmError(f"`{lowered}`: {BLOCKED[lowered]}")
        builtin = BUILTINS.get(lowered)
        if builtin is None:
            raise AsmError(
                f"unknown Yul builtin `{name}`; `help assembly` lists them all"
            )
        if self.current is None or self.current.text != "(":
            raise AsmError(
                f"`{name}` is not a value; Yul builtins are always called, "
                f"as in `{builtin.signature}`"
            )
        self.expect("(")
        args: list[Call | Literal] = []
        if not self.accept(")"):
            while True:
                args.append(self.parse_expression())
                if self.accept(")"):
                    break
                self.expect(",")
        if len(args) != builtin.inputs:
            raise AsmError(
                f"`{builtin.signature}` takes {builtin.inputs} argument(s), got {len(args)}"
            )
        end = self.current.pos if self.current is not None else len(self.source)
        return Call(lowered, tuple(args), self.source[token.pos : end].strip())


def parse(source: str) -> list[Call | Literal]:
    """Parse `;`-separated Yul expression statements.

    Raises:
        AsmError: on any syntax error, unknown builtin, blocked builtin or wrong arity.
    """
    if not source.strip():
        raise AsmError("nothing to run")
    return _Parser(source).parse_program()


def has_builtin_head(line: str) -> bool:
    """True when a bare prompt line opens with a call to a Yul builtin.

    First half of the assembly-vs-Solidity decision: requiring a known builtin leaves a
    contract's own `foo(1)` on the Solidity path. A blocked name counts as known, so
    `revert(0, 0)` gets the Yul reason instead of `Undeclared identifier`. Second half is
    `lexes`, below.
    """
    match = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*\(", line.strip())
    if match is None:
        return False
    name = match.group(1).lower()
    return name in BUILTINS or name in BLOCKED


def lexes(source: str) -> bool:
    """True when every character of `source` is one Yul can read.

    Keeps `keccak256(abi.encode(owner))` (a Solidity expression whose head shares a
    builtin's name) on the Solidity path, since Yul has no `.`. Runs after
    convenience-variable substitution, or `mstore(0x80, $storage[1])` would be rejected
    for the `$` about to become a number.
    """
    try:
        _tokenize(source.strip())
    except AsmError:
        return False
    return True
