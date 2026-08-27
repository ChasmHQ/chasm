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


# ---- assertions -----------------------------------------------------------
#
# forge-std's `assertEq` and friends are thin wrappers: the plain forms call the cheatcode
# only once the comparison has already failed, but the `*Decimal` and `*ApproxEq*` forms
# call it unconditionally and expect the VM to do the comparison. So these implement the
# comparison for real, and revert with forge's message shape when it does not hold.

ASSERT_FAMILY = "assert"

# The types forge-std asserts over, each also in its array form.
_ASSERT_TYPES = ("bool", "uint256", "int256", "address", "bytes32", "string", "bytes")

_ORDER_OPS: dict[str, tuple[Callable[[Any, Any], bool], str]] = {
    "assertGt": (lambda a, b: a > b, "<="),
    "assertGe": (lambda a, b: a >= b, "<"),
    "assertLt": (lambda a, b: a < b, ">="),
    "assertLe": (lambda a, b: a <= b, ">"),
}


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


def _fail(message: str, err: str | None) -> None:
    """forge prefixes a custom message and drops its own; otherwise it says `assertion failed`."""
    raise CheatError(f"{err}: {message}" if err else f"assertion failed: {message}")


def _tail_err(args: tuple, index: int) -> str | None:
    return str(args[index]) if len(args) > index else None


def _equality(op: str, decimal: bool):
    def handler(ctx: CheatContext) -> None:
        left, right = ctx.args[0], ctx.args[1]
        decimals = int(ctx.args[2]) if decimal else None
        err = _tail_err(ctx.args, 3 if decimal else 2)
        equal = left == right
        if op == "assertEq" and not equal:
            _fail(f"{_fmt(left, decimals)} != {_fmt(right, decimals)}", err)
        if op == "assertNotEq" and equal:
            _fail(f"{_fmt(left, decimals)} == {_fmt(right, decimals)}", err)

    return handler


def _ordering(op: str, decimal: bool):
    compare, violated = _ORDER_OPS[op]

    def handler(ctx: CheatContext) -> None:
        left, right = int(ctx.args[0]), int(ctx.args[1])
        decimals = int(ctx.args[2]) if decimal else None
        err = _tail_err(ctx.args, 3 if decimal else 2)
        if not compare(left, right):
            _fail(
                f"{_fmt(left, decimals)} {violated} {_fmt(right, decimals)}",
                err,
            )

    return handler


def _approx(relative: bool, decimal: bool):
    def handler(ctx: CheatContext) -> None:
        left, right = int(ctx.args[0]), int(ctx.args[1])
        limit = int(ctx.args[2])
        decimals = int(ctx.args[3]) if decimal else None
        err = _tail_err(ctx.args, 4 if decimal else 3)
        delta = abs(left - right)
        if relative:
            # forge measures the relative delta against the right-hand (expected) value,
            # in 18-decimal fixed point; a zero expectation only passes on an exact match.
            if right == 0:
                actual = 0 if delta == 0 else None
            else:
                actual = delta * 10**18 // abs(right)
            if actual is None or actual > limit:
                shown = "undefined" if actual is None else _fmt(actual, 16) + "%"
                _fail(
                    f"{_fmt(left, decimals)} !~= {_fmt(right, decimals)} "
                    f"(max delta: {_fmt(limit, 16)}%, real delta: {shown})",
                    err,
                )
        elif delta > limit:
            _fail(
                f"{_fmt(left, decimals)} !~= {_fmt(right, decimals)} "
                f"(max delta: {_fmt(limit, decimals)}, real delta: {_fmt(delta, decimals)})",
                err,
            )

    return handler


def _boolean(expected: bool):
    def handler(ctx: CheatContext) -> None:
        err = _tail_err(ctx.args, 1)
        if bool(ctx.args[0]) is not expected:
            raise CheatError(err or "assertion failed")

    return handler


def _register(signature: str, handler: Callable, doc: str) -> None:
    _cheat(signature, doc=doc, family=ASSERT_FAMILY)(handler)


def _register_assertions() -> None:
    """Build forge-std's assertion surface (116 overloads) from the op x type matrix."""
    for expected, name in ((True, "assertTrue"), (False, "assertFalse")):
        for sig in (f"{name}(bool)", f"{name}(bool,string)"):
            _register(sig, _boolean(expected), f"revert unless the value is {expected}")

    for op in ("assertEq", "assertNotEq"):
        relation = "equal" if op == "assertEq" else "different"
        for base in _ASSERT_TYPES:
            for kind in (base, f"{base}[]"):
                for sig in (f"{op}({kind},{kind})", f"{op}({kind},{kind},string)"):
                    _register(sig, _equality(op, False), f"revert unless {relation}")
        for kind in ("uint256", "int256"):
            for sig in (
                f"{op}Decimal({kind},{kind},uint256)",
                f"{op}Decimal({kind},{kind},uint256,string)",
            ):
                _register(
                    sig,
                    _equality(op, True),
                    f"revert unless {relation}, printing fixed-point values",
                )

    for op in _ORDER_OPS:
        for kind in ("uint256", "int256"):
            for sig in (f"{op}({kind},{kind})", f"{op}({kind},{kind},string)"):
                _register(sig, _ordering(op, False), "revert unless the order holds")
            for sig in (
                f"{op}Decimal({kind},{kind},uint256)",
                f"{op}Decimal({kind},{kind},uint256,string)",
            ):
                _register(
                    sig,
                    _ordering(op, True),
                    "revert unless the order holds, printing fixed-point values",
                )

    for name, relative in (("assertApproxEqAbs", False), ("assertApproxEqRel", True)):
        limit = "max delta" if not relative else "max relative delta (1e18 = 100%)"
        for kind in ("uint256", "int256"):
            for sig in (
                f"{name}({kind},{kind},uint256)",
                f"{name}({kind},{kind},uint256,string)",
            ):
                _register(sig, _approx(relative, False), f"revert past the {limit}")
            for sig in (
                f"{name}Decimal({kind},{kind},uint256,uint256)",
                f"{name}Decimal({kind},{kind},uint256,uint256,string)",
            ):
                _register(
                    sig,
                    _approx(relative, True),
                    f"revert past the {limit}, printing fixed-point values",
                )


_register_assertions()


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


def cheat_name(calldata: bytes) -> str | None:
    """The cheatcode name for a call payload, or None if the selector is unknown."""
    if len(calldata) < 4:
        return None
    spec = _REGISTRY.get(calldata[:4])
    return spec.name if spec else None


# ---- console.log ----------------------------------------------------------

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
