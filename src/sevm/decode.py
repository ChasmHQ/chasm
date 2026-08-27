"""Decoding EVM state into Solidity values.

Two decoders live here:

  StorageDecoder  drives the STORAGE pane. It walks solc's `storageLayout` output, so it
                  handles packed slots, structs, fixed and dynamic arrays, and the short
                  and long forms of `string`/`bytes` without guessing.
  ABI helpers     decode calldata against a contract's ABI and turn revert output back
                  into `Error(string)` / `Panic(uint256)` / a custom error.

Mappings deliberately have no "show me everything" path: their keys are not recoverable
from storage. The pane shows the declaration and the user reads individual entries with
`p balances[addr]`, which goes through the Solidity evaluator instead.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from eth_abi import decode as abi_decode
from eth_utils import keccak

SlotReader = Callable[[int], int]

# Cap on how much of a dynamic container we walk before summarising.
MAX_ELEMENTS = 64

PANIC_CODES = {
    0x00: "generic compiler panic",
    0x01: "assert(false)",
    0x11: "arithmetic overflow or underflow",
    0x12: "division or modulo by zero",
    0x21: "invalid enum conversion",
    0x22: "invalid storage byte array encoding",
    0x31: "pop() on an empty array",
    0x32: "array index out of bounds",
    0x41: "out of memory",
    0x51: "call to an uninitialised internal function",
}


@dataclass
class DecodedValue:
    """A decoded Solidity value plus how it should be shown."""

    type_label: str
    value: Any
    display: str
    slot: int | None = None
    offset: int = 0
    children: list[DecodedValue] = field(default_factory=list)
    truncated: bool = False

    def __str__(self) -> str:
        return self.display


@dataclass
class StateVariable:
    name: str
    type_id: str
    type_label: str
    slot: int
    offset: int
    contract: str


def _slot_bytes(reader: SlotReader, slot: int) -> bytes:
    return int(reader(slot)).to_bytes(32, "big")


def format_address(raw: bytes) -> str:
    return "0x" + raw[-20:].hex()


def _decode_primitive(
    label: str, word: bytes, offset: int, num_bytes: int
) -> DecodedValue:
    """Extract a packed value from its slot word.

    Storage packs right-aligned inside the 32-byte word, with `offset` counted from the
    low-order end, so the value occupies word[32-offset-num_bytes : 32-offset].
    """
    end = 32 - offset
    start = end - num_bytes
    chunk = word[start:end]

    if label == "bool":
        val = chunk[-1] != 0 if chunk else False
        return DecodedValue(label, val, "true" if val else "false")
    if label == "address" or label.startswith("contract "):
        addr = "0x" + chunk[-20:].hex()
        return DecodedValue(label, addr, addr)
    if label.startswith("bytes") and label != "bytes":
        # bytesN is LEFT-aligned inside its packed region.
        return DecodedValue(label, chunk, "0x" + chunk.hex())
    if label.startswith("int") and not label.startswith("uint"):
        val = int.from_bytes(chunk, "big", signed=True)
        return DecodedValue(label, val, str(val))
    if label.startswith("enum "):
        val = int.from_bytes(chunk, "big")
        return DecodedValue(label, val, str(val))
    val = int.from_bytes(chunk, "big")
    return DecodedValue(label, val, str(val))


class StorageDecoder:
    """Reads a contract's state variables out of storage using solc's layout output."""

    def __init__(self, storage_layout: dict | None) -> None:
        layout = storage_layout or {}
        self.types: dict[str, dict] = layout.get("types") or {}
        self.entries: list[dict] = layout.get("storage") or []
        self.variables: list[StateVariable] = [
            StateVariable(
                name=e["label"],
                type_id=e["type"],
                type_label=(self.types.get(e["type"], {}) or {}).get("label", e["type"]),
                slot=int(e["slot"]),
                offset=int(e.get("offset", 0)),
                contract=e.get("contract", ""),
            )
            for e in self.entries
        ]
        self._by_name = {v.name: v for v in self.variables}

    def __bool__(self) -> bool:
        return bool(self.variables)

    def get(self, name: str) -> StateVariable | None:
        return self._by_name.get(name)

    # -- decoding -----------------------------------------------------------

    def read_variable(self, reader: SlotReader, var: StateVariable) -> DecodedValue:
        value = self._read(reader, var.type_id, var.slot, var.offset)
        value.slot = var.slot
        value.offset = var.offset
        return value

    def read_all(self, reader: SlotReader) -> list[tuple[StateVariable, DecodedValue]]:
        out = []
        for var in self.variables:
            try:
                out.append((var, self.read_variable(reader, var)))
            except Exception as exc:
                out.append((var, DecodedValue(var.type_label, None, f"<error: {exc}>")))
        return out

    def _read(
        self, reader: SlotReader, type_id: str, slot: int, offset: int, depth: int = 0
    ) -> DecodedValue:
        info = self.types.get(type_id)
        if info is None:
            return DecodedValue(type_id, None, "<unknown type>")
        encoding = info.get("encoding", "inplace")
        label = info.get("label", type_id)
        num_bytes = int(info.get("numberOfBytes", 32))

        if depth > 6:
            return DecodedValue(label, None, "<nested too deep>")

        if encoding == "mapping":
            return DecodedValue(label, None, "<mapping: query a key>")

        if encoding == "bytes":
            return self._read_bytes_like(reader, slot, label)

        if encoding == "dynamic_array":
            return self._read_dynamic_array(reader, info, slot, depth)

        # encoding == "inplace": struct, fixed array, or a plain packed value.
        if "members" in info:
            return self._read_struct(reader, info, slot, depth)
        if "base" in info:
            return self._read_fixed_array(reader, info, slot, num_bytes, depth)

        word = _slot_bytes(reader, slot)
        return _decode_primitive(label, word, offset, min(num_bytes, 32))

    def _read_struct(
        self, reader: SlotReader, info: dict, slot: int, depth: int
    ) -> DecodedValue:
        children: list[DecodedValue] = []
        for member in info.get("members", []):
            child = self._read(
                reader,
                member["type"],
                slot + int(member["slot"]),
                int(member.get("offset", 0)),
                depth + 1,
            )
            child.slot = slot + int(member["slot"])
            children.append(
                DecodedValue(
                    child.type_label,
                    child.value,
                    f"{member['label']}: {child.display}",
                    slot=child.slot,
                    offset=child.offset,
                    children=child.children,
                )
            )
        body = ", ".join(c.display for c in children)
        return DecodedValue(
            info.get("label", "struct"), children, "{" + body + "}", children=children
        )

    def _read_fixed_array(
        self, reader: SlotReader, info: dict, slot: int, num_bytes: int, depth: int
    ) -> DecodedValue:
        base_id = info["base"]
        base = self.types.get(base_id, {})
        base_bytes = int(base.get("numberOfBytes", 32))
        count = num_bytes // max(base_bytes, 1)
        return self._read_elements(
            reader, base_id, base_bytes, slot, count, depth, info.get("label", "array")
        )

    def _read_dynamic_array(
        self, reader: SlotReader, info: dict, slot: int, depth: int
    ) -> DecodedValue:
        length = reader(slot)
        base_id = info["base"]
        base = self.types.get(base_id, {})
        base_bytes = int(base.get("numberOfBytes", 32))
        data_slot = int.from_bytes(keccak(slot.to_bytes(32, "big")), "big")
        result = self._read_elements(
            reader,
            base_id,
            base_bytes,
            data_slot,
            length,
            depth,
            info.get("label", "array"),
        )
        result.display = f"[{length} items] " + result.display
        return result

    def _read_elements(
        self,
        reader: SlotReader,
        base_id: str,
        base_bytes: int,
        base_slot: int,
        count: int,
        depth: int,
        label: str,
    ) -> DecodedValue:
        shown = min(count, MAX_ELEMENTS)
        per_slot = max(1, 32 // base_bytes) if base_bytes <= 32 else 1
        children: list[DecodedValue] = []
        for i in range(shown):
            if base_bytes <= 32:
                slot = base_slot + i // per_slot
                offset = (i % per_slot) * base_bytes
            else:
                slot = base_slot + i * (base_bytes // 32)
                offset = 0
            children.append(self._read(reader, base_id, slot, offset, depth + 1))
        body = ", ".join(c.display for c in children)
        if count > shown:
            body += f", ... (+{count - shown} more)"
        return DecodedValue(
            label,
            [c.value for c in children],
            "[" + body + "]",
            children=children,
            truncated=count > shown,
        )

    def _read_bytes_like(self, reader: SlotReader, slot: int, label: str) -> DecodedValue:
        """`string` and `bytes` share the short/long storage trick.

        Short (<32 bytes): data left-aligned in the slot, `2*len` in the low byte.
        Long: the slot holds `2*len+1`, and the data starts at keccak256(slot).
        """
        word = _slot_bytes(reader, slot)
        marker = word[31]
        if marker % 2 == 0:
            length = marker // 2
            data = word[:length]
        else:
            length = (int.from_bytes(word, "big") - 1) // 2
            data_slot = int.from_bytes(keccak(slot.to_bytes(32, "big")), "big")
            chunks = bytearray()
            for i in range((length + 31) // 32):
                chunks += _slot_bytes(reader, data_slot + i)
            data = bytes(chunks[:length])
        if label == "string":
            try:
                text = data.decode("utf-8")
                return DecodedValue(label, text, f'"{text}"')
            except UnicodeDecodeError:
                pass
        return DecodedValue(label, data, "0x" + data.hex())


# -- mapping and array slot arithmetic ---------------------------------------


def mapping_slot(key: Any, base_slot: int, key_is_dynamic: bool = False) -> int:
    """keccak256(h(key) . slot) for value-typed keys; keccak256(key . slot) for string/bytes."""
    if key_is_dynamic:
        key_bytes = key if isinstance(key, bytes) else str(key).encode()
    elif isinstance(key, bytes):
        key_bytes = key.rjust(32, b"\x00")
    elif isinstance(key, str) and key.startswith("0x"):
        key_bytes = bytes.fromhex(key[2:]).rjust(32, b"\x00")
    else:
        key_bytes = int(key).to_bytes(32, "big", signed=int(key) < 0)
    return int.from_bytes(keccak(key_bytes + base_slot.to_bytes(32, "big")), "big")


def dynamic_array_slot(base_slot: int) -> int:
    return int.from_bytes(keccak(base_slot.to_bytes(32, "big")), "big")


# -- ABI helpers -------------------------------------------------------------


def _abi_types(items: Sequence[dict]) -> list[str]:
    out = []
    for item in items:
        if item.get("components"):
            inner = ",".join(_abi_types(item["components"]))
            suffix = item["type"][len("tuple") :]
            out.append(f"({inner}){suffix}")
        else:
            out.append(item["type"])
    return out


def decode_calldata(
    abi: Sequence[dict], calldata: bytes
) -> tuple[str, list[tuple[str, str, Any]]] | None:
    """4-byte selector -> (signature, [(type, name, value)]). None if unrecognised."""
    if len(calldata) < 4:
        return None
    from eth_utils import function_abi_to_4byte_selector

    selector = calldata[:4]
    for entry in abi:
        if entry.get("type") != "function":
            continue
        try:
            if function_abi_to_4byte_selector(entry) != selector:
                continue
        except Exception:
            continue
        inputs = entry.get("inputs", [])
        types = _abi_types(inputs)
        signature = f"{entry['name']}({','.join(types)})"
        if not types:
            return signature, []
        try:
            values = abi_decode(types, calldata[4:])
        except Exception:
            return signature, []
        return signature, [
            (t, inp.get("name", ""), v)
            for t, inp, v in zip(types, inputs, values, strict=False)
        ]
    return None


def decode_revert(output: bytes, abi: Sequence[dict] | None = None) -> str:
    """Turn revert output into something a human can act on."""
    if not output:
        return "reverted without a reason"
    selector = output[:4]
    if selector == bytes.fromhex("08c379a0"):  # Error(string)
        try:
            return f'reverted: "{abi_decode(["string"], output[4:])[0]}"'
        except Exception:
            return "reverted: <undecodable Error(string)>"
    if selector == bytes.fromhex("4e487b71"):  # Panic(uint256)
        try:
            code = abi_decode(["uint256"], output[4:])[0]
        except Exception:
            return "reverted: <undecodable Panic>"
        return f"panic 0x{code:02x}: {PANIC_CODES.get(code, 'unknown panic')}"
    if abi:
        from eth_utils import function_abi_to_4byte_selector

        for entry in abi:
            if entry.get("type") != "error":
                continue
            try:
                if function_abi_to_4byte_selector(entry) != selector:
                    continue
                types = _abi_types(entry.get("inputs", []))
                values = abi_decode(types, output[4:]) if types else ()
                args = ", ".join(str(v) for v in values)
                return f"reverted: {entry['name']}({args})"
            except Exception:
                continue
    return f"reverted: 0x{output[:64].hex()}"
