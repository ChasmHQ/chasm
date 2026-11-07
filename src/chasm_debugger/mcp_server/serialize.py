"""Value formatting for the MCP surface.

An AI consumer cannot scroll a pane or eyeball colour, so every value leaves the
debugger as a stable, markup-free primitive: full-width hex words for 32-byte
values, decimal for counts, and explicit bounds on every windowed read. The
helpers here are the single source of that formatting.
"""

from __future__ import annotations

from typing import Any

from ..commands.render import _memory_region
from ..frames import FrameSnapshot, StackEntry

WORD = 32


def hex_word(value: int) -> str:
    """A 32-byte EVM word as 0x + 64 hex chars — the width the model should expect."""
    return f"0x{value & ((1 << 256) - 1):064x}"


def hex_short(raw: bytes, width: int = 20) -> str:
    data = bytes(raw)
    if len(data) > width:
        data = data[:width]
    return "0x" + data.hex()


def describe_wei(value: int) -> str:
    """`1000000000000000000 (1 ether)` — exact decimal plus the human unit."""
    if value and value % 10**18 == 0:
        return f"{value} ({value // 10**18} ether)"
    return str(value)


def stack_entry(entry: StackEntry) -> dict[str, Any]:
    return {
        "index": entry.index,  # 0 = top of stack
        "hex": entry.hex()
        if callable(getattr(entry, "hex", None))
        else hex_word(entry.value),
        "decimal": entry.value if abs(entry.value) < 1 << 64 else None,
    }


def memory_window(data: bytes, offset: int, memory_size: int) -> list[dict[str, Any]]:
    """Chunk a byte window into 32-byte words, annotated the way the TUI annotates.

    `beyond` marks words past the current allocation: they read as zero (EVM
    semantics) but nothing has been written there yet, and a model scanning a dump
    must not mistake the two.
    """
    items = []
    for i in range(0, len(data), WORD):
        word_offset = offset + i
        chunk = data[i : i + WORD]
        value = int.from_bytes(chunk.ljust(WORD, b"\x00"), "big")
        region = _memory_region(word_offset)
        item: dict[str, Any] = {"offset": word_offset, "hex": f"0x{value:064x}"}
        if region:
            item["region"] = region
        if word_offset >= memory_size:
            item["beyond"] = True
        items.append(item)
    return items


def snapshot_report(snap: FrameSnapshot, stack_top: int = 5) -> dict[str, Any]:
    """The uniform 'where am I' dict every navigation tool returns.

    Everything here comes straight off the immutable snapshot — no inspect
    round-trip, no markup — so the shape is identical no matter which tool
    produced the stop.
    """
    fn = snap.function
    location: dict[str, Any] = {"contract": snap.contract_name}
    if fn is not None:
        location["function"] = fn.signature
    if snap.has_source:
        location["file"] = snap.source_key
        location["line"] = snap.line
    return {
        "stopped": True,
        "stop_reason": snap.stop_reason,
        "hit_breakpoints": list(snap.hit_breakpoints),
        "pc": snap.pc,
        "opcode": f"0x{snap.opcode:02x}",
        "mnemonic": snap.mnemonic,
        "gas": {
            "remaining": snap.gas_remaining,
            "used": snap.gas_used,
            "limit": snap.gas_limit,
            "refund": snap.gas_refund,
        },
        "sp": len(snap.stack),
        "depth": snap.depth,
        "address": hex_short(snap.address),
        "code_address": hex_short(snap.code_address),
        "is_static": snap.is_static,
        "step": snap.step,
        "memory_size": snap.memory_size,
        "calldata_size": len(snap.calldata),
        "location": location,
        "annotation": snap.annotation or None,
        "stack_top": [stack_entry(e) for e in snap.stack[:stack_top]],
    }


def backtrace_rows(snap: FrameSnapshot) -> list[dict[str, Any]]:
    rows = []
    for row in snap.backtrace:
        rows.append(
            {
                "index": row.index,
                "name": row.name,
                "kind": row.kind,  # solidity | evm
                "file": row.source_key,
                "line": row.line or None,
                "pc": row.pc,
                "detail": row.detail or None,
            }
        )
    return rows
