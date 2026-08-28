"""Splicing an expression into the contract's source, and reading the result back.

Return type is not known up front: the first attempt compiles as an unreachable struct so
solc's error names the real type. Probing with `uint256` would silently widen a
`uint96`/`uint8` and the debugger would lie about the type.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

EVAL_FUNCTION = "__sevm_eval"

PROBE_STRUCT = "__SevmProbe"
PROBE_DECL = f"struct {PROBE_STRUCT} {{ uint8 __sevm_x; }}"
PROBE_TYPE = f"{PROBE_STRUCT} memory"

# solc phrases the mismatch two different ways depending on where it is caught.
_TYPE_ERROR = re.compile(
    r"(?:Return argument type|Type)\s+(.+?)\s+is not implicitly convertible to expected type"
)
_VOID_ERROR = re.compile(
    r"(?:Different number of arguments in return statement"
    r"|Type tuple\(\) is not implicitly convertible)"
)

# solc type string -> the type we can actually declare and abi-decode.
_TYPE_FIXUPS = {
    "address payable": "address",
    "bool": "bool",
    "string": "string memory",
    "bytes": "bytes memory",
}


class EvalError(RuntimeError):
    """The expression could not be compiled or it reverted."""


@dataclass
class EvalResult:
    expression: str
    type_name: str  # as declared, e.g. "string memory"
    abi_type: str  # as abi-decoded, e.g. "string"
    value: Any
    display: str
    raw: bytes = b""
    gas_used: int = 0
    compile_ms: float = 0.0
    kept: bool = False
    void: bool = False

    def __str__(self) -> str:
        return self.display


def _normalise_type(solc_type: str) -> str:
    """Map a solc type string onto something declarable in a `returns (...)` clause."""
    t = solc_type.strip()
    t = re.sub(r"\s+", " ", t)
    # solc appends storage/memory/calldata location and pointer-ness to reference types.
    t = re.sub(r"\s+(storage|memory|calldata)(\s+(ref|pointer|slice))?$", "", t)
    if t in _TYPE_FIXUPS:
        return _TYPE_FIXUPS[t]
    if t.startswith("int_const"):
        return "int256" if "-" in t else "uint256"
    if t.startswith("rational_const"):
        return "uint256"
    if t.startswith("literal_string"):
        return "string memory"
    if t.startswith("contract "):
        return "address"
    if t.startswith("enum "):
        return "uint256"
    if t.startswith("type("):
        raise EvalError(f"cannot display a type expression ({t})")
    if t.startswith("mapping("):
        raise EvalError("cannot display a whole mapping; index it with a key")
    if t.startswith("tuple("):
        raise EvalError("expression returns multiple values; evaluate them one at a time")
    if t.startswith("function "):
        raise EvalError("cannot display a function reference; call it instead")
    if t.endswith("]") or t.startswith("struct "):
        # Arrays and structs must be returned from memory.
        base = re.sub(r"^struct\s+", "", t)
        return f"{base} memory"
    return t


def _abi_type_of(declared: str) -> str:
    return (
        declared.replace(" memory", "")
        .replace(" calldata", "")
        .replace(" storage", "")
        .strip()
    )


def _inject(
    source: str,
    expression: str,
    return_type: str | None,
    contract_range: tuple[int, int] = (-1, -1),
    probe: bool = False,
    parameters: str = "",
) -> str:
    """Add the eval function just inside the target contract's closing brace.

    `contract_range` comes from the AST; falling back to the file's last `}` is wrong
    whenever a file declares more than one contract (the common case).
    """
    start, end = contract_range
    if start >= 0 and 0 < end <= len(source) and source[end - 1] == "}":
        close = end - 1
    else:
        close = source.rstrip().rfind("}")
    if close < 0:
        raise EvalError("cannot locate the contract body to inject into")
    if return_type is None:
        body = f"    function {EVAL_FUNCTION}({parameters}) public payable {{ {expression}; }}\n"
    else:
        body = (
            f"    function {EVAL_FUNCTION}({parameters}) public payable returns ({return_type})"
            f" {{ return ({expression}); }}\n"
        )
    if probe:
        body = f"    {PROBE_DECL}\n" + body
    return source[:close] + "\n" + body + source[close:]


def _format_value(value: Any, abi_type: str) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str) and abi_type == "string":
        return f'"{value}"'
    if abi_type == "address" and isinstance(value, str):
        return value
    if isinstance(value, bytes):
        return "0x" + value.hex()
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(_format_value(v, "") for v in value) + "]"
    if isinstance(value, int):
        # Only show an ether reading where wei is a plausible reading (0.001 ether to a
        # billion). Above that it's likely a hash, address cast, or type(uintN).max.
        if abi_type.startswith("uint") and 10**15 <= value < 10**27:
            whole, frac = divmod(value, 10**18)
            if frac == 0:
                return f"{value} ({whole} ether)"
            return f"{value} ({value / 10**18:.6f} ether)"
        return str(value)
    return str(value)
