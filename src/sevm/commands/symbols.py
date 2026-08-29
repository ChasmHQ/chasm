"""Selectors, signatures, and the dispatcher route a call takes into a contract.

`sig` is the lookup (`cast sig` both ways: name or signature -> selector, selector ->
signature), `info address` the routing (selector -> the internal JUMPDEST the external
wrapper calls). Both work from the ABI when there is one and from the bytecode when
there is not, so they still answer for a proxy or an unrecognised contract.
"""

from __future__ import annotations

import string
from typing import TYPE_CHECKING

from eth_utils import keccak

from ..compile import Artifact
from ..disasm import Disassembly
from ..dispatch import Entry, entries, find_selector
from ..session import SessionError
from ..srcmap import PcMap
from .render import _escape
from .result import CommandResult

if TYPE_CHECKING:
    from .processor import CommandProcessor


def cmd_sig(proc: CommandProcessor, args: list[str], rest: str) -> CommandResult:
    """`cast sig`, both directions, against the contract in view.

    A name lists its overloads, a signature is hashed whether or not this ABI declares
    it, and a selector is looked up in reverse.
    """
    contract, target = _split_contract("".join(args))
    art, _code = _address_code(proc, contract, target)
    if not target:
        if art is None:
            raise SessionError("no ABI for the code in view; give a signature to hash")
        return _signature_listing(art)
    result = CommandResult()
    matches = _matching_signatures(art, target) if art is not None else []
    if art is not None and matches:
        width = max(len(sig) for sig in matches)
        for signature in sorted(matches):
            selector = int(art.method_identifiers[signature], 16)
            result.add(_signature_row(signature, selector, width))
        return result
    selector, signature, declared = _address_selector(art, target)
    if not signature:
        return result.add(
            f"[cyan]0x{selector:08x}[/cyan] [dim]no function with this selector in "
            f"{art.name if art else 'this code'}[/dim]"
        )
    note = "" if declared else "  [dim](hashed here; not in this ABI)[/dim]"
    return result.add(_signature_row(signature, selector) + note)


def _signature_row(signature: str, selector: int, width: int = 0) -> str:
    return f"[bold]{_escape(signature)}[/bold]{' ' * (width - len(signature))}  [cyan]0x{selector:08x}[/cyan]"


def _signature_listing(art: Artifact) -> CommandResult:
    result = CommandResult().add(f"[dim]{art.qualified_name}[/dim]")
    width = (
        max(len(sig) for sig in art.method_identifiers) if art.method_identifiers else 0
    )
    for signature in sorted(art.method_identifiers):
        selector = int(art.method_identifiers[signature], 16)
        result.add(_signature_row(signature, selector, width))
    return result


def info_address(proc: CommandProcessor, args: list[str]) -> CommandResult:
    """gdb's `info address`: where a symbol lives.

    Here that is the dispatcher's test, the external wrapper, and the internal JUMPDEST
    the wrapper calls. `b withdraw` breaks on the body's first source line; this is the
    pc a proxy, a raw-calldata call and an internal call all converge on.

    The target is a name, a canonical signature or a selector, optionally qualified as
    `Contract.name`. A signature no ABI here declares is hashed, as `cast sig` does, so
    a selector can be traced through code sevm never compiled.
    """
    contract, target = _split_contract("".join(args))
    art, code = _address_code(proc, contract, target)
    dis = proc.session.code.disassembly_for(code)
    if not target:
        return _address_listing(art, dis)
    selector, signature, declared = _address_selector(art, target)
    return _address_route(proc, art, dis, selector, signature, declared)


def _split_contract(text: str) -> tuple[str, str]:
    """`Bank.withdraw(uint256)` -> ('Bank', 'withdraw(uint256)'). A selector has no dot."""
    head, dot, tail = text.partition(".")
    if dot and head.isidentifier() and tail:
        return head, tail
    return "", text


def _address_code(
    proc: CommandProcessor, contract: str, target: str
) -> tuple[Artifact | None, bytes]:
    """Which code to read, and the artifact that names its functions (None if unknown).

    Unqualified, the contract in view wins, but only when it declares the target:
    `info address deposit` while stopped in a helper should find Bank rather than
    report that the helper has no `deposit`.
    """
    if contract:
        art = proc.project.artifact(contract)
        if art is None or not art.deployed_bytecode:
            raise SessionError(f"no compiled contract named {contract!r}")
        return art, art.deployed_bytecode
    snap = proc.snapshot
    here = (
        proc.project.artifact(snap.contract_name)
        if snap is not None and snap.contract_name
        else None
    )
    if here is not None and (not target or _matching_signatures(here, target)):
        return here, here.deployed_bytecode
    elsewhere = [
        art
        for art in proc.project.artifacts.values()
        if art.deployed_bytecode and target and _matching_signatures(art, target)
    ]
    if len(elsewhere) > 1:
        names = ", ".join(sorted(f"{art.name}.{target}" for art in elsewhere))
        raise SessionError(f"{target!r} is in several contracts: {names}")
    if elsewhere:
        return elsewhere[0], elsewhere[0].deployed_bytecode
    if here is not None:
        return here, here.deployed_bytecode
    if snap is not None:
        # Stopped in code with no artifact: read it back off the chain and work on that.
        code = bytes(proc.inspect("read_code", snap.code_address))
        if code:
            return None, code
    raise SessionError(
        "no contract in view; qualify the target as Contract.name, or run to a stop"
    )


