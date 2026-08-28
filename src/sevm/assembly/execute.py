"""Running parsed Yul against the live frame.

Arguments are evaluated depth-first and pushed onto the frame's real stack in EVM order
(first arg on top), Py-EVM's own opcode function runs, the result is read off the top, and
the stack is restored by slice-assignment. Nothing is reimplemented, so `keccak256`,
`mcopy` and `staticcall` behave exactly as they do mid-execution.

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


def run(session: Any, computation: Any, source: str) -> list[dict]:
    """Execute `source` against the paused frame and describe what each statement did.

    Returns one row per statement: `{"text", "name", "value", "gas"}`, where `value` is
    None for a statement that produces nothing (`mstore`, `sstore`, `log1`).

    Raises:
        AsmError: on a parse error or a failed opcode. Statements before the failing one
            have already run and are not undone; the EVM has no undo either.
    """
    statements = parse(source)
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
