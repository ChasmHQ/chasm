"""Pure conversion and string cheats: `toString`, `parse*`, base64, case/trim/split, and the
CREATE / CREATE2 address computations. None of these touch VM state; they are deterministic
transforms of their arguments, so they behave identically to forge.
"""

from __future__ import annotations

import base64
from typing import Any

import rlp
from eth_utils import keccak, to_canonical_address, to_checksum_address

from .registry import CheatContext, CheatError, _cheat

# forge's default CREATE2 deployer (the deterministic-deployment-proxy address).
_CREATE2_DEPLOYER = to_canonical_address("0x4e59b44847b379578588920cA78FbF26c0B4956C")


# ---- toString ----------------------------------------------------------------------------


@_cheat(
    "toString(address)",
    ret_types=["string"],
    family="toString",
    doc="value to its string form",
)
def _ts_address(ctx: CheatContext) -> list[Any]:
    return [to_checksum_address(to_canonical_address(ctx.args[0]))]


@_cheat(
    "toString(uint256)",
    ret_types=["string"],
    family="toString",
    doc="value to its string form",
)
def _ts_uint(ctx: CheatContext) -> list[Any]:
    return [str(int(ctx.args[0]))]


@_cheat(
    "toString(int256)",
    ret_types=["string"],
    family="toString",
    doc="value to its string form",
)
def _ts_int(ctx: CheatContext) -> list[Any]:
    return [str(int(ctx.args[0]))]


@_cheat(
    "toString(bytes32)",
    ret_types=["string"],
    family="toString",
    doc="value to its string form",
)
def _ts_bytes32(ctx: CheatContext) -> list[Any]:
    return ["0x" + bytes(ctx.args[0]).hex()]


@_cheat(
    "toString(bytes)",
    ret_types=["string"],
    family="toString",
    doc="value to its string form",
)
def _ts_bytes(ctx: CheatContext) -> list[Any]:
    return ["0x" + bytes(ctx.args[0]).hex()]


@_cheat(
    "toString(bool)",
    ret_types=["string"],
    family="toString",
    doc="value to its string form",
)
def _ts_bool(ctx: CheatContext) -> list[Any]:
    return ["true" if ctx.args[0] else "false"]


# ---- parse -------------------------------------------------------------------------------


@_cheat(
    "parseUint(string)",
    ret_types=["uint256"],
    family="parse",
    doc="parse a string as that type",
)
def _p_uint(ctx: CheatContext) -> list[Any]:
    return [int(str(ctx.args[0]).strip(), 0)]


@_cheat(
    "parseInt(string)",
    ret_types=["int256"],
    family="parse",
    doc="parse a string as that type",
)
def _p_int(ctx: CheatContext) -> list[Any]:
    return [int(str(ctx.args[0]).strip(), 0)]


@_cheat(
    "parseBool(string)",
    ret_types=["bool"],
    family="parse",
    doc="parse a string as that type",
)
def _p_bool(ctx: CheatContext) -> list[Any]:
    low = str(ctx.args[0]).strip().lower()
    if low not in ("true", "false"):
        raise CheatError(f"parseBool: {ctx.args[0]!r} is not a bool")
    return [low == "true"]


@_cheat(
    "parseAddress(string)",
    ret_types=["address"],
    family="parse",
    doc="parse a string as that type",
)
def _p_address(ctx: CheatContext) -> list[Any]:
    return [to_checksum_address(to_canonical_address(str(ctx.args[0]).strip()))]


@_cheat(
    "parseBytes(string)",
    ret_types=["bytes"],
    family="parse",
    doc="parse a string as that type",
)
def _p_bytes(ctx: CheatContext) -> list[Any]:
    s = str(ctx.args[0]).strip()
    return [bytes.fromhex(s[2:] if s.lower().startswith("0x") else s)]


@_cheat(
    "parseBytes32(string)",
    ret_types=["bytes32"],
    family="parse",
    doc="parse a string as that type",
)
def _p_bytes32(ctx: CheatContext) -> list[Any]:
    s = str(ctx.args[0]).strip()
    b = bytes.fromhex(s[2:] if s.lower().startswith("0x") else s)
    if len(b) > 32:
        raise CheatError("parseBytes32: value does not fit in 32 bytes")
    return [b.rjust(32, b"\x00")]


# ---- base64 ------------------------------------------------------------------------------


def _as_bytes(value: Any) -> bytes:
    return value.encode() if isinstance(value, str) else bytes(value)


