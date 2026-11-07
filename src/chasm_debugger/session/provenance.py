"""Operand provenance: which entry slot, constant, calldata word or memory write
produced the value sitting in a stack slot right now.

The question a JOP/gadget investigation actually asks — "why did operand N of this
CALL get this value" — is answered with a recorder plus a backward slice:

  * recording is opt-in (`provenance on`, or `provenance=true` at session start):
    every executed opcode appends its pc, mnemonic, gas and the stack before and
    after it. Pure appends of immutable tuples; off by default so normal
    debugging pays nothing.
  * `why $stack[n]` walks those records newest→oldest from the queried slot:
    PUSH resolves to a constant, DUP/SWAP to the slot they copied, a consuming
    opcode rewires to the operands it popped, MLOAD chains to the last MSTORE of
    the same offset (or "memory at entry"), SLOAD likewise through SSTORE,
    CALLDATALOAD names its calldata window. Surviving positions at the frame's
    first record are the frame-entry slots — the reviewer's "entry slot >= N".

The slice is concrete (it follows recorded values), not a symbolic executor:
repeating values are tracked by position, and a hand mutation (`set $stack[i]`)
is not recorded, so ask `why` before mutating, or expect the mutated slot's
origin to be reported as unknown.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any

_DUP_RE = re.compile(r"DUP(\d+)$")
_SWAP_RE = re.compile(r"SWAP(\d+)$")
# Opcodes whose single pushed result carries a named external origin.
_ORIGIN_OPS = {
    "CALLDATALOAD": "calldata",
    "MLOAD": "memory",
    "SLOAD": "storage",
    "SHA3": "keccak",
    "KECCAK256": "keccak",
    "CALL": "call-output",
    "CALLCODE": "call-output",
    "DELEGATECALL": "call-output",
    "STATICCALL": "call-output",
    "CREATE": "create",
    "CREATE2": "create",
    "CODECOPY": "codecopy",
    "EXTCODECOPY": "extcodecopy",
    "RETURNDATACOPY": "returndatacopy",
    "CALLDATACOPY": "calldatacopy",
}

# (pops, pushes) per mnemonic — evm.codes semantics. DUP/SWAP are handled
# structurally (copies/permutations) and PUSH* resolve to constants, so they are
# absent. Unknown mnemonics fall back to the height delta, which is only right
# for net-consumers; the table keeps every balanced op (MLOAD, ADD, ...) honest.
_ARITY: dict[str, tuple[int, int]] = {
    "STOP": (0, 0),
    "INVALID": (0, 0),
    "ADD": (2, 1),
    "SUB": (2, 1),
    "MUL": (2, 1),
    "DIV": (2, 1),
    "SDIV": (2, 1),
    "MOD": (2, 1),
    "SMOD": (2, 1),
    "ADDMOD": (3, 1),
    "MULMOD": (3, 1),
    "EXP": (2, 1),
    "SIGNEXTEND": (2, 1),
    "LT": (2, 1),
    "GT": (2, 1),
    "SLT": (2, 1),
    "SGT": (2, 1),
    "EQ": (2, 1),
    "AND": (2, 1),
    "OR": (2, 1),
    "XOR": (2, 1),
    "NOT": (1, 1),
    "BYTE": (2, 1),
    "SHL": (2, 1),
    "SHR": (2, 1),
    "SAR": (2, 1),
    "KECCAK256": (2, 1),
    "SHA3": (2, 1),
    "ADDRESS": (0, 1),
    "BALANCE": (1, 1),
    "ORIGIN": (0, 1),
    "CALLER": (0, 1),
    "CALLVALUE": (0, 1),
    "CALLDATASIZE": (0, 1),
    "CODESIZE": (0, 1),
    "GASPRICE": (0, 1),
    "EXTCODESIZE": (1, 1),
    "EXTCODEHASH": (1, 1),
    "BLOCKHASH": (1, 1),
    "COINBASE": (0, 1),
    "TIMESTAMP": (0, 1),
    "NUMBER": (0, 1),
    "PREVRANDAO": (0, 1),
    "DIFFICULTY": (0, 1),
    "GASLIMIT": (0, 1),
    "CHAINID": (0, 1),
    "SELFBALANCE": (0, 1),
    "BASEFEE": (0, 1),
    "BLOBHASH": (1, 1),
    "BLOBBASEFEE": (0, 1),
    "POP": (1, 0),
    "MLOAD": (1, 1),
    "MSTORE": (2, 0),
    "MSTORE8": (2, 0),
    "SLOAD": (1, 1),
    "SSTORE": (2, 0),
    "TLOAD": (1, 1),
    "TSTORE": (2, 0),
    "JUMP": (2, 0),
    "JUMPI": (3, 0),
    "PC": (0, 1),
    "MSIZE": (0, 1),
    "GAS": (0, 1),
    "CALLDATALOAD": (1, 1),
    "CALLDATACOPY": (3, 0),
    "CODECOPY": (3, 0),
    "EXTCODECOPY": (4, 0),
    "RETURNDATACOPY": (3, 0),
    "MCOPY": (3, 0),
    "RETURN": (2, 0),
    "REVERT": (2, 0),
    "RETURNDATASIZE": (0, 1),
    "SELFDESTRUCT": (1, 0),
    "CREATE": (3, 1),
    "CALL": (7, 1),
    "CALLCODE": (7, 1),
    "DELEGATECALL": (6, 1),
    "STATICCALL": (6, 1),
    "LOG0": (2, 0),
    "LOG1": (3, 0),
    "LOG2": (4, 0),
    "LOG3": (5, 0),
    "LOG4": (6, 0),
    "PUSH0": (0, 1),
}

MAX_FRONTIER = 64
MAX_ORIGINS = 128


@dataclass(frozen=True)
class StepRecord:
    step: int
    pc: int
    opcode: int
    mnemonic: str
    depth: int
    gas_before: int
    gas_remaining: int
    mem_size: int
    before: tuple  # stack bottom-up; values[-1] is the top
    after: tuple


class Provenance:
    """The per-session recorder; the slice reads it from the controller thread."""

    def __init__(self) -> None:
        self.enabled = False
        self.records: list[StepRecord] = []

    def record(
        self,
        step: int,
        pc: int,
        opcode: int,
        mnemonic: str,
        depth: int,
        gas_before: int,
        gas_remaining: int,
        mem_size: int,
        before: list[Any],
        after: list[Any],
    ) -> None:
        if not self.enabled:
            return
        self.records.append(
            StepRecord(
                step=step,
                pc=pc,
                opcode=opcode,
                mnemonic=mnemonic,
                depth=depth,
                gas_before=gas_before,
                gas_remaining=gas_remaining,
                mem_size=mem_size,
                before=tuple(before),
                after=tuple(after),
            )
        )

    def truncate(self, length: int) -> None:
        """Drop records past a checkpoint's saved length (restore rolls time back)."""
        del self.records[length:]

    def clear(self) -> None:
        self.records.clear()

    # -- structLog export (shares the recording) ---------------------------

    def structlog(self, offset: int = 0, limit: int = 500) -> dict[str, Any]:
        """anvil/geth `debug_traceTransaction`-shaped rows, windowed."""
        rows = [
            {
                "pc": hex(r.pc),
                "op": r.mnemonic,
                "gas": hex(r.gas_before),
                "gasCost": hex(max(0, r.gas_before - r.gas_remaining)),
                "depth": r.depth,
                "stack": [
                    _word(v)
                    if not isinstance(v, bytes)
                    else "0x" + v.hex().rjust(64, "0")
                    for v in r.before
                ],
                "memSize": r.mem_size,
                "step": r.step,
            }
            for r in self.records[offset : offset + limit]
        ]
        return {
            "structLogs": rows,
            "total": len(self.records),
            "offset": offset,
            "truncated": offset + limit < len(self.records),
        }


