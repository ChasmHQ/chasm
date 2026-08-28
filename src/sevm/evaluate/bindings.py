"""Getting a frame's locals (and `msg.data`/`msg.sig`) into the injected function.

Locals ride in as *parameters*, with their values in the call's calldata, rather than as
literals: compiled code then depends only on names and types, so `display amount - fee`
costs one compile per session instead of one per step. Solidity's own scoping shadows a
state variable of the same name, exactly as a real local does.

`msg.data`/`msg.sig` ride in the same way. The injected function is reached by a real call,
so read directly they would report *that* call's calldata, silently and plausibly wrong.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

from eth_abi import encode as abi_encode
from eth_utils import function_signature_to_4byte_selector

from ..locals import referenced_names

EVAL_FUNCTION = "__sevm_eval"
EVAL_SELECTOR = function_signature_to_4byte_selector(f"{EVAL_FUNCTION}()")

# msg field -> (parameter name, declared type, abi type). `bytes calldata` rather than
# `bytes memory` so `msg.data[4:]` still slices, which is how anyone reads arguments out
# of raw calldata.
MSG_FIELDS: dict[str, tuple[str, str, str]] = {
    "data": ("__sevm_msg_data", "bytes calldata", "bytes"),
    "sig": ("__sevm_msg_sig", "bytes4", "bytes4"),
}

_MSG_FIELD = re.compile(r"\bmsg\s*\.\s*(data|sig)\b")
# One capture group, so `split` alternates code, literal, code, ... and the literals can
# be put back untouched.
_STRING_LITERAL = re.compile(r"(\"(?:[^\"\\]|\\.)*\"|'(?:[^'\\]|\\.)*')")


@dataclass(frozen=True)
class Binding:
    """One local variable, ready to be passed into the injected eval function."""

    name: str
    declared_type: str  # as written in the parameter list, e.g. "string memory"
    abi_type: str  # as abi-encoded, e.g. "string"
    value: Any

    @property
    def parameter(self) -> str:
        return f"{self.declared_type} {self.name}"


def _declared_parameter_type(abi_type: str) -> str:
    """A parameter list needs a data location on every reference type."""
    if (
        abi_type in ("string", "bytes")
        or abi_type.endswith("]")
        or abi_type.startswith("(")
    ):
        return f"{abi_type} memory"
    return abi_type


def bindings_for(expression: str, locals_available: Sequence[Any]) -> list[Binding]:
    """Pick the locals an expression actually mentions.

    Only referenced locals get injected, so a frame full of unmaterialisable locals
    doesn't stop `p totalDeposits` from working, and unreferenced names don't affect
    the compiled code or cache key.
    """
    wanted = referenced_names(expression)
    out: list[Binding] = []
    seen = set()
    for local in locals_available:
        if local.name not in wanted or local.name in seen:
            continue
        if not getattr(local, "bindable", False):
            continue
        seen.add(local.name)
        out.append(
            Binding(
                name=local.name,
                declared_type=_declared_parameter_type(local.abi_type),
                abi_type=local.abi_type,
                value=local.abi_value,
            )
        )
    return out


def rewrite_msg(expression: str) -> tuple[str, list[str]]:
    """Point `msg.data`/`msg.sig` at the paused frame instead of the injected call.

    Returns the expression to compile and the fields it used, in the order they appear
    (which is the order `msg_bindings` must encode them in). String literals are left
    alone, so `bytes("msg.data")` still says what it says.
    """
    used: list[str] = []

    def replace(match: re.Match[str]) -> str:
        field = match.group(1)
        if field not in used:
            used.append(field)
        return MSG_FIELDS[field][0]

    parts = _STRING_LITERAL.split(expression)
    rewritten = [
        part if index % 2 else _MSG_FIELD.sub(replace, part)
        for index, part in enumerate(parts)
    ]
    return "".join(rewritten), used


def msg_bindings(frame: Any, fields: Sequence[str]) -> list[Binding]:
    """The frame's real calldata, ready to pass into the injected function."""
    calldata = bytes(getattr(frame, "calldata", b"") or b"")
    # Short calldata (a create frame has none) gives msg.sig 0x00000000, as in Solidity.
    values = {"data": calldata, "sig": calldata[:4].ljust(4, b"\x00")}
    out = []
    for field in fields:
        name, declared_type, abi_type = MSG_FIELDS[field]
        out.append(
            Binding(
                name=name,
                declared_type=declared_type,
                abi_type=abi_type,
                value=values[field],
            )
        )
    return out


def unbindable_reference(expression: str, locals_available: Sequence[Any]) -> str | None:
    """The error for an expression that names a local we cannot pass in.

    Without this the name would quietly resolve to a same-named state variable or to
    nothing, reporting "undeclared identifier" for a name that's right there on screen.
    """
    wanted = referenced_names(expression)
    for local in locals_available:
        if local.name in wanted and not getattr(local, "bindable", False):
            detail = local.reason or "not readable here"
            return f"local `{local.name}` cannot be used in an expression: {detail}"
    return None


def _call_data(bindings: Sequence[Binding]) -> bytes:
    """Selector plus arguments for the injected function, as a real ABI call."""
    types = [b.abi_type for b in bindings]
    signature = f"{EVAL_FUNCTION}({','.join(types)})"
    selector = function_signature_to_4byte_selector(signature)
    if not bindings:
        return selector
    return selector + abi_encode(types, [b.value for b in bindings])


def _parameter_list(bindings: Sequence[Binding]) -> str:
    return ", ".join(b.parameter for b in bindings)


# Type probe: nothing implicitly converts to a locally-declared struct, so solc's error
# reports the expression's actual type. Probing with `uint256` would silently widen a
