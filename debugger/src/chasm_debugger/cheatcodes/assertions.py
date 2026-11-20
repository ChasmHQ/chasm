"""The `vm.assert*` family.

forge-std's `*Decimal` and `*ApproxEq*` wrappers call the cheatcode unconditionally and
expect the VM to do the comparison, so these implement it for real and revert with forge's
message shape. A blanket revert would fail assertions that actually hold.

All 116 overloads are registered programmatically from an op x type matrix and share a
`family`, so `help cheatcodes` prints one row instead of 116.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from .registry import CheatContext, CheatError, _cheat, _fmt

ASSERT_FAMILY = "assert"

# The types forge-std asserts over, each also in its array form.
_ASSERT_TYPES = ("bool", "uint256", "int256", "address", "bytes32", "string", "bytes")

_ORDER_OPS: dict[str, tuple[Callable[[Any, Any], bool], str]] = {
    "assertGt": (lambda a, b: a > b, "<="),
    "assertGe": (lambda a, b: a >= b, "<"),
    "assertLt": (lambda a, b: a < b, ">="),
    "assertLe": (lambda a, b: a <= b, ">"),
}


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
