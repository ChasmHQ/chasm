"""Frames, and the AST index that gives them names.

Two stacks are in play at once and conflating them is the classic Solidity-debugger bug:

  EVM frames       one per CALL/DELEGATECALL/STATICCALL/CREATE. Py-EVM gives us these for
                   free because each one re-enters `apply_computation`.
  internal frames  Solidity `internal`/`private` calls, and modifiers. These compile to
                   JUMP, so the EVM depth never changes. The only signal is the source
                   map's `i`/`o` jump field.

`bt` must interleave both or the user sees a call stack that is missing most of their
program. `next` must step over both or it dives into helper functions.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .srcmap import Location

# AST node types that own a source range we want to name.
_FUNCTION_NODES = {"FunctionDefinition", "ModifierDefinition"}


@dataclass(frozen=True)
class FunctionInfo:
    """A named source range from the AST."""

    name: str
    kind: str  # function | constructor | fallback | receive | modifier
    contract: str
    file_id: int
    start: int
    length: int
    visibility: str = ""
    state_mutability: str = ""
    parameters: tuple[tuple[str, str], ...] = ()  # (type, name)
    returns: tuple[tuple[str, str], ...] = ()
    ast_id: int = -1

    @property
    def end(self) -> int:
        return self.start + self.length

    @property
    def display_name(self) -> str:
        if self.kind == "constructor":
            return f"{self.contract}.constructor"
        if self.kind in ("fallback", "receive"):
            return f"{self.contract}.{self.kind}"
        return f"{self.contract}.{self.name}"

    @property
    def signature(self) -> str:
        args = ", ".join(t for t, _ in self.parameters)
        return f"{self.display_name}({args})"


def _parse_src(src: str) -> tuple[int, int, int]:
    """solc 'start:length:file' -> ints."""
    parts = (src or "0:0:-1").split(":")
    while len(parts) < 3:
        parts.append("-1")
    return int(parts[0]), int(parts[1]), int(parts[2])


def _param_list(node: dict | None) -> tuple[tuple[str, str], ...]:
    if not node:
        return ()
    out: list[tuple[str, str]] = []
    for p in node.get("parameters", []) or []:
        type_name = (p.get("typeDescriptions") or {}).get("typeString") or ""
        out.append((type_name, p.get("name") or ""))
    return tuple(out)


class FunctionIndex:
    """Source offset -> enclosing function, built from solc's AST.

    Ranges nest (a modifier body inside a function, a function inside a contract), so
    lookup returns the *innermost* match. A linear scan is fine: contracts have tens of
    functions, not thousands, and results are cached per location.
    """

    def __init__(self, asts: dict[str, dict]) -> None:
        self.functions: list[FunctionInfo] = []
        self.contracts: dict[int, list[tuple[int, int, str]]] = {}
        for ast in asts.values():
            self._walk(ast, contract="")
        # Innermost-first: shorter ranges win ties.
        self.functions.sort(key=lambda f: (f.file_id, f.start, f.length))
        self._cache: dict[tuple[int, int], FunctionInfo | None] = {}
        self._by_name: dict[str, list[FunctionInfo]] = {}
        for fn in self.functions:
            self._by_name.setdefault(fn.name, []).append(fn)
            self._by_name.setdefault(fn.display_name, []).append(fn)

    def _walk(self, node: Any, contract: str) -> None:
        if isinstance(node, list):
            for item in node:
                self._walk(item, contract)
            return
        if not isinstance(node, dict):
            return

        node_type = node.get("nodeType")
        if node_type == "ContractDefinition":
            contract = node.get("name") or contract
            start, length, file_id = _parse_src(node.get("src", ""))
            self.contracts.setdefault(file_id, []).append(
                (start, start + length, contract)
            )
        elif node_type in _FUNCTION_NODES:
            start, length, file_id = _parse_src(node.get("src", ""))
            kind = node.get("kind") or (
                "modifier" if node_type == "ModifierDefinition" else "function"
            )
            name = node.get("name") or kind
            self.functions.append(
                FunctionInfo(
                    name=name,
                    kind=kind,
                    contract=contract,
                    file_id=file_id,
                    start=start,
                    length=length,
                    visibility=node.get("visibility") or "",
                    state_mutability=node.get("stateMutability") or "",
                    parameters=_param_list(node.get("parameters")),
                    returns=_param_list(node.get("returnParameters")),
                    ast_id=int(node.get("id", -1)),
                )
            )

        for value in node.values():
            if isinstance(value, (dict, list)):
                self._walk(value, contract)

    # -- lookup -------------------------------------------------------------

    def at_offset(self, file_id: int, offset: int) -> FunctionInfo | None:
        key = (file_id, offset)
        if key in self._cache:
            return self._cache[key]
        best: FunctionInfo | None = None
        for fn in self.functions:
            if fn.file_id != file_id:
                continue
            if fn.start <= offset < fn.end and (best is None or fn.length < best.length):
                best = fn
        self._cache[key] = best
        return best

    def at_location(self, loc: Location | None) -> FunctionInfo | None:
        if loc is None or loc.is_generated:
            return None
        return self.at_offset(loc.file_id, loc.entry.start)

    def contract_at(self, file_id: int, offset: int) -> str:
        best: tuple[int, int, str] | None = None
        for start, end, name in self.contracts.get(file_id, []):
            if start <= offset < end and (
                best is None or (end - start) < (best[1] - best[0])
            ):
                best = (start, end, name)
        return best[2] if best else ""

    def find(self, name: str) -> list[FunctionInfo]:
        """Resolve `break funcName` or `break Contract.funcName`."""
        return list(self._by_name.get(name, []))


@dataclass
class InternalFrame:
    """A Solidity-level call that did not create an EVM frame."""

    function: FunctionInfo | None
    entry_pc: int
    call_site_pc: int = -1  # pc of the JUMP that entered this frame; gdb's caller line
    return_pc: int | None = None
    # Local-variable bookkeeping, filled in by the session as execution passes through.
    # `entry_sp` is the stack height at the frame's entry JUMPDEST, which is the base
    # every parameter and local is measured from. `slots` maps a declaration's AST id to
    # the absolute stack position its first word was observed at.
    entry_sp: int | None = None
    slots: dict[int, int] = field(default_factory=dict)

    @property
    def name(self) -> str:
        return self.function.signature if self.function else "<compiler-generated>"

    @property
    def is_generated(self) -> bool:
        """Solc's ABI encode/decode helpers have no AST function, so they get no name."""
        return self.function is None


