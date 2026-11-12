"""The Yul builtin table.

Blocked builtins are Yul's own restrictions (`jump`, `jumpi`, `pc`, `push*`, `dup*`,
`swap*`) plus the frame terminators, which have their own debugger verb instead. A blocked
name still counts as known, so `revert(0, 0)` gets the Yul reason rather than
`Undeclared identifier`.
"""

from __future__ import annotations

from dataclasses import dataclass

MAX_UINT256 = 2**256


class AsmError(Exception):
    pass


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
