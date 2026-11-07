"""What an opcode does, and what it is about to eat off the stack.

Feeds the inline hint in the DISASSEMBLY pane and the operand labels in STACK, so a
reader does not have to look up DELEGATECALL while staring at one.
"""

from __future__ import annotations

# have to look up what DELEGATECALL does while staring at one.
OPCODE_HINTS = {
    "SSTORE": "write storage: slot <- value",
    "SLOAD": "read storage at slot",
    "TSTORE": "write transient storage",
    "TLOAD": "read transient storage",
    "MSTORE": "write 32 bytes to memory",
    "MSTORE8": "write 1 byte to memory",
    "MLOAD": "read 32 bytes from memory",
    "CALL": "call another contract (new frame)",
    "STATICCALL": "call another contract, read-only",
    "DELEGATECALL": "run their code against OUR storage",
    "CALLCODE": "run their code against OUR storage (legacy)",
    "CREATE": "deploy a new contract",
    "CREATE2": "deploy at a deterministic address",
    "REVERT": "abort and undo this frame",
    "RETURN": "return from this frame",
    "SELFDESTRUCT": "destroy this contract",
    "JUMP": "unconditional jump",
    "JUMPI": "jump if the condition is non-zero",
    "JUMPDEST": "a legal jump target",
    "KECCAK256": "hash a memory range",
    "CALLDATALOAD": "read 32 bytes of calldata",
    "CALLDATACOPY": "copy calldata into memory",
    "LOG0": "emit an anonymous event",
    "LOG1": "emit an event",
    "LOG2": "emit an event",
    "LOG3": "emit an event",
    "LOG4": "emit an event",
}

# How many stack items the next opcode consumes, so they can be highlighted as operands.
_OPERANDS = {
    "SSTORE": 2,
    "SLOAD": 1,
    "TSTORE": 2,
    "TLOAD": 1,
    "MSTORE": 2,
    "MSTORE8": 2,
    "MLOAD": 1,
    "JUMP": 1,
    "JUMPI": 2,
    "RETURN": 2,
    "REVERT": 2,
    "KECCAK256": 2,
    "CALL": 7,
    "CALLCODE": 7,
    "DELEGATECALL": 6,
    "STATICCALL": 6,
    "CREATE": 3,
    "CREATE2": 4,
    "ADD": 2,
    "SUB": 2,
    "MUL": 2,
    "DIV": 2,
    "SDIV": 2,
    "MOD": 2,
    "SMOD": 2,
    "EXP": 2,
    "LT": 2,
    "GT": 2,
    "SLT": 2,
    "SGT": 2,
    "EQ": 2,
    "AND": 2,
    "OR": 2,
    "XOR": 2,
    "SHL": 2,
    "SHR": 2,
    "SAR": 2,
    "BYTE": 2,
    "ISZERO": 1,
    "NOT": 1,
    "BALANCE": 1,
    "EXTCODESIZE": 1,
    "EXTCODEHASH": 1,
    "BLOCKHASH": 1,
    "LOG0": 2,
    "LOG1": 3,
    "LOG2": 4,
    "LOG3": 5,
    "LOG4": 6,
    "CALLDATALOAD": 1,
    "CALLDATACOPY": 3,
    "CODECOPY": 3,
    "RETURNDATACOPY": 3,
}

# Names for those operands, so the stack reads as arguments rather than as numbers.
_OPERAND_NAMES = {
    "SSTORE": ("slot", "value"),
    "SLOAD": ("slot",),
    "TSTORE": ("slot", "value"),
    "TLOAD": ("slot",),
    "MSTORE": ("offset", "value"),
    "MSTORE8": ("offset", "byte"),
    "MLOAD": ("offset",),
    "JUMP": ("dest",),
    "JUMPI": ("dest", "cond"),
    "RETURN": ("offset", "length"),
    "REVERT": ("offset", "length"),
    "KECCAK256": ("offset", "length"),
    "CALL": ("gas", "to", "value", "in", "insize", "out", "outsize"),
    "STATICCALL": ("gas", "to", "in", "insize", "out", "outsize"),
    "DELEGATECALL": ("gas", "to", "in", "insize", "out", "outsize"),
    "CALLCODE": ("gas", "to", "value", "in", "insize", "out", "outsize"),
    "CREATE": ("value", "offset", "length"),
    "CREATE2": ("value", "offset", "length", "salt"),
    "BALANCE": ("address",),
    "CALLDATALOAD": ("offset",),
    "LOG1": ("offset", "length", "topic0"),
    "LOG2": ("offset", "length", "topic0", "topic1"),
    "LOG3": ("offset", "length", "topic0", "topic1", "topic2"),
}


# ==================================================================
# formatting helpers
# ==================================================================


def operand_count(mnemonic: str) -> int:
    return _OPERANDS.get(mnemonic, 0)


def operand_name(mnemonic: str, index: int) -> str:
    labels = _OPERAND_NAMES.get(mnemonic)
    if labels and index < len(labels):
        return labels[index]
    return ""
