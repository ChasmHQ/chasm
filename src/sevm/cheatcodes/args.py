"""Parsing and encoding cheat arguments typed at the prompt.

An argument is either a plain literal (an integer, `1 ether`, a 0x address or bytes value,
`true`/`false`, a quoted string) or, when it is not one of those, a Solidity expression the
caller has already evaluated against the paused frame. An evaluated argument arrives as a
`CheatArg` carrying solc's own ABI type, which then picks the overload directly instead of
going through the guesswork the untyped literals need.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
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

# Sentinel: the text is not a prompt literal, so the caller may try Solidity on it.
_NOT_LITERAL = object()

# Rank for a declared type that does not match solc's type for the argument at all.
_MISMATCH = 50


@dataclass(frozen=True)
class CheatArg:
    """One argument on its way into a cheat call.

    `abi_type` is set only when solc typed the argument (an evaluated Solidity expression);
    a literal leaves it None and is ranked against the declared types instead. `text` and
    `note` exist for the error message, which is otherwise reduced to "does not fit".
    """

    value: Any
    abi_type: str | None = None
    text: str = ""
    note: str = ""

    @property
    def shown(self) -> str:
        return self.text or str(self.value)


def _parse_literal(text: str) -> Any:
    """One prompt literal, or `_NOT_LITERAL` if the text is not one."""
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
        return _NOT_LITERAL


def parse_cheat_arg(text: str) -> Any:
    """Parse one interactive argument: `1 ether`, an address/bytes hex, an int, or a quoted
    string. Text that is none of those comes back unchanged, which is how a bare word still
    reaches a `string` parameter."""
    parsed = _parse_literal(text)
    return text.strip() if parsed is _NOT_LITERAL else parsed


def is_cheat_literal(text: str) -> bool:
    """Whether the text is a prompt literal. If it is not, it may be Solidity."""
    return _parse_literal(text) is not _NOT_LITERAL


def _as_address(text: str) -> str:
    """Zero-pad a short hex literal to 20 bytes, as Solidity's `address(0xcafe)` does.

    Only short values are padded. Anything longer goes to eth_utils untouched and is
    rejected there, since padding cannot fix it and truncating would silently drop bytes
    the user typed.
    """
    if text.lower().startswith("0x") and len(text) < 42:
        return "0x" + text[2:].rjust(40, "0")
    return text


def _coerce(abi_type: str, value: Any) -> Any:
    if abi_type.startswith(("uint", "int")):
        if isinstance(value, str):
            return int(value, 16 if value.lower().startswith("0x") else 10)
        return int(value)
    if abi_type == "bool":
        # Not `bool(value)`: that takes any non-empty string, so a mistyped name would
        # quietly become `true` and land in a bool overload instead of failing.
        if isinstance(value, bool):
            return value
        if isinstance(value, int) and value in (0, 1):
            return bool(value)
        raise ValueError(f"{value!r} is not a bool")
    if abi_type == "address":
        return (
            to_canonical_address(_as_address(value)) if isinstance(value, str) else value
        )
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
    if abi_type == "string":
        if not isinstance(value, str):
            raise ValueError(f"{value!r} is not a string")
        return value
    return value


def _abi_family(abi_type: str) -> str:
    """Types that differ only in width, so `uint8` still fits a `uint256` parameter."""
    if abi_type.endswith("[]"):
        return _abi_family(abi_type[:-2]) + "[]"
    if abi_type.startswith("uint"):
        return "uint"
    if abi_type.startswith("int"):
        return "int"
    if abi_type == "bytes":
        return "bytes"
    if abi_type.startswith("bytes"):
        return "bytes32"
    return abi_type


def _type_rank(abi_type: str, arg: CheatArg) -> int:
    """How well a declared parameter type suits one argument; lower is better.

    An untyped `vm.assertEq(1, 2)` carries no type, and `1` encodes as `bool` just as
    happily as `uint256`, so overload choice needs a preference, not just a trial encode.
    An argument solc has typed needs no such guess.
    """
    if arg.abi_type is not None:
        if abi_type == arg.abi_type:
            return 0
        return 1 if _abi_family(abi_type) == _abi_family(arg.abi_type) else _MISMATCH
    value = arg.value
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


@dataclass
class _Rejection:
    """Why one overload turned an argument down, kept for the error message."""

    spec: CheatSpec
    index: int
    arg: CheatArg
    abi_type: str


def _coerce_all(spec: CheatSpec, args: Sequence[CheatArg]) -> list[Any]:
    """Coerce every argument to its declared type, or say which one refused."""
    coerced = []
    for index, (abi_type, arg) in enumerate(zip(spec.arg_types, args, strict=True)):
        try:
            coerced.append(_coerce(abi_type, arg.value))
        except Exception as exc:
            raise _Rejected(_Rejection(spec, index, arg, abi_type)) from exc
    return coerced


class _Rejected(Exception):
    def __init__(self, rejection: _Rejection) -> None:
        super().__init__(rejection)
        self.rejection = rejection


def _overload_error(name: str, rejections: list[_Rejection]) -> CheatError:
    """Name the argument that did not fit, not just the fact that none did.

    `rejections` is in ranked order, so the first is the overload that came closest.
    """
    best = rejections[0]
    message = (
        f"vm.{name}: argument {best.index + 1} ({best.arg.shown}) "
        f"is not a valid {best.abi_type}"
    )
    if best.arg.note:
        message += f" ({best.arg.note})"
    if len(rejections) > 1:
        message += f"; closest of {len(rejections)} overloads is vm.{best.spec.signature}"
    return CheatError(message)


def _as_arg(item: Any) -> CheatArg:
    return item if isinstance(item, CheatArg) else CheatArg(value=item)


def _select_overload(name: str, args: Sequence[CheatArg]) -> tuple[CheatSpec, list[Any]]:
    """Pick the overload the arguments fit, and return it with them coerced."""
    candidates = specs_by_name(name)
    if not candidates:
        raise CheatError(f"unknown or unimplemented cheatcode: vm.{name}")
    fitting = [spec for spec in candidates if len(spec.arg_types) == len(args)]
    if not fitting:
        arities = sorted({len(spec.arg_types) for spec in candidates})
        raise CheatError(
            f"vm.{name} takes {' or '.join(str(a) for a in arities)} argument(s), "
            f"got {len(args)}"
        )
    fitting.sort(
        key=lambda spec: (
            sum(_type_rank(t, a) for t, a in zip(spec.arg_types, args, strict=True)),
            spec.signature,
        )
    )
    rejections: list[_Rejection] = []
    for spec in fitting:
        try:
            coerced = _coerce_all(spec, args)
            abi_encode(spec.arg_types, coerced)
        except _Rejected as exc:
            rejections.append(exc.rejection)
            continue
        except Exception:
            # The values coerced but do not encode (out of range, wrong arity inside an
            # array); blame the first argument whose declared type is not a plain match.
            rejections.append(_Rejection(spec, 0, args[0], spec.arg_types[0]))
            continue
        return spec, coerced
    raise _overload_error(name, rejections)


def encode_cheat_call(name: str, values: Sequence[Any]) -> bytes:
    """Build the calldata (selector + ABI args) for an interactive `vm.<name>(...)`."""
    args = [_as_arg(value) for value in values]
    spec, coerced = _select_overload(name, args)
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
