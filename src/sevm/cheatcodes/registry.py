"""The cheat registry, and the dispatch that runs one.

Foundry cheatcodes are calls to the magic address `0x7109709E...` (the last 20 bytes of
keccak256("hevm cheat code")). A real EVM does not know it; forge's revm intercepts the
call and interprets it. sevm intercepts every message in its patched opcode loop, so
`apply_cheat` is that interpreter: decode selector + args, mutate live Py-EVM state (or the
session's cheat bookkeeping), hand back the ABI-encoded return.

Handlers register themselves with `@_cheat` at import time, which is why `__init__` imports
the `cheats` and `assertions` modules for their side effects.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from eth_abi import decode as abi_decode
from eth_abi import encode as abi_encode
from eth_utils import function_signature_to_4byte_selector, to_canonical_address

# Address = last 20 bytes of keccak256("hevm cheat code").
VM_ADDRESS = bytes.fromhex("7109709ECfa91a80626fF3989D68f67F5b1DD12D".lower())


class CheatError(Exception):
    """A cheatcode failed; the calling contract should revert with this reason."""


@dataclass
class Prank:
    """An active prank: while set, the next call (or every call, if persistent) whose sender
    is `caller` has its `msg.sender` rewritten to `new_sender`."""

    caller: bytes | None  # the address that invoked the prank (None = any caller)
    new_sender: bytes
    persistent: bool  # startPrank -> True (stays until stopPrank); prank -> False
    # forge's two/three-arg pranks also rewrite tx.origin, and can be told to apply to
    # DELEGATECALL as well. None origin = leave tx.origin alone.
    new_origin: bytes | None = None
    delegate: bool = False


# A private key seeded from a fixed constant, so `vm.random*` is reproducible across runs
# until the caller changes it with `vm.setSeed`. forge uses a per-run seed; a debugger wants
# the same value every time you re-run a stopped transaction.
_DEFAULT_SEED = 0x5EED


@dataclass
class CheatState:
    """Mutable cheat bookkeeping owned by a DebugSession."""

    prank: Prank | None = None
    labels: dict[bytes, str] = field(default_factory=dict)
    console_lines: list[str] = field(default_factory=list)
    seed: int = _DEFAULT_SEED
    _rng: Any = None

    @property
    def rng(self) -> Any:
        """The lazily-built PRNG behind `vm.random*`, reseeded by `vm.setSeed`."""
        import random

        if self._rng is None:
            self._rng = random.Random(self.seed)
        return self._rng

    def reseed(self, seed: int) -> None:
        self.seed = seed
        self._rng = None

    def reset(self) -> None:
        self.prank = None
        self.labels.clear()
        self.console_lines.clear()
        self.reseed(_DEFAULT_SEED)


@dataclass
class CheatContext:
    """Everything a handler needs: the session's cheat state, the live VM state, decoded
    args, and the address that invoked the cheat (for prank targeting)."""

    cheats: CheatState
    state: Any
    args: tuple
    caller: bytes | None


@dataclass
class CheatSpec:
    name: str
    signature: str
    arg_types: list[str]
    ret_types: list[str]
    fn: Callable[[CheatContext], list[Any] | None]
    # One line of help. `help cheatcodes` is generated from these, so the documented set
    # is the implemented set by construction and cannot drift as cheats are added.
    doc: str = ""
    # Overload group, collapsed to one row in `help cheatcodes`: forge-std's assertions
    # alone are 116 signatures.
    family: str = ""


_REGISTRY: dict[bytes, CheatSpec] = {}


def _cheat(
    signature: str,
    ret_types: list[str] | None = None,
    doc: str = "",
    family: str = "",
):
    """Register a handler under `signature`'s 4-byte selector."""
    arg_types = _arg_types(signature)

    def wrap(fn: Callable[[CheatContext], list[Any] | None]) -> Callable:
        selector = function_signature_to_4byte_selector(signature)
        _REGISTRY[selector] = CheatSpec(
            name=signature.split("(", 1)[0],
            signature=signature,
            arg_types=arg_types,
            ret_types=ret_types or [],
            fn=fn,
            doc=doc,
            family=family,
        )
        return fn

    return wrap


def _arg_types(signature: str) -> list[str]:
    inner = signature[signature.index("(") + 1 : signature.rindex(")")]
    return [t.strip() for t in inner.split(",") if t.strip()]


def _addr(value: Any) -> bytes:
    """Normalize an eth_abi-decoded address (a checksummed/lowercased hex str) to bytes."""
    return to_canonical_address(value)


def apply_cheat(
    cheats: CheatState, state: Any, calldata: bytes, caller: bytes | None
) -> bytes:
    """Decode `calldata` (selector + ABI args), run the handler, return ABI-encoded output.

    Raises CheatError for a failed or unimplemented cheatcode; the caller turns that into an
    EVM revert.
    """
    if len(calldata) < 4:
        raise CheatError("cheatcode call with no selector")
    selector, body = calldata[:4], calldata[4:]
    spec = _REGISTRY.get(selector)
    if spec is None:
        raise CheatError(f"unimplemented cheatcode 0x{selector.hex()}")
    args = tuple(abi_decode(spec.arg_types, body)) if spec.arg_types else ()
    result = spec.fn(CheatContext(cheats=cheats, state=state, args=args, caller=caller))
    if spec.ret_types:
        return abi_encode(spec.ret_types, result or [])
    return b""


def all_specs() -> list[CheatSpec]:
    """Every registered cheat spec, alphabetically by signature."""
    return sorted(_REGISTRY.values(), key=lambda spec: spec.signature.lower())


def listing() -> list[CheatSpec]:
    """Cheatcodes for `help cheatcodes`, with each overload family on one row."""
    rows = [spec for spec in _REGISTRY.values() if not spec.family]
    for family in sorted({spec.family for spec in _REGISTRY.values() if spec.family}):
        members = [spec for spec in _REGISTRY.values() if spec.family == family]
        rows.append(
            CheatSpec(
                name=family,
                signature=f"{family}*(...)",
                arg_types=[],
                ret_types=[],
                fn=members[0].fn,
                doc=f"forge-std assertions, {len(members)} overloads (listed below)",
                family=family,
            )
        )
    return sorted(rows, key=lambda spec: spec.name.lower())


def specs_by_name(name: str) -> list[CheatSpec]:
    """Every overload registered under a bare name, e.g. "assertEq"."""
    return [spec for spec in all_specs() if spec.name == name]


def spec_by_name(name: str, argc: int | None = None) -> CheatSpec | None:
    """One overload for a bare name, e.g. "warp"; `argc` picks between overloads."""
    matches = specs_by_name(name)
    if argc is not None:
        matches = [spec for spec in matches if len(spec.arg_types) == argc]
    return matches[0] if matches else None


def cheat_name(calldata: bytes) -> str | None:
    """The cheatcode name for a call payload, or None if the selector is unknown."""
    if len(calldata) < 4:
        return None
    spec = _REGISTRY.get(calldata[:4])
    return spec.name if spec else None


def _fmt(value: Any, decimals: int | None = None) -> str:
    """Render one asserted value the way forge prints it in a failure."""
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(_fmt(v, decimals) for v in value) + "]"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (bytes, bytearray)):
        return "0x" + bytes(value).hex()
    if isinstance(value, int) and decimals:
        sign = "-" if value < 0 else ""
        whole, frac = divmod(abs(value), 10**decimals)
        return f"{sign}{whole}.{frac:0{decimals}d}"
    return str(value)
