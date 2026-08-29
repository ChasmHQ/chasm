"""The `vm.env*` family: read process environment variables into Solidity values.

These read `os.environ`, exactly as forge reads the process environment, so a value is set
by exporting it before launching sevm (`payload=0x... uv run sevm run ...`) or with
`vm.setEnv` at the prompt. Each type has a scalar form and a `(name, delimiter)` array form;
`envOr` returns a caller-supplied default instead of reverting when the key is absent.
"""

from __future__ import annotations

import os
from typing import Any

from eth_utils import to_canonical_address, to_checksum_address

from .registry import CheatContext, CheatError, _cheat


def _parse_scalar(abi_type: str, raw: str) -> Any:
    """Parse one env string into the Python value eth_abi wants for `abi_type`."""
    raw = raw.strip()
    if abi_type == "bool":
        low = raw.lower()
        if low in ("true", "1"):
            return True
        if low in ("false", "0"):
            return False
        raise CheatError(f"env value {raw!r} is not a bool")
    if abi_type.startswith(("uint", "int")):
        return int(raw, 0)
    if abi_type == "address":
        return to_checksum_address(to_canonical_address(raw))
    if abi_type == "bytes32":
        b = bytes.fromhex(raw[2:] if raw.lower().startswith("0x") else raw)
        # forge rejects any other length rather than padding; see `parseBytes32`.
        if len(b) != 32:
            raise CheatError(f"env value {raw!r} is not 32 bytes")
        return b
    if abi_type == "bytes":
        return bytes.fromhex(raw[2:] if raw.lower().startswith("0x") else raw)
    return raw  # string


def _parse(abi_type: str, raw: str, delim: str | None) -> Any:
    """Scalar when `delim` is None, else the split-and-parsed array."""
    if delim is None:
        return _parse_scalar(abi_type, raw)
    inner = abi_type[:-2]  # strip the trailing "[]"
    parts = raw.split(delim) if raw != "" else []
    return [_parse_scalar(inner, p) for p in parts]


def _read(name: str) -> str:
    val = os.environ.get(name)
    if val is None:
        raise CheatError(f"environment variable {name!r} not found")
    return val


def _env_cheat(sol_type: str) -> None:
    """Register the scalar and array `env<Type>` overloads for one Solidity type."""
    abi = {"string": "string"}.get(sol_type, sol_type)
    name = (
        "env"
        + {
            "uint256": "Uint",
            "int256": "Int",
            "address": "Address",
            "bool": "Bool",
            "bytes32": "Bytes32",
            "bytes": "Bytes",
            "string": "String",
        }[sol_type]
    )

    @_cheat(
        f"{name}(string)",
        ret_types=[abi],
        family="env",
        doc=f"read env var as {sol_type}",
    )
    def _scalar(ctx: CheatContext, _abi: str = abi) -> list[Any]:
        return [_parse(_abi, _read(ctx.args[0]), None)]

    @_cheat(
        f"{name}(string,string)",
        ret_types=[abi + "[]"],
        family="env",
        doc=f"read env var as {sol_type}[] split on a delimiter",
    )
    def _array(ctx: CheatContext, _abi: str = abi) -> list[Any]:
        return [_parse(_abi + "[]", _read(ctx.args[0]), ctx.args[1])]


for _t in ("uint256", "int256", "address", "bool", "bytes32", "bytes", "string"):
    _env_cheat(_t)


@_cheat(
    "envExists(string)", ret_types=["bool"], family="env", doc="whether an env var is set"
)
def _env_exists(ctx: CheatContext) -> list[Any]:
    return [ctx.args[0] in os.environ]


@_cheat("setEnv(string,string)", family="env", doc="set a process env var")
def _set_env(ctx: CheatContext) -> None:
    os.environ[str(ctx.args[0])] = str(ctx.args[1])


# ---- envOr: read, or fall back to the default given as the last argument -----------------

# (signature, abi return type, index of the default arg). The default's decoded value is
# already the right Python shape, so on a miss we just hand it straight back.
_ENV_OR_SCALAR = [
    ("envOr(string,bool)", "bool"),
    ("envOr(string,uint256)", "uint256"),
    ("envOr(string,int256)", "int256"),
    ("envOr(string,address)", "address"),
    ("envOr(string,bytes32)", "bytes32"),
    ("envOr(string,bytes)", "bytes"),
    ("envOr(string,string)", "string"),
]
_ENV_OR_ARRAY = [
    ("envOr(string,string,bool[])", "bool[]"),
    ("envOr(string,string,uint256[])", "uint256[]"),
    ("envOr(string,string,int256[])", "int256[]"),
    ("envOr(string,string,address[])", "address[]"),
    ("envOr(string,string,bytes32[])", "bytes32[]"),
    ("envOr(string,string,bytes[])", "bytes[]"),
    ("envOr(string,string,string[])", "string[]"),
]


def _register_env_or() -> None:
    for sig, abi in _ENV_OR_SCALAR:

        @_cheat(sig, ret_types=[abi], family="env", doc=f"read env {abi}, or a default")
        def _scalar(ctx: CheatContext, _abi: str = abi) -> list[Any]:
            raw = os.environ.get(str(ctx.args[0]))
            if raw is None:
                return [ctx.args[1]]
            return [_parse(_abi, raw, None)]

    for sig, abi in _ENV_OR_ARRAY:

        @_cheat(sig, ret_types=[abi], family="env", doc=f"read env {abi}, or a default")
        def _array(ctx: CheatContext, _abi: str = abi) -> list[Any]:
            raw = os.environ.get(str(ctx.args[0]))
            if raw is None:
                return [list(ctx.args[2])]
            return [_parse(_abi, raw, str(ctx.args[1]))]


_register_env_or()