@_cheat(
    "toBase64(bytes)",
    ret_types=["string"],
    family="toBase64",
    doc="standard base64 encode",
)
def _b64_bytes(ctx: CheatContext) -> list[Any]:
    return [base64.b64encode(_as_bytes(ctx.args[0])).decode()]


@_cheat(
    "toBase64(string)",
    ret_types=["string"],
    family="toBase64",
    doc="standard base64 encode",
)
def _b64_string(ctx: CheatContext) -> list[Any]:
    return [base64.b64encode(_as_bytes(ctx.args[0])).decode()]


@_cheat(
    "toBase64URL(bytes)",
    ret_types=["string"],
    family="toBase64",
    doc="url-safe base64 encode",
)
def _b64url_bytes(ctx: CheatContext) -> list[Any]:
    return [base64.urlsafe_b64encode(_as_bytes(ctx.args[0])).decode().rstrip("=")]


@_cheat(
    "toBase64URL(string)",
    ret_types=["string"],
    family="toBase64",
    doc="url-safe base64 encode",
)
def _b64url_string(ctx: CheatContext) -> list[Any]:
    return [base64.urlsafe_b64encode(_as_bytes(ctx.args[0])).decode().rstrip("=")]


# ---- string utilities --------------------------------------------------------------------


@_cheat(
    "toLowercase(string)",
    ret_types=["string"],
    family="strops",
    doc="string manipulation",
)
def _lower(ctx: CheatContext) -> list[Any]:
    return [str(ctx.args[0]).lower()]


@_cheat(
    "toUppercase(string)",
    ret_types=["string"],
    family="strops",
    doc="string manipulation",
)
def _upper(ctx: CheatContext) -> list[Any]:
    return [str(ctx.args[0]).upper()]


@_cheat("trim(string)", ret_types=["string"], family="strops", doc="string manipulation")
def _trim(ctx: CheatContext) -> list[Any]:
    return [str(ctx.args[0]).strip()]


@_cheat(
    "replace(string,string,string)",
    ret_types=["string"],
    family="strops",
    doc="string manipulation",
)
def _replace(ctx: CheatContext) -> list[Any]:
    return [str(ctx.args[0]).replace(str(ctx.args[1]), str(ctx.args[2]))]


@_cheat(
    "contains(string,string)",
    ret_types=["bool"],
    family="strops",
    doc="string manipulation",
)
def _contains(ctx: CheatContext) -> list[Any]:
    return [str(ctx.args[1]) in str(ctx.args[0])]


@_cheat(
    "indexOf(string,string)",
    ret_types=["uint256"],
    family="strops",
    doc="string manipulation",
)
def _index_of(ctx: CheatContext) -> list[Any]:
    idx = str(ctx.args[0]).find(str(ctx.args[1]))
    # forge returns type(uint256).max when the key is absent.
    return [idx if idx >= 0 else (1 << 256) - 1]


@_cheat(
    "split(string,string)",
    ret_types=["string[]"],
    family="strops",
    doc="string manipulation",
)
def _split(ctx: CheatContext) -> list[Any]:
    return [str(ctx.args[0]).split(str(ctx.args[1]))]


# ---- CREATE / CREATE2 address computation ------------------------------------------------


@_cheat(
    "computeCreateAddress(address,uint256)",
    ret_types=["address"],
    doc="the address a CREATE from (deployer, nonce) lands at",
)
def _create_addr(ctx: CheatContext) -> list[Any]:
    deployer = to_canonical_address(ctx.args[0])
    nonce = int(ctx.args[1])
    raw = keccak(rlp.encode([deployer, nonce]))[12:]
    return [to_checksum_address(raw)]


def _create2(salt: bytes, init_code_hash: bytes, deployer: bytes) -> str:
    raw = keccak(b"\xff" + deployer + salt + init_code_hash)[12:]
    return to_checksum_address(raw)


@_cheat(
    "computeCreate2Address(bytes32,bytes32,address)",
    ret_types=["address"],
    doc="the address a CREATE2 (salt, initCodeHash, deployer) lands at",
)
def _create2_addr(ctx: CheatContext) -> list[Any]:
    return [
        _create2(
            bytes(ctx.args[0]), bytes(ctx.args[1]), to_canonical_address(ctx.args[2])
        )
    ]


@_cheat(
    "computeCreate2Address(bytes32,bytes32)",
    ret_types=["address"],
    doc="CREATE2 address using the default deterministic deployer",
)
def _create2_addr_default(ctx: CheatContext) -> list[Any]:
    return [_create2(bytes(ctx.args[0]), bytes(ctx.args[1]), _CREATE2_DEPLOYER)]