# -- the backward slice -------------------------------------------------------


@dataclass
class _Target:
    position: int  # bottom-up index into some record's before-stack
    via: list[tuple[int, str]] = field(default_factory=list)  # (pc, mnemonic)


@dataclass
class Origin:
    kind: str
    detail: str = ""
    value: str | None = None
    via: list[tuple[int, str]] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "detail": self.detail,
            "value": self.value,
            "via": [{"pc": pc, "op": op} for pc, op in self.via],
        }


def _num(value: Any) -> int:
    """Stack values are ints or bytes; slices need them comparable as ints."""
    if isinstance(value, (bytes, bytearray)):
        return int.from_bytes(value, "big")
    return int(value)


def _word(value: Any) -> str:
    """A 32-byte word as 0x + 64 hex chars; bytes and ints both occur on the stack."""
    if isinstance(value, (bytes, bytearray)):
        return "0x" + bytes(value).rjust(32, b"\x00").hex()
    try:
        return f"0x{int(value):064x}"
    except (TypeError, ValueError):
        return str(value)


def _frame_tail(records: list[StepRecord], depth: int) -> list[StepRecord]:
    """The contiguous newest run of records at one depth: this frame's execution."""
    tail: list[StepRecord] = []
    for record in reversed(records):
        if record.depth != depth:
            break
        tail.append(record)
    tail.reverse()
    return tail