def _is_selector(target: str) -> str:
    """The bare hex of a 4-byte selector, or '' if the target is not one."""
    raw = target[2:] if target[:2].lower() == "0x" else target
    ok = len(raw) == 8 and all(c in string.hexdigits for c in raw)
    return raw if ok else ""


def _matching_signatures(art: Artifact, target: str) -> list[str]:
    """Signatures in `art` the target names: by selector, by signature, or by bare name."""
    selector = _is_selector(target)
    if selector:
        found = [
            sig
            for sig, sel in art.method_identifiers.items()
            if sel.lower() == selector.lower()
        ]
        if found:
            return found
    if "(" in target:
        return [sig for sig in art.method_identifiers if sig == target]
    return [sig for sig in art.method_identifiers if sig.split("(")[0] == target]


def _address_selector(art: Artifact | None, target: str) -> tuple[int, str, bool]:
    """The selector the target means, the signature it came from, and whether the ABI has it.

    A signature the ABI does not declare is hashed rather than refused, which is the
    point against unknown code. The caller marks it, or the answer reads as something
    this contract implements.
    """
    matches = _matching_signatures(art, target) if art is not None else []
    if len(matches) > 1:
        raise SessionError(
            f"{target!r} is ambiguous; try one of: {', '.join(sorted(matches))}"
        )
    if matches and art is not None:
        return int(art.method_identifiers[matches[0]], 16), matches[0], True
    selector = _is_selector(target)
    if selector:
        return int(selector, 16), "", False
    if target.endswith(")") and "(" in target:
        return int.from_bytes(keccak(text=target)[:4], "big"), target, False
    where = f" in {art.name}" if art is not None else ""
    raise SessionError(
        f"no function {target!r}{where}; `info functions` lists them. A signature "
        "(`withdraw(uint256)`) or a selector (`0x2e1a7d4d`) works on any code"
    )


def _address_route(
    proc: CommandProcessor,
    art: Artifact | None,
    dis: Disassembly,
    selector: int,
    signature: str,
    declared: bool,
) -> CommandResult:
    result = CommandResult()
    if signature:
        title = f"{art.name}.{signature}" if art is not None and declared else signature
        result.add(f"[bold cyan]{_escape(title)}[/bold cyan]")
    result.add(f"[cyan]{'selector':<12}[/cyan] 0x{selector:08x}")
    entry = find_selector(dis, selector)
    if entry is None:
        return result.add(
            "[dim]the dispatcher never tests this selector: a call carrying it lands "
            "in the fallback[/dim]"
        )
    result.add(
        f"[cyan]{'dispatch':<12}[/cyan] 0x{entry.dispatch_pc:04x}"
        "  [dim]where the dispatcher compares it[/dim]"
    )
    result.add(
        f"[cyan]{'wrapper':<12}[/cyan] 0x{entry.wrapper_pc:04x}"
        "  [dim]the external entry: guards, then decodes calldata[/dim]"
    )
    if entry.internal_pc is None:
        result.add(f"[cyan]{'internal':<12}[/cyan] [dim]{entry.reason}[/dim]")
    else:
        where = _address_source(proc, art, entry.internal_pc)
        note = f"  [dim]{_escape(where)}[/dim]" if where else ""
        result.add(f"[cyan]{'internal':<12}[/cyan] 0x{entry.internal_pc:04x}{note}")
    return result.add(f"[dim]break there with `b *0x{entry.entry_pc:x}`[/dim]")


def _address_listing(art: Artifact | None, dis: Disassembly) -> CommandResult:
    """Every selector: named against the ABI when there is one, read off the code when not."""
    if art is None:
        result = CommandResult().add(
            "[dim]no artifact for this code; selectors only[/dim]"
        )
        for found in entries(dis):
            result.add(f"[cyan]{found.selector_hex}[/cyan]  {_address_pcs(found)}")
        return result
    result = CommandResult().add(f"[dim]{art.qualified_name}[/dim]")
    for signature in sorted(art.method_identifiers):
        selector = int(art.method_identifiers[signature], 16)
        entry = find_selector(dis, selector)
        route = _address_pcs(entry) if entry else "[dim]not in the dispatcher[/dim]"
        result.add(
            f"[cyan]0x{selector:08x}[/cyan] [bold]{_escape(signature):<34}[/bold] {route}"
        )
    return result


def _address_pcs(entry: Entry) -> str:
    internal = (
        f"0x{entry.internal_pc:04x}" if entry.internal_pc is not None else "[dim]-[/dim]"
    )
    return f"[dim]wrapper[/dim] 0x{entry.wrapper_pc:04x} [dim]internal[/dim] {internal}"


def _address_source(proc: CommandProcessor, art: Artifact | None, pc: int) -> str:
    """`Bank.sol:72` for the internal entry, when the artifact carries a source map."""
    if art is None or not art.deployed_source_map:
        return ""
    pcmap = PcMap(
        art.deployed_bytecode, art.deployed_source_map, proc.session.line_indexes
    )
    loc = pcmap.at(pc)
    if loc is None or loc.line == 0:
        return ""
    src = proc.project.source_by_id(loc.file_id)
    return f"{src.key}:{loc.line}" if src else f"line {loc.line}"


VERBS = {
    "sig": cmd_sig,
    "selector": cmd_sig,
}
