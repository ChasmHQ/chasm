"""Verbs that write to the live VM: `set`, inline assembly, and `jump`."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING

from .parsing import _integer
from .render import _escape
from .result import CommandResult

if TYPE_CHECKING:
    from .processor import CommandProcessor


def cmd_set(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """`set var <lvalue> = <value>` plus the low-level `set $pc/$gas/$stack[n]`."""
    body = rest.strip()
    if body.startswith("var "):
        body = body[4:].strip()
    if "=" not in body:
        return CommandResult(error="usage: set var <lvalue> = <expression>")
    lhs, rhs = body.split("=", 1)
    lhs, rhs = lhs.strip(), rhs.strip()

    if lhs.startswith("$"):
        return _set_convenience(proc, lhs, rhs)

    # A bare local name is a stack slot, not storage, and has to be written as one.
    if re.fullmatch(r"[A-Za-z_$][\w$]*", lhs) and proc.is_local(lhs):
        value = int(proc.evaluate(rhs).value)
        written = proc.inspect(
            "write_local", lhs, value, internal_index=proc.selected_internal
        )
        return CommandResult(mutated=True).add(
            f"[yellow]{_escape(lhs)}[/yellow] = {_escape(written['display'])}"
        )

    # Everything else goes through solc, so packed slots, mappings, structs and
    # arrays are all written correctly without reimplementing the layout rules.
    res = proc.evaluate(f"{lhs} = ({rhs})", keep=True)
    try:
        now = proc.evaluate(lhs)
        return CommandResult(mutated=True).add(
            f"[yellow]{_escape(lhs)}[/yellow] = {_escape(now.display)}"
        )
    except Exception:
        return CommandResult(mutated=True).add(
            f"[yellow]{_escape(lhs)}[/yellow] set  [dim](gas {res.gas_used})[/dim]"
        )


def _set_convenience(proc: CommandProcessor, lhs: str, rhs: str) -> CommandResult:
    value = int(proc.evaluate(rhs).value)
    name = lhs[1:]
    if name == "pc":
        proc.inspect("set_pc", value)
        return CommandResult(mutated=True).add(f"[yellow]$pc = 0x{value:x}[/yellow]")
    if name == "gas":
        proc.inspect("set_gas", value)
        return CommandResult(mutated=True).add(f"[yellow]$gas = {value:,}[/yellow]")
    match = re.match(r"^stack\[(\d+)\]$", name)
    if match:
        index = int(match.group(1))
        proc.inspect("write_stack", index, value)
        return CommandResult(mutated=True).add(
            f"[yellow]$stack[{index}] = 0x{value:x}[/yellow]"
        )
    match = re.match(r"^mem\[(0x[0-9a-fA-F]+|\d+)\]$", name)
    if match:
        offset = int(match.group(1), 0)
        proc.inspect("write_memory", offset, value.to_bytes(32, "big"))
        return CommandResult(mutated=True).add(
            f"[yellow]memory[0x{offset:x}] = 0x{value:x}[/yellow]"
        )
    match = re.match(r"^storage\[(0x[0-9a-fA-F]+|\d+)\]$", name)
    if match:
        slot = int(match.group(1), 0)
        proc.inspect("write_storage", slot, value)
        return CommandResult(mutated=True).add(
            f"[yellow]storage[0x{slot:x}] = 0x{value:x}[/yellow]"
        )
    return CommandResult(error=f"unknown convenience variable {lhs}")


# ==================================================================
# misc
# ==================================================================


def cmd_jump(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    if not args:
        return CommandResult(error="usage: jump <pc>")
    pc = _integer(args[0], "jump")
    proc.inspect("set_pc", pc)
    return CommandResult(mutated=True).add(
        f"[yellow]program counter set to 0x{pc:x}[/yellow]"
    )


# ==================================================================
# info subcommands
# ==================================================================


def cmd_asm(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """`asm <yul>`: run inline assembly against the frame we are stopped in."""
    source = rest.strip()
    if not source:
        return CommandResult(
            error="usage: asm <yul>, e.g. `asm mstore(0x80, 1)`; "
            "`help assembly` lists the builtins"
        )
    return proc.assemble(proc.substitute(source))


VERBS = {
    "set": cmd_set,
    "asm": cmd_asm,
    "assembly": cmd_asm,
    "yul": cmd_asm,
    "jump": cmd_jump,
}