def _last_store(
    tail: list[StepRecord], before_index: int, mnemonic: str, offset_value: int
) -> tuple[int, int, int] | None:
    """The newest record < before_index writing `mnemonic` with `offset_value`.

    MSTORE/SSTORE pop (offset, value): offset second-from-top, value top of the
    record's before-stack. Returns (record_index, offset_pos, value_pos).
    """
    for index in range(before_index - 1, -1, -1):
        record = tail[index]
        if record.mnemonic != mnemonic:
            continue
        height = len(record.before)
        offset_pos, value_pos = height - 2, height - 1
        if offset_pos < 0 or _num(record.before[offset_pos]) != offset_value:
            continue
        return index, offset_pos, value_pos
    return None


def why_stack(
    records: list[StepRecord], depth: int, stack_bottom_up: tuple, index_from_top: int
) -> dict[str, Any]:
    """Trace one current stack slot back to its origins.

    `stack_bottom_up` has the top value last; `index_from_top` is the gdb
    convention ($stack[0] = top).
    """
    if not records:
        return {"origins": [{"kind": "unknown", "detail": "recording is off"}]}
    sp = len(stack_bottom_up)
    if not 0 <= index_from_top < sp:
        return {"error": f"stack index {index_from_top} out of range (sp={sp})"}

    tail = _frame_tail(records, depth)
    if not tail:
        return {"origins": [{"kind": "unknown", "detail": "no records at this depth"}]}

    latest = tail[-1]
    start_position = len(latest.after) - 1 - index_from_top
    frontier: dict[int, _Target] = {
        start_position: _Target(start_position, [(latest.pc, latest.mnemonic)])
    }
    origins: list[Origin] = []
    entry_stack: tuple = ()

    for record_index in range(len(tail) - 1, -1, -1):
        record = tail[record_index]
        before, after = record.before, record.after
        height_before, height_after = len(before), len(after)
        dup = _DUP_RE.match(record.mnemonic)
        swap = _SWAP_RE.match(record.mnemonic)
        if dup is None and swap is None:
            pops, _pushes = _ARITY.get(record.mnemonic, (None, None))
            if pops is None:
                # Unknown mnemonic: approximate with the height delta. Right for
                # net-consumers, wrong for balanced unknowns — the table covers
                # every opcode that can appear in real bytecode.
                pops = height_before - height_after if height_before > height_after else 0
            pushed_base = height_before - pops
        else:
            pops = 0
            pushed_base = height_before

        next_frontier: dict[int, _Target] = {}
        for position in sorted(frontier):
            target = frontier[position]

            if dup is not None and height_after == height_before + 1:
                k = int(dup.group(1))
                source = height_before - k
                _carry(
                    next_frontier,
                    source if position == height_before else position,
                    target,
                    record,
                )

            elif swap is not None and height_after == height_before:
                k = int(swap.group(1))
                top, other = height_before - 1, height_before - 1 - k
                if position == top:
                    _carry(next_frontier, other, target, record)
                elif position == other:
                    _carry(next_frontier, top, target, record)
                else:
                    _carry(next_frontier, position, target)

            elif position < pushed_base:
                _carry(next_frontier, position, target)

            else:
                via = [(record.pc, record.mnemonic), *target.via]
                value = after[position] if position < height_after else None
                origin_kind = _ORIGIN_OPS.get(record.mnemonic)

                if record.mnemonic.startswith("PUSH"):
                    origins.append(Origin("constant", _word(value), _word(value), via))
                    continue

                if origin_kind in ("memory", "storage"):
                    offset_pos = height_before - pops
                    offset_value = (
                        _num(before[offset_pos])
                        if 0 <= offset_pos < height_before
                        else -1
                    )
                    detail = f"{origin_kind}[{_word(offset_value)}]"
                    store = _last_store(
                        tail,
                        record_index,
                        "SSTORE" if origin_kind == "storage" else "MSTORE",
                        offset_value,
                    )
                    if store is None:
                        origins.append(
                            Origin(f"{origin_kind}-entry", detail, _word(value), via)
                        )
                    else:
                        store_index, _off, value_pos = store
                        origins.append(
                            Origin(
                                origin_kind,
                                f"{detail}, written by "
                                f"{tail[store_index].mnemonic} at pc "
                                f"0x{tail[store_index].pc:04x}",
                                _word(value),
                                via,
                            )
                        )
                        # Chain through the storing record's value operand.
                        _carry(next_frontier, value_pos, _Target(value_pos, via))
                    if 0 <= offset_pos < height_before:
                        _carry(next_frontier, offset_pos, _Target(offset_pos, via))
                    continue

                if origin_kind is not None:
                    detail = f"{record.mnemonic} result"
                    if origin_kind == "calldata":
                        offset_pos = height_before - pops
                        if 0 <= offset_pos < height_before:
                            detail = f"calldata[{_word(_num(before[offset_pos]))}..+0x20]"
                    origins.append(Origin(origin_kind, detail, _word(value), via))

                for consumed_pos in range(pushed_base, height_before):
                    _carry(next_frontier, consumed_pos, _Target(consumed_pos, via))

        frontier = next_frontier
        if record_index == 0:
            entry_stack = before
        if not frontier or len(origins) >= MAX_ORIGINS:
            break

    for position, target in frontier.items():
        from_top = len(entry_stack) - 1 - position if entry_stack else position
        origins.append(
            Origin(
                "entry-slot",
                f"frame-entry stack, {from_top} from top (absolute position {position})",
                _word(entry_stack[position])
                if entry_stack and position < len(entry_stack)
                else None,
                target.via,
            )
        )

    return {
        "origins": [origin.as_dict() for origin in origins[:MAX_ORIGINS]],
        "records_examined": len(tail),
        "truncated": len(origins) > MAX_ORIGINS,
    }


def _carry(
    frontier: dict[int, _Target],
    position: int,
    target: _Target,
    record: StepRecord | None = None,
) -> None:
    if position < 0:
        return
    via = (
        [(record.pc, record.mnemonic), *target.via] if record is not None else target.via
    )
    existing = frontier.get(position)
    if existing is None:
        frontier[position] = _Target(position, via)
    # Positions already pending keep their first chain; the merge is lossy on
    # purpose — one representative path per slot keeps the answer bounded.
