"""The decoding half: a stack word plus a type becomes a value.

Nothing here guesses. A local whose slot was never observed, or that is outside its scope,
reports `<unavailable>` with a reason: a plausible wrong number is worse than a gap.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from typing import Any

from .index import KIND_LOCAL, LocalVar
from .layout import _base_type, _is_value_type


@dataclass
class LocalValue:
    """One row of `info locals`."""

    name: str
    type_label: str
    display: str
    available: bool = True
    reason: str = ""
    kind: str = KIND_LOCAL
    # Set when the value can be handed to the evaluator as a function argument.
    abi_type: str = ""
    abi_value: Any = None
    words: tuple[int, ...] = ()
    position: int | None = None  # absolute stack position of the first word
    # True only when the stack word *is* the value. A memory or calldata local holds a
    # pointer, so writing its slot corrupts the reference instead of changing the value.
    writable: bool = False

    @property
    def bindable(self) -> bool:
        return self.available and bool(self.name) and bool(self.abi_type)


MemoryReader = Callable[[int, int], bytes]


def _word(value: int) -> bytes:
    return int(value).to_bytes(32, "big")


def _format_int(value: int, type_label: str) -> str:
    if type_label.startswith("uint") and 10**15 <= value < 10**27:
        whole, frac = divmod(value, 10**18)
        if frac == 0:
            return f"{value} ({whole} ether)"
        return f"{value} ({value / 10**18:.6f} ether)"
    return str(value)


def decode_value_type(word: int, type_string: str) -> tuple[str, str, Any]:
    """(display, abi_type, python value) for a type that lives entirely in one word."""
    t = _base_type(type_string)
    raw = _word(word)
    if t == "bool":
        val = word != 0
        return ("true" if val else "false", "bool", val)
    if t in ("address", "address payable") or t.startswith("contract "):
        addr = "0x" + raw[-20:].hex()
        return (addr, "address", addr)
    match = re.fullmatch(r"bytes([1-9]|[12][0-9]|3[0-2])", t)
    if match:
        size = int(match.group(1))
        chunk = raw[:size]  # bytesN is left-aligned on the stack
        return ("0x" + chunk.hex(), t, chunk)
    if t.startswith("enum "):
        return (str(word), "uint256", word)
    if t.startswith("int") and not t.startswith("uint"):
        signed = int.from_bytes(raw, "big", signed=True)
        width = t[3:] or "256"
        return (str(signed), f"int{width}", signed)
    width = (t[4:] or "256") if t.startswith("uint") else "256"
    return (_format_int(word, t), f"uint{width}", word)


def _read_memory_bytes(read: MemoryReader, pointer: int) -> bytes:
    length = int.from_bytes(read(pointer, 32), "big")
    if length > 1 << 20:  # a pointer into uninitialised memory, not a real string
        raise ValueError(f"implausible length {length}")
    return read(pointer + 32, length)


def _decode_memory(
    read: MemoryReader, pointer: int, type_string: str, depth: int = 0
) -> tuple[str, str, Any]:
    """Decode a memory reference type by following its pointer.

    Solidity's memory layout is uniform: a dynamic array/string is a length word
    followed by elements; a struct/fixed array is its members in order; every member is
    one word holding either a value or another pointer.
    """
    t = _base_type(type_string)
    if depth > 3:
        raise ValueError("nested too deeply to display")

    if t in ("string", "bytes"):
        data = _read_memory_bytes(read, pointer)
        if t == "string":
            try:
                text = data.decode("utf-8")
                return (f'"{text}"', "string", text)
            except UnicodeDecodeError:
                pass
        return ("0x" + data.hex(), "bytes", data)

    array = re.fullmatch(r"(.+)\[(\d*)\]", t)
    if array:
        element, fixed = array.group(1).strip(), array.group(2)
        if fixed:
            count, base = int(fixed), pointer
        else:
            count = int.from_bytes(read(pointer, 32), "big")
            base = pointer + 32
        if count > 256:
            raise ValueError(f"{count} elements is too many to display")
        displays, values = [], []
        for i in range(count):
            word = int.from_bytes(read(base + 32 * i, 32), "big")
            if _is_value_type(element):
                shown, abi_element, value = decode_value_type(word, element)
            else:
                shown, abi_element, value = _decode_memory(read, word, element, depth + 1)
            displays.append(shown)
            values.append(value)
        abi_element = decode_value_type(0, element)[1] if _is_value_type(element) else ""
        suffix = f"[{fixed}]" if fixed else "[]"
        abi_type = f"{abi_element}{suffix}" if abi_element else ""
        return (f"[{count} items] [" + ", ".join(displays) + "]", abi_type, values)

    raise ValueError(f"cannot decode {t} from memory yet")


def read_local(
    var: LocalVar,
    words: Sequence[int],
    read_memory: MemoryReader,
) -> LocalValue:
    """Turn the raw stack words of one local into a displayable, bindable value."""
    name = var.name or f"<{var.kind}>"
    label = var.display_type

    if var.location == "storage":
        slot = words[0]
        shown = f"0x{slot:x}" if slot > 0xFFFF else str(slot)
        return LocalValue(
            name=name,
            type_label=label,
            display=f"<storage pointer -> slot {shown}>",
            available=True,
            reason="storage pointers are not dereferenced; index the state variable instead",
            kind=var.kind,
            words=tuple(words),
        )

    if var.location == "calldata":
        return LocalValue(
            name=name,
            type_label=label,
            display="<calldata reference: " + ", ".join(f"0x{w:x}" for w in words) + ">",
            available=True,
            reason="calldata references are not decoded; `info args` decodes the call's arguments",
            kind=var.kind,
            words=tuple(words),
        )

    if _is_value_type(var.type_string):
        display, abi_type, value = decode_value_type(words[0], var.type_string)
        return LocalValue(
            name=name,
            type_label=label,
            display=display,
            kind=var.kind,
            abi_type=abi_type,
            abi_value=value,
            words=tuple(words),
            writable=True,
        )

    if var.location == "memory":
        try:
            display, abi_type, value = _decode_memory(
                read_memory, words[0], var.type_string
            )
        except Exception as exc:
            return LocalValue(
                name=name,
                type_label=label,
                display="<unavailable>",
                available=False,
                reason=str(exc),
                kind=var.kind,
                words=tuple(words),
            )
        return LocalValue(
            name=name,
            type_label=label,
            display=display,
            kind=var.kind,
            abi_type=abi_type,
            abi_value=value,
            words=tuple(words),
        )

    return LocalValue(
        name=name,
        type_label=label,
        display="<unavailable>",
        available=False,
        reason=f"no decoder for {label}",
        kind=var.kind,
        words=tuple(words),
    )
