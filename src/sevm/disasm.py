"""EVM disassembly.

The opcode table is derived at import time by pairing `eth.vm.opcode_values` with
`eth.vm.mnemonics`, which declare the same constant names. That keeps our mnemonics
identical to the ones Py-EVM reports at runtime rather than a hand-maintained copy that
drifts one hard fork later.
"""

from __future__ import annotations

from dataclasses import dataclass

from eth.vm import mnemonics as _mnemonics
from eth.vm import opcode_values as _opcode_values

from .srcmap import PUSH0, PUSH1, PUSH32

JUMPDEST = 0x5B


def _build_opcode_table() -> dict[int, str]:
    table: dict[int, str] = {}
    for name, value in vars(_opcode_values).items():
        if name.startswith("_") or not isinstance(value, int):
            continue
        mnemonic = getattr(_mnemonics, name, None)
        if isinstance(mnemonic, str):
            table[value] = mnemonic
    return table


OPCODES: dict[int, str] = _build_opcode_table()

# Opcodes that hand control to another contract. `nexti` steps over these; `stepi` enters.
CALL_OPCODES = frozenset(
    op
    for op, name in OPCODES.items()
    if name in {"CALL", "CALLCODE", "DELEGATECALL", "STATICCALL", "CREATE", "CREATE2"}
)

# Opcodes that end a frame.
HALT_OPCODES = frozenset(
    op
    for op, name in OPCODES.items()
    if name in {"STOP", "RETURN", "REVERT", "SELFDESTRUCT", "INVALID"}
)


def mnemonic(opcode: int) -> str:
    return OPCODES.get(opcode, f"UNKNOWN_0x{opcode:02x}")


@dataclass(frozen=True)
class Instruction:
    pc: int
    opcode: int
    mnemonic: str
    immediate: bytes | None = None

    @property
    def size(self) -> int:
        return 1 + (len(self.immediate) if self.immediate else 0)

    @property
    def operand(self) -> int | None:
        if self.immediate is None:
            return None
        return int.from_bytes(self.immediate, "big")

    def render(self) -> str:
        if self.immediate is None:
            return self.mnemonic
        return f"{self.mnemonic} 0x{self.immediate.hex()}"

    def __str__(self) -> str:
        return f"{self.pc:04x}  {self.render()}"


def disassemble(code: bytes) -> list[Instruction]:
    """Linear sweep. Immediates are consumed, so `pc` is always a real instruction."""
    out: list[Instruction] = []
    pc = 0
    n = len(code)
    while pc < n:
        op = code[pc]
        if PUSH1 <= op <= PUSH32:
            width = op - PUSH0
            imm = code[pc + 1 : pc + 1 + width]
            # Truncated trailing PUSH (common in the metadata tail) is padded, not dropped.
            if len(imm) < width:
                imm = imm + b"\x00" * (width - len(imm))
            out.append(Instruction(pc, op, mnemonic(op), imm))
            pc += 1 + width
        else:
            out.append(Instruction(pc, op, mnemonic(op)))
            pc += 1
    return out


class Disassembly:
    """Indexed disassembly of one code object."""

    def __init__(self, code: bytes) -> None:
        self.code = code
        self.instructions = disassemble(code)
        self.by_pc: dict[int, Instruction] = {i.pc: i for i in self.instructions}
        self._order: dict[int, int] = {i.pc: n for n, i in enumerate(self.instructions)}
        self.jumpdests: set[int] = {
            i.pc for i in self.instructions if i.opcode == JUMPDEST
        }

    def at(self, pc: int) -> Instruction | None:
        return self.by_pc.get(pc)

    def index_of(self, pc: int) -> int | None:
        return self._order.get(pc)

    def window(self, pc: int, before: int = 4, after: int = 12) -> list[Instruction]:
        """Instructions around `pc`, for the disassembly pane."""
        idx = self._order.get(pc)
        if idx is None:
            return self.instructions[: before + after]
        return self.instructions[max(0, idx - before) : idx + after]

    def is_valid_jumpdest(self, pc: int) -> bool:
        return pc in self.jumpdests

    def __len__(self) -> int:
        return len(self.instructions)
