"""Validate solc CBOR tails before excluding them from source matching."""

from __future__ import annotations

from typing import Any


def _item(data: bytes, offset: int, depth: int = 0) -> tuple[Any, int]:
    if depth > 8 or offset >= len(data):
        raise ValueError("invalid CBOR item")
    initial = data[offset]
    offset += 1
    major, size = initial >> 5, initial & 31
    if size >= 24:
        width = {24: 1, 25: 2, 26: 4, 27: 8}.get(size)
        if width is None or offset + width > len(data):
            raise ValueError("invalid CBOR length")
        size = int.from_bytes(data[offset : offset + width], "big")
        offset += width
    if major in (0, 1):
        return (size if major == 0 else -1 - size), offset
    if major in (2, 3):
        end = offset + size
        if end > len(data):
            raise ValueError("truncated CBOR string")
        value = data[offset:end]
        return (value if major == 2 else value.decode("utf-8")), end
    if major == 5:
        if size > len(data) // 2:
            raise ValueError("invalid CBOR map")
        entries = {}
        for _ in range(size):
            key, offset = _item(data, offset, depth + 1)
            value, offset = _item(data, offset, depth + 1)
            if not isinstance(key, str) or key in entries:
                raise ValueError("invalid metadata key")
            entries[key] = value
        return entries, offset
    if major == 7 and initial in (0xF4, 0xF5):
        return initial == 0xF5, offset
    raise ValueError("unsupported CBOR item")


def strip_metadata(code: bytes) -> bytes:
    if len(code) < 3:
        return code
    size = int.from_bytes(code[-2:], "big")
    if not 0 < size <= len(code) - 2:
        return code
    tail = code[-size - 2 : -2]
    try:
        value, end = _item(tail, 0)
    except (UnicodeError, ValueError):
        return code
    if not isinstance(value, dict) or end != len(tail) or not value:
        return code
    if not set(value) <= {"solc", "ipfs", "bzzr0", "bzzr1", "experimental"}:
        return code
    if not set(value) & {"solc", "ipfs", "bzzr0", "bzzr1"}:
        return code
    for key, entry in value.items():
        if key == "solc":
            if not (isinstance(entry, bytes) and len(entry) == 3) and not isinstance(
                entry, str
            ):
                return code
        elif key == "experimental":
            if not isinstance(entry, bool):
                return code
        elif not isinstance(entry, bytes) or len(entry) != (34 if key == "ipfs" else 32):
            return code
    return code[: -size - 2]
