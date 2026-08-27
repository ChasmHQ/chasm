"""Yul / inline-assembly execution against the paused frame.

Unlike `p <expr>`, which compiles Solidity and runs it on a throwaway state snapshot, a Yul
expression here runs via the *real* opcode implementations against the *live* computation, so
`sstore(3, 1)` writes the slot the running transaction will read.

Execution: args are evaluated depth-first, pushed onto the frame's real stack in EVM order
(first arg on top), Py-EVM's own opcode function runs, the result is read off the top, and the
stack is restored to its prior height/contents. Nothing is reimplemented, so `keccak256`,
`mcopy`, `staticcall` behave exactly as they do mid-execution.

Two departures from real execution: gas is metered then refunded (cost is reported, but
inspection must not be able to induce an out-of-gas); memory expansion from an op is kept,
since the op genuinely wrote there.

Blocked builtins: Yul's own restrictions (`jump`, `jumpi`, `pc`, `push*`, `dup*`, `swap*`)
plus the frame terminators, which have their own debugger verb instead.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from eth.exceptions import VMError

MAX_UINT256 = 2**256


class AsmError(Exception):
    """A Yul fragment could not be parsed or could not be run."""


# ==================================================================
# the builtin table
# ==================================================================


@dataclass(frozen=True)
class Builtin:
    """One Yul builtin: its opcode, its arity, and a line of help."""

    name: str
    opcode: int
    inputs: int
    outputs: int
    summary: str

    @property
    def signature(self) -> str:
        args = ", ".join(_ARG_NAMES.get(self.name, ())) or ""
        return f"{self.name}({args})"


# Argument names, so `help assembly` reads as documentation rather than as arity counts.
_ARG_NAMES: dict[str, tuple[str, ...]] = {
    "add": ("x", "y"),
    "sub": ("x", "y"),
    "mul": ("x", "y"),
    "div": ("x", "y"),
    "sdiv": ("x", "y"),
    "mod": ("x", "y"),
    "smod": ("x", "y"),
    "exp": ("x", "y"),
    "addmod": ("x", "y", "m"),
    "mulmod": ("x", "y", "m"),
    "signextend": ("i", "x"),
    "not": ("x",),
    "lt": ("x", "y"),
    "gt": ("x", "y"),
    "slt": ("x", "y"),
    "sgt": ("x", "y"),
    "eq": ("x", "y"),
    "iszero": ("x",),
    "and": ("x", "y"),
    "or": ("x", "y"),
    "xor": ("x", "y"),
    "byte": ("n", "x"),
    "shl": ("bits", "x"),
    "shr": ("bits", "x"),
    "sar": ("bits", "x"),
    "keccak256": ("offset", "length"),
    "sha3": ("offset", "length"),
    "pop": ("x",),
    "mload": ("offset",),
    "mstore": ("offset", "value"),
    "mstore8": ("offset", "byte"),
    "mcopy": ("to", "from", "length"),
    "msize": (),
    "sload": ("slot",),
    "sstore": ("slot", "value"),
    "tload": ("slot",),
    "tstore": ("slot", "value"),
    "balance": ("account",),
    "extcodesize": ("account",),
    "extcodehash": ("account",),
    "extcodecopy": ("account", "to", "from", "length"),
    "calldataload": ("offset",),
    "calldatacopy": ("to", "from", "length"),
    "codecopy": ("to", "from", "length"),
    "returndatacopy": ("to", "from", "length"),
    "blockhash": ("number",),
    "blobhash": ("index",),
    "log0": ("offset", "length"),
    "log1": ("offset", "length", "topic0"),
    "log2": ("offset", "length", "topic0", "topic1"),
    "log3": ("offset", "length", "topic0", "topic1", "topic2"),
    "log4": ("offset", "length", "topic0", "topic1", "topic2", "topic3"),
    "create": ("value", "offset", "length"),
    "create2": ("value", "offset", "length", "salt"),
    "call": ("gas", "to", "value", "in", "insize", "out", "outsize"),
    "callcode": ("gas", "to", "value", "in", "insize", "out", "outsize"),
    "delegatecall": ("gas", "to", "in", "insize", "out", "outsize"),
    "staticcall": ("gas", "to", "in", "insize", "out", "outsize"),
}

# (yul name, opcode, inputs, outputs, summary). Names and arities follow the Yul dialect
# for the EVM, so anything valid inside `assembly { }` is valid at the prompt.
_TABLE: tuple[tuple[str, int, int, int, str], ...] = (
    # arithmetic and comparison
    ("add", 0x01, 2, 1, "x + y"),
    ("sub", 0x03, 2, 1, "x - y"),
    ("mul", 0x02, 2, 1, "x * y"),
    ("div", 0x04, 2, 1, "x / y, unsigned (0 if y is 0)"),
    ("sdiv", 0x05, 2, 1, "x / y, two's-complement signed"),
    ("mod", 0x06, 2, 1, "x % y, unsigned"),
    ("smod", 0x07, 2, 1, "x % y, signed"),
    ("exp", 0x0A, 2, 1, "x to the power y"),
    ("addmod", 0x08, 3, 1, "(x + y) % m, at arbitrary precision"),
    ("mulmod", 0x09, 3, 1, "(x * y) % m, at arbitrary precision"),
    ("signextend", 0x0B, 2, 1, "sign-extend x from byte i"),
    ("not", 0x19, 1, 1, "bitwise negation"),
    ("lt", 0x10, 2, 1, "1 if x < y, unsigned"),
    ("gt", 0x11, 2, 1, "1 if x > y, unsigned"),
    ("slt", 0x12, 2, 1, "1 if x < y, signed"),
    ("sgt", 0x13, 2, 1, "1 if x > y, signed"),
    ("eq", 0x14, 2, 1, "1 if x == y"),
    ("iszero", 0x15, 1, 1, "1 if x is 0"),
    ("and", 0x16, 2, 1, "bitwise and"),
    ("or", 0x17, 2, 1, "bitwise or"),
    ("xor", 0x18, 2, 1, "bitwise xor"),
    ("byte", 0x1A, 2, 1, "the nth byte of x, counting from the left"),
    ("shl", 0x1B, 2, 1, "x shifted left"),
    ("shr", 0x1C, 2, 1, "x shifted right, filling with zeros"),
    ("sar", 0x1D, 2, 1, "x shifted right, sign-preserving"),
    ("keccak256", 0x20, 2, 1, "hash a memory range"),
    ("sha3", 0x20, 2, 1, "hash a memory range (keccak256 under its opcode name)"),
    # stack, memory, storage
    ("pop", 0x50, 1, 0, "discard a value"),
    ("mload", 0x51, 1, 1, "read 32 bytes of memory"),
    ("mstore", 0x52, 2, 0, "write 32 bytes of memory"),
    ("mstore8", 0x53, 2, 0, "write one byte of memory"),
    ("mcopy", 0x5E, 3, 0, "copy a memory range"),
    ("msize", 0x59, 0, 1, "size of the touched memory"),
    ("sload", 0x54, 1, 1, "read a storage slot"),
    ("sstore", 0x55, 2, 0, "write a storage slot"),
    ("tload", 0x5C, 1, 1, "read a transient storage slot"),
    ("tstore", 0x5D, 2, 0, "write a transient storage slot"),
    # accounts and code
    ("address", 0x30, 0, 1, "the address this frame's storage belongs to"),
    ("balance", 0x31, 1, 1, "an account's balance"),
    ("selfbalance", 0x47, 0, 1, "this contract's balance"),
    ("caller", 0x33, 0, 1, "msg.sender"),
    ("callvalue", 0x34, 0, 1, "msg.value"),
    ("calldataload", 0x35, 1, 1, "read 32 bytes of calldata"),
    ("calldatasize", 0x36, 0, 1, "size of the calldata"),
    ("calldatacopy", 0x37, 3, 0, "copy calldata into memory"),
    ("codesize", 0x38, 0, 1, "size of this frame's code"),
    ("codecopy", 0x39, 3, 0, "copy this frame's code into memory"),
    ("extcodesize", 0x3B, 1, 1, "size of another account's code"),
    ("extcodecopy", 0x3C, 4, 0, "copy another account's code into memory"),
    ("extcodehash", 0x3F, 1, 1, "hash of another account's code"),
    ("returndatasize", 0x3D, 0, 1, "size of the last call's return data"),
    ("returndatacopy", 0x3E, 3, 0, "copy return data into memory"),
    # environment
    ("origin", 0x32, 0, 1, "tx.origin"),
    ("gasprice", 0x3A, 0, 1, "tx.gasprice"),
    ("blockhash", 0x40, 1, 1, "hash of a recent block"),
    ("blobhash", 0x49, 1, 1, "a blob versioned hash"),
    ("coinbase", 0x41, 0, 1, "block.coinbase"),
    ("timestamp", 0x42, 0, 1, "block.timestamp"),
    ("number", 0x43, 0, 1, "block.number"),
    ("prevrandao", 0x44, 0, 1, "block.prevrandao"),
    ("difficulty", 0x44, 0, 1, "block.prevrandao under its pre-merge name"),
    ("gaslimit", 0x45, 0, 1, "block.gaslimit"),
    ("chainid", 0x46, 0, 1, "the chain id"),
    ("basefee", 0x48, 0, 1, "block.basefee"),
    ("blobbasefee", 0x4A, 0, 1, "block.blobbasefee"),
    ("gas", 0x5A, 0, 1, "gas remaining in this frame"),
    # logs
    ("log0", 0xA0, 2, 0, "emit an anonymous event"),
    ("log1", 0xA1, 3, 0, "emit an event with one topic"),
    ("log2", 0xA2, 4, 0, "emit an event with two topics"),
    ("log3", 0xA3, 5, 0, "emit an event with three topics"),
    ("log4", 0xA4, 6, 0, "emit an event with four topics"),
    # calls and creation
    ("create", 0xF0, 3, 1, "deploy a contract"),
    ("create2", 0xF5, 4, 1, "deploy at a deterministic address"),
    ("call", 0xF1, 7, 1, "call another contract"),
    ("callcode", 0xF2, 7, 1, "call with our storage (legacy)"),
    ("delegatecall", 0xF4, 6, 1, "call with our storage and sender"),
    ("staticcall", 0xFA, 6, 1, "call, read-only"),
)

BUILTINS: dict[str, Builtin] = {
    name: Builtin(name, opcode, inputs, outputs, summary)
    for name, opcode, inputs, outputs, summary in _TABLE
}

# Refused, with reason. First group: Yul's own exclusion list, compiler-only control-flow
# opcodes a Yul author never writes. Second group would end the frame under the debugger's
# feet; each has a verb instead.
BLOCKED: dict[str, str] = {
    "jump": "Yul has no `jump`; use the debugger's `jump 0xPC` or `set $pc = 0xPC`",
    "jumpi": "Yul has no `jumpi`; set a conditional breakpoint instead",
    "jumpdest": "Yul has no `jumpdest`",
    "pc": "Yul has no `pc`; read `$pc`",
    "stop": "that would end the frame; use `finish` to run it to its end",
    "return": "that would end the frame; use `finish` to run it to its end",
    "revert": "that would end the frame; use `finish` to run it to its end",
    "selfdestruct": "refused: it would destroy the contract you are debugging",
    "invalid": "that would end the frame; use `finish` to run it to its end",
}
for _n in range(33):
    BLOCKED[f"push{_n}"] = "Yul has no `push`; write the value as a literal"
for _n in range(1, 17):
    BLOCKED[f"dup{_n}"] = "Yul has no `dup`; read `$stack[N]`"
    BLOCKED[f"swap{_n}"] = "Yul has no `swap`; write `set $stack[N] = V`"


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


def listing() -> list[Builtin]:
    """Every builtin, deduplicated by opcode alias, for `help assembly`."""
    seen: set[str] = set()
    out: list[Builtin] = []
    for builtin in BUILTINS.values():
        if builtin.name in ("sha3", "difficulty"):
            continue  # aliases of keccak256 / prevrandao
        if builtin.name in seen:
            continue
        seen.add(builtin.name)
        out.append(builtin)
    return out


# ==================================================================
# parsing
# ==================================================================


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


# ==================================================================
# execution (VM thread)
# ==================================================================


def _as_int(value: Any) -> int:
    """Py-EVM stack items are int OR bytes depending on how they were pushed."""
    if isinstance(value, int):
        return value
    return int.from_bytes(value, "big")


def _push(stack: Any, value: int, where: str) -> None:
    if not 0 <= value < MAX_UINT256:
        raise AsmError(f"`{where}`: {value} does not fit in a 256-bit word")
    stack.push_int(value)


def _apply(
    session: Any, computation: Any, builtin: Builtin, args: list[int]
) -> int | None:
    """Run one opcode against the live frame, leaving the stack exactly as it was."""
    opcode_fn = computation.opcodes.get(builtin.opcode)
    if opcode_fn is None:
        raise AsmError(f"`{builtin.name}` is not available in this fork")
    stack = computation._stack
    # Slice-assignment, never rebinding: Py-EVM's Stack caches `append`/`pop` bound to the
    # list object it was constructed with, so a fresh list would silently detach them.
    saved = list(stack.values)
    try:
        for value in reversed(args):
            _push(stack, value, builtin.name)
        # Suspended, so a call opcode that re-enters `apply_computation` runs untraced
        # instead of trying to pause a debugger that is already parked.
        with session.suspended():
            opcode_fn(computation=computation)
        if not builtin.outputs:
            return None
        if len(stack.values) <= len(saved):
            raise AsmError(f"`{builtin.name}` produced no result")
        return _as_int(stack.values[-1])
    except VMError as exc:
        detail = str(exc) or type(exc).__name__
        raise AsmError(f"`{builtin.name}` failed: {detail}") from exc
    finally:
        stack.values[:] = saved


def _evaluate(session: Any, computation: Any, node: Call | Literal) -> int | None:
    if isinstance(node, Literal):
        return node.value
    args: list[int] = []
    for arg in node.args:
        value = _evaluate(session, computation, arg)
        if value is None:
            name = arg.name if isinstance(arg, Call) else "?"
            raise AsmError(f"`{name}` returns nothing, so it cannot be an argument")
        args.append(value)
    return _apply(session, computation, BUILTINS[node.name], args)


def run(session: Any, computation: Any, source: str) -> list[dict]:
    """Execute `source` against the paused frame and describe what each statement did.

    Returns one row per statement: `{"text", "name", "value", "gas"}`, where `value` is
    None for a statement that produces nothing (`mstore`, `sstore`, `log1`).

    Raises:
        AsmError: on a parse error or a failed opcode. Statements before the failing one
            have already run and are not undone; the EVM has no undo either.
    """
    statements = parse(source)
    meter = computation._gas_meter
    rows: list[dict] = []
    for node in statements:
        before = meter.gas_remaining
        try:
            value = _evaluate(session, computation, node)
            spent = before - meter.gas_remaining
        finally:
            # Cost is real and worth reporting, but inspection must not starve the
            # transaction of gas, so the meter is restored.
            meter.gas_remaining = before
        rows.append(
            {
                "text": node.text,
                "name": node.name if isinstance(node, Call) else "literal",
                "value": value,
                "gas": spent,
            }
        )
    return rows
