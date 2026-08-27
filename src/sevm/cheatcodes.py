"""Foundry cheatcode engine.

Foundry cheatcodes are calls to the magic address
`0x7109709ECfa91a80626fF3989D68f67F5b1DD12D` (the last 20 bytes of
keccak256("hevm cheat code")). A real EVM does not know this address; forge's revm
intercepts the call and interprets it. sevm already intercepts every message in its patched
opcode loop, so this module supplies the interpreter: decode the selector + args, mutate the
live Py-EVM state (or the session's cheat bookkeeping), and hand back the ABI-encoded return.

`console.log` works the same way against `0x000000000000000000636F6e736F6c652e6c6f67`.

Scope (v1): environment + identity cheats only. `warp`, `roll`, `fee`, `chainId`,
`coinbase`, `deal`, `etch`, `store`, `load`, `prank`/`startPrank`/`stopPrank`, `addr`,
`sign`, `assume`, `label`. Everything else declared in the bundled `Vm.sol` reverts with a
clear "unimplemented" message rather than silently doing nothing.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from eth_abi import decode as abi_decode
from eth_abi import encode as abi_encode
from eth_keys import keys
from eth_utils import function_signature_to_4byte_selector, to_canonical_address

# Address = last 20 bytes of keccak256("hevm cheat code").
VM_ADDRESS = bytes.fromhex("7109709ECfa91a80626fF3989D68f67F5b1DD12D".lower())
# Address = ASCII "console.log" right-aligned in 20 bytes.
CONSOLE_ADDRESS = bytes.fromhex("000000000000000000636F6e736F6c652e6c6f67".lower())


class CheatError(Exception):
    """A cheatcode failed; the calling contract should revert with this reason."""


@dataclass
class Prank:
    """An active prank: while set, the next call (or every call, if persistent) whose sender
    is `caller` has its `msg.sender` rewritten to `new_sender`."""

    caller: bytes | None  # the address that invoked the prank (None = any caller)
    new_sender: bytes
    persistent: bool  # startPrank -> True (stays until stopPrank); prank -> False


@dataclass
class CheatState:
    """Mutable cheat bookkeeping owned by a DebugSession."""

    prank: Prank | None = None
    labels: dict[bytes, str] = field(default_factory=dict)
    console_lines: list[str] = field(default_factory=list)

    def reset(self) -> None:
        self.prank = None
        self.labels.clear()
        self.console_lines.clear()


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


_REGISTRY: dict[bytes, CheatSpec] = {}


def _cheat(signature: str, ret_types: list[str] | None = None, doc: str = ""):
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
        )
        return fn

    return wrap


def _arg_types(signature: str) -> list[str]:
    inner = signature[signature.index("(") + 1 : signature.rindex(")")]
    return [t.strip() for t in inner.split(",") if t.strip()]


def _addr(value: Any) -> bytes:
    """Normalize an eth_abi-decoded address (a checksummed/lowercased hex str) to bytes."""
    return to_canonical_address(value)


# ---- environment ----------------------------------------------------------


@_cheat("warp(uint256)", doc="set block.timestamp")
def _warp(ctx: CheatContext) -> None:
    ctx.state.execution_context._timestamp = int(ctx.args[0])


@_cheat("roll(uint256)", doc="set block.number")
def _roll(ctx: CheatContext) -> None:
    ctx.state.execution_context._block_number = int(ctx.args[0])


@_cheat("fee(uint256)", doc="set block.basefee")
def _fee(ctx: CheatContext) -> None:
    ctx.state.execution_context._base_fee_per_gas = int(ctx.args[0])


@_cheat("chainId(uint256)", doc="set the chain id")
def _chain_id(ctx: CheatContext) -> None:
    ctx.state.execution_context._chain_id = int(ctx.args[0])


@_cheat("coinbase(address)", doc="set block.coinbase")
def _coinbase(ctx: CheatContext) -> None:
    ctx.state.execution_context._coinbase = _addr(ctx.args[0])


# ---- account state --------------------------------------------------------


@_cheat("deal(address,uint256)", doc="set an account's balance")
def _deal(ctx: CheatContext) -> None:
    ctx.state.set_balance(_addr(ctx.args[0]), int(ctx.args[1]))


@_cheat("etch(address,bytes)", doc="replace an account's code")
def _etch(ctx: CheatContext) -> None:
    ctx.state.set_code(_addr(ctx.args[0]), bytes(ctx.args[1]))


@_cheat("store(address,bytes32,bytes32)", doc="write a raw storage slot of any account")
def _store(ctx: CheatContext) -> None:
    slot = int.from_bytes(ctx.args[1], "big")
    value = int.from_bytes(ctx.args[2], "big")
    ctx.state.set_storage(_addr(ctx.args[0]), slot, value)


@_cheat(
    "load(address,bytes32)",
    ret_types=["bytes32"],
    doc="read a raw storage slot of any account",
)
def _load(ctx: CheatContext) -> list[Any]:
    slot = int.from_bytes(ctx.args[1], "big")
    value = ctx.state.get_storage(_addr(ctx.args[0]), slot)
    return [int(value).to_bytes(32, "big")]


# ---- identity / prank -----------------------------------------------------


@_cheat("prank(address)", doc="rewrite msg.sender for the next call only")
def _prank(ctx: CheatContext) -> None:
    ctx.cheats.prank = Prank(
        caller=ctx.caller, new_sender=_addr(ctx.args[0]), persistent=False
    )


@_cheat("startPrank(address)", doc="rewrite msg.sender until stopPrank")
def _start_prank(ctx: CheatContext) -> None:
    ctx.cheats.prank = Prank(
        caller=ctx.caller, new_sender=_addr(ctx.args[0]), persistent=True
    )


@_cheat("stopPrank()", doc="end an active startPrank")
def _stop_prank(ctx: CheatContext) -> None:
    ctx.cheats.prank = None


# ---- keys / signing -------------------------------------------------------


@_cheat("addr(uint256)", ret_types=["address"], doc="the address of a private key")
def _addr_of(ctx: CheatContext) -> list[Any]:
    pk = keys.PrivateKey(int(ctx.args[0]).to_bytes(32, "big"))
    return [pk.public_key.to_checksum_address()]


@_cheat(
    "sign(uint256,bytes32)",
    ret_types=["uint8", "bytes32", "bytes32"],
    doc="sign a hash with a private key, returning (v, r, s)",
)
def _sign(ctx: CheatContext) -> list[Any]:
    pk = keys.PrivateKey(int(ctx.args[0]).to_bytes(32, "big"))
    sig = pk.sign_msg_hash(bytes(ctx.args[1]))
    return [sig.v + 27, sig.r.to_bytes(32, "big"), sig.s.to_bytes(32, "big")]


# ---- fuzzing / labelling --------------------------------------------------


@_cheat("assume(bool)", doc="reject a fuzz input that fails the condition")
def _assume(ctx: CheatContext) -> None:
    if not ctx.args[0]:
        raise CheatError("vm.assume rejected the input")


@_cheat("label(address,string)", doc="give an address a readable name")
def _label(ctx: CheatContext) -> None:
    ctx.cheats.labels[_addr(ctx.args[0])] = ctx.args[1]


# ===========================================================================
# entry points
# ===========================================================================


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


_ETHER_UNITS = {
    "wei": 1,
    "gwei": 10**9,
    "szabo": 10**12,
    "finney": 10**15,
    "ether": 10**18,
}


def listing() -> list[CheatSpec]:
    """Every implemented cheatcode, alphabetically, for `help cheatcodes`."""
    return sorted(_REGISTRY.values(), key=lambda spec: spec.name.lower())


def spec_by_name(name: str) -> CheatSpec | None:
    """The (v1-unique) cheat spec for a bare name, e.g. "warp"."""
    for spec in _REGISTRY.values():
        if spec.name == name:
            return spec
    return None


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


def encode_cheat_call(name: str, values: Sequence[Any]) -> bytes:
    """Build the calldata (selector + ABI args) for an interactive `vm.<name>(...)`."""
    spec = spec_by_name(name)
    if spec is None:
        raise CheatError(f"unknown or unimplemented cheatcode: vm.{name}")
    if len(values) != len(spec.arg_types):
        raise CheatError(
            f"vm.{name} takes {len(spec.arg_types)} argument(s), got {len(values)}"
        )
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


def cheat_name(calldata: bytes) -> str | None:
    """The cheatcode name for a call payload, or None if the selector is unknown."""
    if len(calldata) < 4:
        return None
    spec = _REGISTRY.get(calldata[:4])
    return spec.name if spec else None


# ---- console.log ----------------------------------------------------------

# console.log forwards `log(<types>)` calls. We decode the leading selector's declared
# argument types from the signature embedded by solc; forge-std uses a fixed table, so we
# resolve the common overloads by selector.
_CONSOLE_SIGS = [
    "log(string)",
    "log(uint256)",
    "log(int256)",
    "log(bool)",
    "log(address)",
    "log(bytes)",
    "log(string,uint256)",
    "log(string,int256)",
    "log(string,address)",
    "log(string,bool)",
    "log(string,string)",
    "log(address,uint256)",
]
_CONSOLE_TABLE: dict[bytes, list[str]] = {
    function_signature_to_4byte_selector(sig): _arg_types(sig) for sig in _CONSOLE_SIGS
}


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
    parts = [str(v) for v in values]
    return " ".join(parts)
