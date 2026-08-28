"""Parsing and encoding cheat arguments typed at the prompt.

Arguments are plain literals (an integer, `1 ether`, a 0x address or bytes value,
`true`/`false`, a quoted string), not Solidity expressions. Overloads are picked by ranking
how well each declared type fits the values given.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from eth_abi import decode as abi_decode
from eth_abi import encode as abi_encode
from eth_utils import (
    function_signature_to_4byte_selector,
    to_canonical_address,
)

from .registry import CheatError, CheatSpec, spec_by_name, specs_by_name

_ETHER_UNITS = {
    "wei": 1,
    "gwei": 10**9,
    "szabo": 10**12,
    "finney": 10**15,
    "ether": 10**18,
}


def parse_cheat_arg(text: str) -> Any:
    """Parse one interactive argument: `1 ether`, an address/bytes32 hex, an int, or a
    quoted string. Deliberately small; the value is coerced to the ABI type on encode."""
    text = text.strip()
    if (text.startswith('"') and text.endswith('"')) or (
        text.startswith("'") and text.endswith("'")
    ):
        return text[1:-1]
    lowered = text.lower()
    for unit, mult in _ETHER_UNITS.items():
        if lowered.endswith(unit):
            head = lowered[: -len(unit)].strip()
            if head and head.replace("_", "").isdigit():
                return int(head) * mult
    if text.startswith("0x") or text.startswith("0X"):
        return text  # address / bytesN / hex int, resolved against the ABI type
    if lowered in ("true", "false"):
        return lowered == "true"
    try:
        return int(text)
    except ValueError:
        return text


def _coerce(abi_type: str, value: Any) -> Any:
    if abi_type.startswith(("uint", "int")):
        if isinstance(value, str):
            return int(value, 16 if value.lower().startswith("0x") else 10)
        return int(value)
    if abi_type == "bool":
        return bool(value)
    if abi_type == "address":
        return to_canonical_address(value) if isinstance(value, str) else value
    if abi_type == "bytes32":
        if isinstance(value, int):
            return value.to_bytes(32, "big")
        if isinstance(value, str):
            return bytes.fromhex(value[2:] if value.lower().startswith("0x") else value)
        return bytes(value).rjust(32, b"\x00")
    if abi_type == "bytes":
        if isinstance(value, str):
            return bytes.fromhex(value[2:] if value.lower().startswith("0x") else value)
        return bytes(value)
    return value


def _type_rank(abi_type: str, value: Any) -> int:
    """How well an ABI type suits an untyped prompt literal; lower is better.

    An interactive `vm.assertEq(1, 2)` carries no type, and `1` encodes as `bool` just as
    happily as `uint256`, so overload choice needs a preference, not just a trial encode.
    """
    if isinstance(value, bool):
        order = ["bool", "uint256", "int256", "bytes32", "string"]
    elif isinstance(value, int):
        head = "uint256" if value >= 0 else "int256"
        order = [head, "int256", "bytes32", "bool", "string"]
    elif isinstance(value, str) and value.lower().startswith("0x"):
        order = (
            ["address", "bytes32", "bytes", "uint256", "string"]
            if len(value) == 42
            else ["bytes32", "bytes", "uint256", "address", "string"]
        )
    else:
        order = ["string", "bytes"]
    return order.index(abi_type) if abi_type in order else len(order)


def _select_overload(name: str, values: Sequence[Any]) -> CheatSpec:
    """Pick the overload that matches the literals typed at the prompt."""
    candidates = specs_by_name(name)
    if not candidates:
        raise CheatError(f"unknown or unimplemented cheatcode: vm.{name}")
    fitting = [spec for spec in candidates if len(spec.arg_types) == len(values)]
    if not fitting:
        arities = sorted({len(spec.arg_types) for spec in candidates})
        raise CheatError(
            f"vm.{name} takes {' or '.join(str(a) for a in arities)} argument(s), "
            f"got {len(values)}"
        )
    fitting.sort(
        key=lambda spec: (
            sum(_type_rank(t, v) for t, v in zip(spec.arg_types, values, strict=True)),
            spec.signature,
        )
    )
    for spec in fitting:
        try:
            abi_encode(
                spec.arg_types,
                [_coerce(t, v) for t, v in zip(spec.arg_types, values, strict=True)],
            )
        except Exception:
            continue
        return spec
    raise CheatError(f"vm.{name}: arguments do not fit any overload")


def encode_cheat_call(name: str, values: Sequence[Any]) -> bytes:
    """Build the calldata (selector + ABI args) for an interactive `vm.<name>(...)`."""
    spec = _select_overload(name, values)
    coerced = [_coerce(t, v) for t, v in zip(spec.arg_types, values, strict=True)]
    return function_signature_to_4byte_selector(spec.signature) + (
        abi_encode(spec.arg_types, coerced) if spec.arg_types else b""
    )


def format_cheat_result(name: str, output: bytes) -> str:
    """Render a cheat's return value (load/addr/sign) for the prompt."""
    spec = spec_by_name(name)
    if spec is None or not spec.ret_types:
        return "ok"
    values = abi_decode(spec.ret_types, output)
    rendered = [v.hex() if isinstance(v, (bytes, bytearray)) else str(v) for v in values]
    return ", ".join(rendered)