@dataclass(frozen=True)
class BacktraceRow:
    """One line of `bt`, structured so the TUI can style it rather than parse it."""

    index: int
    name: str
    line: int
    pc: int
    kind: str  # solidity | evm
    detail: str = ""
    address: bytes | None = None
    evm_index: int = -1  # index into the session's EVM frame stack, for `frame N`
    internal_index: int = -1  # index into that frame's internal stack, for locals
    source_key: str | None = None

    def render(self) -> str:
        where = f"line {self.line}" if self.line else f"pc 0x{self.pc:x}"
        suffix = f" [{self.detail}]" if self.detail else ""
        return f"#{self.index}  {self.name} at {where}{suffix}"


@dataclass
class EvmFrame:
    """One Py-EVM computation, plus the internal call stack running inside it."""

    depth: int
    address: bytes  # storage_address, the account whose storage is in play
    code_address: bytes
    sender: bytes
    value: int
    calldata: bytes
    is_static: bool
    is_create: bool
    kind: str = "call"  # call | delegatecall | staticcall | create | tx
    artifact_name: str | None = None
    artifact: Any = None
    internal: list[InternalFrame] = field(default_factory=list)

    # Live objects. Only ever touched on the VM thread.
    computation: Any = None
    pc_map: Any = None
    disassembly: Any = None
    # pc -> AST id of the local variable that instruction allocates. Empty when the code
    # has no artifact, which is how a frame with no source degrades to no locals.
    decl_pcs: dict[int, int] = field(default_factory=dict)

    @property
    def internal_depth(self) -> int:
        return len(self.internal)

    def location(self, pc: int) -> Location | None:
        return self.pc_map.at(pc) if self.pc_map is not None else None


def stack_int(value: Any) -> int:
    """Py-EVM stack items are int OR bytes depending on how they were pushed."""
    if isinstance(value, int):
        return value
    return int.from_bytes(value, "big")


@dataclass(frozen=True)
class StackEntry:
    index: int  # 0 is top of stack
    value: int
    raw: Any

    def hex(self) -> str:
        return f"0x{self.value:x}"


@dataclass(frozen=True)
class FrameSnapshot:
    """Immutable picture of the VM at a pause, safe to hand to the UI thread.

    Deliberately contains no Py-EVM objects. The UI renders this; anything it wants that
    is not here it must ask the VM thread for with an inspect command.
    """

    step: int
    pc: int
    opcode: int
    mnemonic: str
    depth: int
    gas_remaining: int
    gas_used: int
    gas_limit: int
    gas_refund: int

    address: bytes
    code_address: bytes
    sender: bytes
    origin: bytes
    value: int
    calldata: bytes
    is_static: bool

    stack: tuple[StackEntry, ...]
    memory_size: int
    memory: bytes

    # Source attribution, absent when the code has no artifact.
    contract_name: str | None = None
    source_key: str | None = None
    file_id: int = -1
    line: int = 0
    col: int = 0
    end_line: int = 0
    jump: str = "-"
    function: FunctionInfo | None = None

    backtrace: tuple[BacktraceRow, ...] = ()
    locals: tuple[Any, ...] = ()  # LocalValue rows for the innermost Solidity frame
    stop_reason: str = "step"
    hit_breakpoints: tuple[int, ...] = ()
    static_gas: int | None = None
    annotation: str = ""

    @property
    def has_source(self) -> bool:
        return self.line > 0 and self.source_key is not None
