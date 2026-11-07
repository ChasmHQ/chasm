"""Decoding `console.log` payloads.

forge-std's console.sol declares one overload per combination of four types, up to four
arguments, plus a few single-argument extras. The table is generated rather than listed:
it is 340 entries and a missing one silently swallows the user's log line.
"""

from __future__ import annotations

from eth_abi import decode as abi_decode
from eth_utils import function_signature_to_4byte_selector

from .registry import _arg_types, _fmt

# Address of forge-std's console.sol ("console.log" in ascii, right-padded).
CONSOLE_ADDRESS = bytes.fromhex("000000000000000000636F6e736F6c652e6c6f67".lower())

# forge-std's console.sol declares one overload per combination of these four types, up to
# four arguments, plus a few single-argument extras. Generated rather than listed: the
# table is 340 entries and a missing one silently swallows the user's log line.
_CONSOLE_COMBO_TYPES = ("uint256", "string", "bool", "address")
_CONSOLE_EXTRA_SIGS = (
    "log()",
    "log(int256)",
    "log(bytes)",
    "log(bytes32)",
    "logs(bytes)",
    "logInt(int256)",
    "logUint(uint256)",
    "logString(string)",
    "logBool(bool)",
    "logAddress(address)",
    "logBytes(bytes)",
    "logBytes32(bytes32)",
)


def _console_table() -> dict[bytes, list[str]]:
    table: dict[bytes, list[str]] = {}
    current: list[tuple[str, ...]] = [(t,) for t in _CONSOLE_COMBO_TYPES]
    for _ in range(4):
        for combo in current:
            sig = f"log({','.join(combo)})"
            table[function_signature_to_4byte_selector(sig)] = list(combo)
        current = [(*c, t) for c in current for t in _CONSOLE_COMBO_TYPES]
    for sig in _CONSOLE_EXTRA_SIGS:
        table[function_signature_to_4byte_selector(sig)] = _arg_types(sig)
    return table


_CONSOLE_TABLE: dict[bytes, list[str]] = _console_table()


def decode_console_log(calldata: bytes) -> str | None:
    """Decode a console.log payload to a printable line, or None if unrecognized."""
    if len(calldata) < 4:
        return None
    types = _CONSOLE_TABLE.get(calldata[:4])
    if types is None:
        return None
    try:
        values = abi_decode(types, calldata[4:])
    except Exception:
        return None
    return " ".join(_fmt(v) for v in values)
