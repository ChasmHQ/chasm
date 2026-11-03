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

from .builtins import BUILTINS, AsmError
from .parser import Call, Literal, parse


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
    rows: list[dict] = []
    for node in statements:
        value, spent = _evaluate_native(session.execute_opcode, node)
        rows.append(
            {
                "text": node.text,
                "name": node.name if isinstance(node, Call) else "literal",
                "value": value,
                "gas": spent,
            }
        )
    return rows
