"""Looking at things: expressions, memory, the backtrace, source and disassembly."""

from __future__ import annotations

from typing import TYPE_CHECKING

from ..frames import FrameSnapshot
from ..session import SessionError
from .parsing import _UNIT_SIZES, _X_FORMAT, _breakpoint_numbers
from .render import _escape, _memory_region
from .result import CommandResult

if TYPE_CHECKING:
    from .processor import CommandProcessor


def cmd_print(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    expr = rest.strip()
    if not expr:
        return CommandResult(error="usage: print <solidity expression>")
    res = proc.evaluate(expr)
    proc.history.append(res)
    idx = len(proc.history)
    return CommandResult().add(
        f"[bold]${idx}[/bold] = {_escape(res.display)}  [dim]({res.type_name})[/dim]"
    )


def cmd_call(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """gdb's `call`: evaluate and KEEP the side effects."""
    expr = rest.strip()
    if not expr:
        return CommandResult(error="usage: call <solidity expression>")
    res = proc.evaluate(expr, keep=True)
    if res.void:
        return CommandResult(mutated=True).add(
            f"[green]done[/green] [dim](gas {res.gas_used})[/dim]"
        )
    proc.history.append(res)
    return CommandResult(mutated=True).add(
        f"[bold]${len(proc.history)}[/bold] = {_escape(res.display)}  [dim]({res.type_name})[/dim]"
    )


def cmd_ptype(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    expr = rest.strip()
    if not expr:
        return CommandResult(error="usage: ptype <expression>")
    type_name = proc.inspect(
        "evaluate", proc.substitute(expr), internal_index=proc.selected_internal
    ).type_name
    return CommandResult().add(f"type = [bold]{type_name}[/bold]")


def cmd_display(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    expr = rest.strip()
    if not expr:
        return CommandResult(lines=proc.render_displays())
    proc._display_counter += 1
    proc.displays.append((proc._display_counter, expr))
    return CommandResult(lines=proc.render_displays()[-1:])


def cmd_undisplay(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    if not args:
        proc.displays.clear()
        return CommandResult().add("all displays removed")
    targets = set(_breakpoint_numbers(args, "undisplay"))
    proc.displays = [(n, e) for n, e in proc.displays if n not in targets]
    return CommandResult().add("removed")


def cmd_examine(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """gdb's `x/NFU addr` over EVM memory."""
    snap = proc.require_stop()
    spec, addr_text = "/8xg", ""
    tokens = rest.strip().split(None, 1)
    if tokens and tokens[0].startswith("/"):
        spec = tokens[0]
        addr_text = tokens[1] if len(tokens) > 1 else "0"
    elif tokens:
        addr_text = rest.strip()
    match = _X_FORMAT.match(spec)
    if not match:
        return CommandResult(error=f"bad format {spec!r}; try x/32xb 0x40")
    count = int(match.group(1) or 8)
    fmt = match.group(2) or "x"
    unit = _UNIT_SIZES.get(match.group(3) or "g", 8)

    try:
        offset = int(proc.evaluate(addr_text or "0").value) if addr_text else 0
    except Exception:
        offset = int(addr_text, 0) if addr_text else 0

    total = count * unit
    data = proc.inspect("read_memory", offset, total)
    result = CommandResult()
    if fmt == "s":
        text = bytes(data).split(b"\x00", 1)[0].decode("utf-8", "replace")
        return result.add(f'0x{offset:04x}: "{_escape(text)}"')
    per_row = max(1, min(count, 32 // unit if unit <= 32 else 1))
    if unit == 1:
        per_row = 16
    for row_start in range(0, total, per_row * unit):
        chunk = data[row_start : row_start + per_row * unit]
        cells = []
        for i in range(0, len(chunk), unit):
            word = chunk[i : i + unit]
            value = int.from_bytes(word, "big")
            if fmt == "d":
                cells.append(str(int.from_bytes(word, "big", signed=True)))
            elif fmt in ("u", "o", "t"):
                cells.append({"u": str(value), "o": oct(value), "t": bin(value)}[fmt])
            elif fmt == "c":
                cells.append("".join(chr(b) if 32 <= b < 127 else "." for b in word))
            else:
                cells.append(f"{value:0{unit * 2}x}")
        annotation = _memory_region(offset + row_start)
        result.add(
            f"[cyan]0x{offset + row_start:04x}[/cyan]: {' '.join(cells)}"
            + (f"  [dim]{annotation}[/dim]" if annotation else "")
        )
    if offset + total > snap.memory_size:
        result.add(
            f"[dim](memory is {snap.memory_size} bytes; the rest reads as zero)[/dim]"
        )
    return result


def cmd_backtrace(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    snap = proc.require_stop()
    result = CommandResult()
    for row in snap.backtrace:
        marker = (
            "[bold yellow]->[/bold yellow]" if row.index == proc.selected_row else "  "
        )
        colour = "cyan" if row.kind == "solidity" else "magenta"
        key = row.source_key or snap.source_key or "?"
        where = f"{key}:{row.line}" if row.line else f"pc 0x{row.pc:x}"
        detail = f" [dim][{row.detail}][/dim]" if row.detail else ""
        result.add(
            f"{marker} [dim]#{row.index}[/dim] [{colour}]{_escape(row.name)}[/{colour}] at [green]{where}[/green]{detail}"
        )
    return result


def cmd_frame(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    snap = proc.require_stop()
    if not args:
        return CommandResult(lines=proc.describe_stop(snap))
    if not args[0].lstrip("-").isdigit():
        raise SessionError(f"frame takes a number, not {args[0]!r}; see `bt`")
    index = int(args[0])
    rows = snap.backtrace
    if not 0 <= index < len(rows):
        return CommandResult(error=f"no frame #{index}")
    proc.selected_row = index
    proc.selected_frame = rows[index].evm_index
    proc.selected_internal = (
        rows[index].internal_index if rows[index].internal_index >= 0 else None
    )
    row = rows[index]
    where = (
        f"{row.source_key or snap.source_key}:{row.line}"
        if row.line
        else f"pc 0x{row.pc:x}"
    )
    return CommandResult().add(
        f"#{row.index}  [cyan]{_escape(row.name)}[/cyan] at [green]{where}[/green]"
    )


def cmd_up(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return _move_frame(proc, +1)


def cmd_down(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    return _move_frame(proc, -1)


def _move_frame(proc: CommandProcessor, delta: int) -> CommandResult:
    """`up` moves toward the caller, `down` toward the callee, as in gdb."""
    snap = proc.require_stop()
    rows = snap.backtrace
    target = proc.selected_row + delta
    if not 0 <= target < len(rows):
        return CommandResult(
            error="already at the outermost frame"
            if delta > 0
            else "already at the innermost frame"
        )
    return cmd_frame(proc, [str(target)], str(target))


def cmd_list(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    snap = proc.snapshot
    source_key = (
        snap.source_key
        if snap and snap.source_key
        else next(iter(proc.project.sources), None)
    )
    centre = snap.line if snap and snap.line else 1
    if args:
        if ":" in args[0]:
            source_key, line_text = args[0].rsplit(":", 1)
            centre = int(line_text)
        elif args[0].isdigit():
            centre = int(args[0])
    lines = proc.source_lines(source_key)
    if not lines:
        return CommandResult(error=f"no source for {source_key}")
    start = max(1, centre - 5)
    end = min(len(lines), start + 10)
    result = CommandResult().add(f"[dim]{source_key}[/dim]")
    exec_lines = _executable_lines(proc, snap)
    for n in range(start, end + 1):
        here = snap is not None and n == snap.line
        gutter = (
            "[bold yellow]->[/bold yellow]"
            if here
            else ("[dim] .[/dim]" if n in exec_lines else "  ")
        )
        body = _escape(lines[n - 1])
        result.add(
            f"{gutter} [dim]{n:>4}[/dim]  " + (f"[bold]{body}[/bold]" if here else body)
        )
    proc.last_list_line = end
    return result


def _executable_lines(proc: CommandProcessor, snap: FrameSnapshot | None) -> set:
    if snap is None or not snap.contract_name:
        return set()
    art = proc.project.artifact(snap.contract_name)
    if art is None or not art.deployed_source_map:
        return set()
    from ..srcmap import PcMap

    pcmap = PcMap(
        art.deployed_bytecode, art.deployed_source_map, proc.session.line_indexes
    )
    return set(pcmap.executable_lines(snap.file_id))


def cmd_disassemble(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    proc.require_stop()
    rows = proc.inspect("disassembly", 8, 20)
    result = CommandResult()
    for row in rows:
        marker = "[bold yellow]=>[/bold yellow]" if row["current"] else "  "
        label = "[magenta]" if row["jumpdest"] else ""
        close = "[/magenta]" if row["jumpdest"] else ""
        line = f" [dim]L{row['line']}[/dim]" if row["line"] else ""
        result.add(
            f"{marker} [cyan]{row['pc']:04x}[/cyan]  {label}{row['text']}{close}{line}"
        )
    return result


VERBS = {
    "print": cmd_print,
    "p": cmd_print,
    "inspect": cmd_print,
    "call": cmd_call,
    "ptype": cmd_ptype,
    "display": cmd_display,
    "undisplay": cmd_undisplay,
    "x": cmd_examine,
    "backtrace": cmd_backtrace,
    "bt": cmd_backtrace,
    "where": cmd_backtrace,
    "frame": cmd_frame,
    "f": cmd_frame,
    "up": cmd_up,
    "down": cmd_down,
    "list": cmd_list,
    "l": cmd_list,
    "disassemble": cmd_disassemble,
    "disas": cmd_disassemble,
}
