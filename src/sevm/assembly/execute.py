"""Running parsed Yul against the live frame.

Arguments are evaluated depth-first and passed to the active engine's opcode implementation
in EVM order. Nothing is reimplemented, so arithmetic, memory, storage and environment
operations behave exactly as they do mid-execution.

Two departures from real execution: gas is metered then refunded, since inspection must not
be able to induce an out-of-gas; memory expansion is kept, since the op genuinely wrote
there.
"""

from __future__ import annotations

from typing import Any

from eth.exceptions import VMError

from .builtins import BUILTINS, MAX_UINT256, AsmError, Builtin
from .parser import Call, Literal, parse


def _as_int(value: Any) -> int:
    """Py-EVM stack items are int OR bytes depending on how they were pushed."""
    if isinstance(value, int):
        return value
    return int.from_bytes(value, "big")


def _push(stack: Any, value: int, where: str) -> None:
    if not 0 <= value < MAX_UINT256:
        raise AsmError(f"`{where}`: {value} does not fit in a 256-bit word")
    stack.push_int(value)


def _apply(
    session: Any, computation: Any, builtin: Builtin, args: list[int]
) -> int | None:
    """Run one opcode against the live frame, leaving the stack exactly as it was."""
    opcode_fn = computation.opcodes.get(builtin.opcode)
    if opcode_fn is None:
        raise AsmError(f"`{builtin.name}` is not available in this fork")
    stack = computation._stack
    # Slice-assignment, never rebinding: Py-EVM's Stack caches `append`/`pop` bound to the
    # list object it was constructed with, so a fresh list would silently detach them.
    saved = list(stack.values)
    try:
        for value in reversed(args):
            _push(stack, value, builtin.name)
        # Suspended, so a call opcode that re-enters `apply_computation` runs untraced
        # instead of trying to pause a debugger that is already parked.
        with session.suspended():
            opcode_fn(computation=computation)
        if not builtin.outputs:
            return None
        if len(stack.values) <= len(saved):
            raise AsmError(f"`{builtin.name}` produced no result")
        return _as_int(stack.values[-1])
    except VMError as exc:
        detail = str(exc) or type(exc).__name__
        raise AsmError(f"`{builtin.name}` failed: {detail}") from exc
    finally:
        stack.values[:] = saved


def _evaluate(session: Any, computation: Any, node: Call | Literal) -> int | None:
    if isinstance(node, Literal):
        return node.value
    args: list[int] = []
    for arg in node.args:
        value = _evaluate(session, computation, arg)
        if value is None:
            name = arg.name if isinstance(arg, Call) else "?"
            raise AsmError(f"`{name}` returns nothing, so it cannot be an argument")
        args.append(value)
    return _apply(session, computation, BUILTINS[node.name], args)


def _evaluate_native(execute: Any, node: Call | Literal) -> tuple[int | None, int]:
    if isinstance(node, Literal):
        return node.value, 0
    args: list[int] = []
    gas = 0
    for arg in node.args:
        value, spent = _evaluate_native(execute, arg)
        gas += spent
        if value is None:
            name = arg.name if isinstance(arg, Call) else "?"
            raise AsmError(f"`{name}` returns nothing, so it cannot be an argument")
        args.append(value)
    builtin = BUILTINS[node.name]
    try:
        result = execute(builtin.opcode, args, builtin.outputs)
    except Exception as exc:
        raise AsmError(f"`{builtin.name}` failed: {exc}") from exc
    value = result.get("value")
    return (int(value, 16) if value is not None else None), gas + int(result["gas_used"])


def run(session: Any, computation: Any, source: str) -> list[dict]:
    """Execute `source` against the paused frame and describe what each statement did.

    Returns one row per statement: `{"text", "name", "value", "gas"}`, where `value` is
    None for a statement that produces nothing (`mstore`, `sstore`, `log1`).

    Raises:
        AsmError: on a parse error or a failed opcode. Statements before the failing one
            have already run and are not undone; the EVM has no undo either.
    """
    statements = parse(source)
    execute = getattr(session, "execute_opcode", None)
    if execute is not None:
        rows = []
        for node in statements:
            value, spent = _evaluate_native(execute, node)
            rows.append(
                {
                    "text": node.text,
                    "name": node.name if isinstance(node, Call) else "literal",
                    "value": value,
                    "gas": spent,
                }
            )
        return rows

    meter = computation._gas_meter
    rows: list[dict] = []
    for node in statements:
        before = meter.gas_remaining
        try:
            value = _evaluate(session, computation, node)
            spent = before - meter.gas_remaining
        finally:
            # Cost is real and worth reporting, but inspection must not starve the
            # transaction of gas, so the meter is restored.
            meter.gas_remaining = before
        rows.append(
            {
                "text": node.text,
                "name": node.name if isinstance(node, Call) else "literal",
                "value": value,
                "gas": spent,
            }
        )
    return rows
