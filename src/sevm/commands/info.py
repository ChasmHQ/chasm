"""`info <topic>`: the state of the machine, the frame, and the contract.

`registers`, `frame`, `args`, `locals`, `address` and `breakpoints` are gdb's. `storage`,
`gas`, `logs`, `sources` and `functions` have no gdb equivalent and are ours.
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING

from ..decode import decode_calldata
from ..frames import FrameSnapshot
from .render import _addr, _calldata, _escape, _event_name, _short, _wei
from .result import CommandResult
from .symbols import info_address

if TYPE_CHECKING:
    from .processor import CommandProcessor


def cmd_info(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    if not args:
        return CommandResult(
            error="usage: info <registers|breakpoints|frame|args|locals|storage|gas|logs|sources|functions|address|watchpoints>"
        )
    topic = args[0]
    table = {
        "registers": _info_registers,
        "r": _info_registers,
        "reg": _info_registers,
        "breakpoints": _info_breakpoints,
        "b": _info_breakpoints,
        "break": _info_breakpoints,
        "watchpoints": _info_breakpoints,
        "frame": _info_frame,
        "args": _info_args,
        "locals": _info_locals,
        "storage": _info_storage,
        "gas": _info_gas,
        "logs": _info_logs,
        "sources": _info_sources,
        "functions": _info_functions,
        "address": info_address,
    }
    handler = table.get(topic)
    if handler is None:
        return CommandResult(error=f"unknown info topic {topic!r}")
    return handler(proc, args[1:])


def _info_registers(proc: CommandProcessor, args: list[str]) -> CommandResult:
    snap = proc.require_stop()
    result = CommandResult()
    rows = [
        ("pc", f"0x{snap.pc:04x}"),
        ("opcode", f"{snap.mnemonic} (0x{snap.opcode:02x})"),
        (
            "gas",
            f"{snap.gas_remaining:,} remaining / {snap.gas_used:,} used of {snap.gas_limit:,}",
        ),
        ("refund", f"{snap.gas_refund:,}"),
        ("depth", str(snap.depth)),
        ("sp", f"{len(snap.stack)} items"),
        ("memory", f"{snap.memory_size:,} bytes"),
        ("calldatasize", f"{len(snap.calldata):,} bytes"),
        ("address", _addr(snap.address)),
        ("code", _addr(snap.code_address)),
        ("msg.sender", _addr(snap.sender)),
        ("msg.value", _wei(snap.value)),
        ("tx.origin", _addr(snap.origin)),
        ("static", "yes" if snap.is_static else "no"),
        ("step", str(snap.step)),
    ]
    for name, value in rows:
        result.add(f"[cyan]{name:<12}[/cyan] {value}")
    return result


def _info_breakpoints(proc: CommandProcessor, args: list[str]) -> CommandResult:
    rows = proc.session.breakpoints.listing()
    if not rows:
        return CommandResult().add("[dim]no breakpoints or watchpoints[/dim]")
    result = CommandResult().add("[dim]Num Type     What[/dim]")
    for row in rows:
        result.add(_escape(row))
    return result


def _info_frame(proc: CommandProcessor, args: list[str]) -> CommandResult:
    info = proc.inspect("frame_info")
    result = CommandResult()
    for key in ("depth", "kind", "artifact", "is_static", "gas_remaining"):
        result.add(f"[cyan]{key:<14}[/cyan] {info[key]}")
    if proc.session.estimations:
        result.add(
            f"[cyan]{'estimates':<14}[/cyan] {proc.session.estimations} gas-estimation "
            "pass(es) skipped [dim](they re-run the tx; not debugged)[/dim]"
        )
    for key in ("address", "code_address", "sender"):
        result.add(f"[cyan]{key:<14}[/cyan] {_addr(info[key])}")
    result.add(f"[cyan]{'value':<14}[/cyan] {_wei(info['value'])}")
    result.add(f"[cyan]{'calldata':<14}[/cyan] {_calldata(info['calldata'])}")
    if info["internal"]:
        result.add(
            f"[cyan]{'internal':<14}[/cyan] " + " <- ".join(reversed(info["internal"]))
        )
    return result


def _info_args(proc: CommandProcessor, args: list[str]) -> CommandResult:
    snap = proc.require_stop()
    params = [row for row in proc.read_locals() if row["kind"] == "param"]
    if params:
        # The arguments of the frame we are *in*, which for an internal call is not
        # what calldata holds.
        rows = snap.backtrace
        name = rows[proc.selected_row].name if 0 <= proc.selected_row < len(rows) else ""
        result = CommandResult().add(f"[bold cyan]{_escape(name)}[/bold cyan]")
        for row in params:
            body = (
                f"[bold]{_escape(str(row['value']))}[/bold]"
                if row["available"]
                else f"[dim]{_escape(str(row['value']))}[/dim]"
            )
            result.add(
                f"  [cyan]{row['name']:<14}[/cyan] [dim]{row['type']:<18}[/dim] = {body}"
            )
        return result
    info = proc.inspect("frame_info")
    art = proc.project.artifact(snap.contract_name) if snap.contract_name else None
    if art is None:
        return CommandResult().add("[dim]no ABI for this frame[/dim]")
    decoded = decode_calldata(art.abi, bytes(info["calldata"]))
    if decoded is None:
        return CommandResult().add(
            "[dim]calldata does not match any function in the ABI[/dim]"
        )
    signature, params = decoded
    result = CommandResult().add(f"[bold cyan]{signature}[/bold cyan]")
    if not params:
        result.add("[dim](no arguments)[/dim]")
    for type_name, name, value in params:
        shown = _addr(value) if isinstance(value, bytes) and len(value) == 20 else value
        result.add(
            f"  [cyan]{name or '_':<14}[/cyan] [dim]{type_name}[/dim] = {_escape(str(shown))}"
        )
    if (
        snap.backtrace
        and snap.backtrace[0].kind == "solidity"
        and snap.function
        and snap.function.name not in signature
    ):
        result.add(
            "[dim]note: these are the arguments of the external call, not of the "
            "internal function you are stopped in[/dim]"
        )
    return result


def _info_locals(proc: CommandProcessor, args: list[str]) -> CommandResult:
    snap = proc.require_stop()
    rows = proc.read_locals()
    result = CommandResult()
    if not rows:
        return result.add(f"[dim]{_no_locals_reason(proc, snap)}[/dim]")
    for row in rows:
        name = row["name"]
        kind = "" if row["kind"] == "local" else f" [dim]({row['kind']})[/dim]"
        value = row["value"]
        body = (
            f"[bold]{_escape(str(value))}[/bold]"
            if row["available"]
            else f"[dim]{_escape(str(value))}[/dim]"
        )
        note = f"  [dim]{_escape(row['reason'])}[/dim]" if row["reason"] else ""
        result.add(
            f"  [cyan]{name:<14}[/cyan] [dim]{row['type']:<18}[/dim] = {body}{kind}{note}"
        )
    return result


def _no_locals_reason(proc: CommandProcessor, snap: FrameSnapshot) -> str:
    """gdb prints "No symbol table info available"; say which of the two it is."""
    rows = snap.backtrace
    if 0 <= proc.selected_row < len(rows):
        row = rows[proc.selected_row]
        if row.kind == "solidity" and "compiler-generated" not in row.detail:
            return f"no locals in {row.name}"
    return (
        "no locals here: this frame has no Solidity source, or execution is in "
        "compiler-generated code"
    )


def _info_storage(proc: CommandProcessor, args: list[str]) -> CommandResult:
    snap = proc.require_stop()
    contract = args[0] if args else snap.contract_name
    decoder = proc.decoder(contract)
    if not decoder:
        return CommandResult().add("[dim]no storage layout for this contract[/dim]")
    reader = lambda slot: proc.inspect("read_storage", slot)  # noqa: E731
    result = CommandResult().add(f"[dim]{contract} at {_addr(snap.address)}[/dim]")
    for var, value in decoder.read_all(reader):
        warm = ""
        with contextlib.suppress(Exception):
            warm = (
                " [dim](warm)[/dim]"
                if proc.inspect("is_warm", var.slot)
                else " [dim](cold)[/dim]"
            )
        result.add(
            f"  [dim]slot {var.slot:>3}+{var.offset:<2}[/dim] "
            f"[cyan]{var.name:<16}[/cyan] [dim]{var.type_label:<22}[/dim] "
            f"= [bold]{_escape(value.display[:60])}[/bold]{warm}"
        )
    return result


def _info_gas(proc: CommandProcessor, args: list[str]) -> CommandResult:
    snap = proc.require_stop()
    result = CommandResult()
    result.add(f"[cyan]{'limit':<12}[/cyan] {snap.gas_limit:,}")
    result.add(f"[cyan]{'used':<12}[/cyan] {snap.gas_used:,}")
    result.add(f"[cyan]{'remaining':<12}[/cyan] {snap.gas_remaining:,}")
    result.add(f"[cyan]{'refund':<12}[/cyan] {snap.gas_refund:,}")
    if snap.static_gas is not None:
        result.add(
            f"[cyan]{'this op':<12}[/cyan] {snap.mnemonic} base cost {snap.static_gas}"
        )
    by_line = sorted(proc.session.gas_by_line.items(), key=lambda kv: -kv[1])[:12]
    if by_line:
        result.add("")
        result.add("[dim]gas by source line (highest first)[/dim]")
        lines = proc.source_lines(snap.source_key)
        for (_file_id, line), spent in by_line:
            text = lines[line - 1].strip()[:46] if 0 < line <= len(lines) else ""
            result.add(
                f"  [magenta]{spent:>9,}[/magenta] [dim]L{line:<4}[/dim] {_escape(text)}"
            )
    by_op = sorted(proc.session.gas_by_opcode.items(), key=lambda kv: -kv[1])[:8]
    if by_op:
        result.add("")
        result.add("[dim]gas by opcode[/dim]")
        for name, spent in by_op:
            result.add(f"  [magenta]{spent:>9,}[/magenta] {name}")
    return result


def _info_logs(proc: CommandProcessor, args: list[str]) -> CommandResult:
    snap = proc.require_stop()
    records = proc.inspect("logs")
    if not records:
        return CommandResult().add("[dim]no events emitted yet in this frame[/dim]")
    art = proc.project.artifact(snap.contract_name) if snap.contract_name else None
    result = CommandResult()
    for address, topics, data in records:
        name = _event_name(art.abi if art else [], topics)
        result.add(f"[cyan]{name}[/cyan] [dim]from {_short(address)}[/dim]")
        for i, topic in enumerate(topics):
            result.add(f"    [dim]topic{i}[/dim] 0x{topic:064x}")
        if data:
            result.add(f"    [dim]data  [/dim] 0x{bytes(data).hex()}")
    return result


def _info_sources(proc: CommandProcessor, args: list[str]) -> CommandResult:
    result = CommandResult()
    for key, src in proc.project.sources.items():
        result.add(
            f"[cyan]{key}[/cyan] [dim]({len(src.text.splitlines())} lines, id {src.file_id})[/dim]"
        )
    return result


def _info_functions(proc: CommandProcessor, args: list[str]) -> CommandResult:
    pattern = args[0] if args else None
    result = CommandResult()
    for fn in proc.session.functions.functions:
        if fn.kind == "modifier":
            continue
        if pattern and pattern not in fn.display_name:
            continue
        index = proc.session.line_indexes.get(fn.file_id)
        line = index.line_col(fn.start)[0] if index else 0
        visibility = f"[dim]{fn.visibility}[/dim] " if fn.visibility else ""
        result.add(
            f"{visibility}[cyan]{_escape(fn.signature)}[/cyan] [dim]line {line}[/dim]"
        )
    return result


# ==================================================================
# mutation
# ==================================================================


VERBS = {
    "info": cmd_info,
    "i": cmd_info,
}
