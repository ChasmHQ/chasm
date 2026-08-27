"""Breakpoints and watchpoints.

Numbering is shared across both, exactly as gdb does it, so `delete 3` is unambiguous.

Matching runs on the VM thread once per opcode, so it has to stay cheap. The set is
bucketed by kind and, for line breakpoints, resolved to concrete program counters when the
breakpoint is created rather than on every step.
"""

from __future__ import annotations

import threading
from collections.abc import Sequence
from dataclasses import dataclass, field

BP_LINE = "line"
BP_FUNCTION = "function"
BP_PC = "pc"
BP_OPCODE = "opcode"

WATCH_WRITE = "w"
WATCH_READ = "r"
WATCH_ACCESS = "a"


@dataclass
class Breakpoint:
    number: int
    kind: str
    location: str  # what the user typed, for `info breakpoints`
    enabled: bool = True
    temporary: bool = False
    condition: str | None = None
    ignore_count: int = 0
    hit_count: int = 0

    # Resolved targets. A line breakpoint can cover several pcs across contracts.
    file_id: int = -1
    line: int = 0
    pcs: set[int] = field(default_factory=set)
    opcode: str | None = None
    contract: str | None = None
    pending: bool = False  # source line known, but no code loaded for it yet
    condition_error: str | None = None  # last failure from evaluating `condition`

    def describe(self) -> str:
        bits = [f"{self.number:<3} {self.kind:<8} {self.location}"]
        if not self.enabled:
            bits.append("(disabled)")
        if self.temporary:
            bits.append("(temporary)")
        if self.condition:
            bits.append(f"if {self.condition}")
        if self.ignore_count:
            bits.append(f"ignore {self.ignore_count}")
        if self.hit_count:
            bits.append(f"hits {self.hit_count}")
        if self.pending:
            bits.append("<pending>")
        if self.condition_error:
            bits.append(f"[condition failed: {self.condition_error}]")
        return " ".join(bits)


@dataclass
class Watchpoint:
    number: int
    kind: str  # storage | memory
    expression: str
    mode: str = WATCH_WRITE
    enabled: bool = True
    hit_count: int = 0

    address: bytes | None = None
    slot: int | None = None
    offset: int | None = None
    size: int = 32
    old_value: int | None = None
    initialised: bool = False

    def describe(self) -> str:
        scope = "storage" if self.kind == "storage" else "memory"
        label = {WATCH_WRITE: "watch", WATCH_READ: "rwatch", WATCH_ACCESS: "awatch"}[
            self.mode
        ]
        state = "" if self.enabled else " (disabled)"
        hits = f" hits {self.hit_count}" if self.hit_count else ""
        return f"{self.number:<3} {label:<8} {scope} {self.expression}{state}{hits}"


class BreakpointSet:
    """Thread-safe container. The UI mutates it while the VM thread may be running."""

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._counter = 0
        self.breakpoints: dict[int, Breakpoint] = {}
        self.watchpoints: dict[int, Watchpoint] = {}

    def _next_number(self) -> int:
        self._counter += 1
        return self._counter

    # -- creation -----------------------------------------------------------

    def add_line(
        self,
        location: str,
        file_id: int,
        line: int,
        pcs: Sequence[int],
        temporary: bool = False,
        condition: str | None = None,
        contract: str | None = None,
    ) -> Breakpoint:
        with self._lock:
            bp = Breakpoint(
                number=self._next_number(),
                kind=BP_LINE,
                location=location,
                temporary=temporary,
                condition=condition,
                file_id=file_id,
                line=line,
                pcs=set(pcs),
                contract=contract,
                pending=not pcs,
            )
            self.breakpoints[bp.number] = bp
            return bp

    def add_pc(
        self, pc: int, temporary: bool = False, condition: str | None = None
    ) -> Breakpoint:
        with self._lock:
            bp = Breakpoint(
                number=self._next_number(),
                kind=BP_PC,
                location=f"*0x{pc:x}",
                temporary=temporary,
                condition=condition,
                pcs={pc},
            )
            self.breakpoints[bp.number] = bp
            return bp

    def add_opcode(
        self, mnemonic: str, temporary: bool = False, condition: str | None = None
    ) -> Breakpoint:
        with self._lock:
            bp = Breakpoint(
                number=self._next_number(),
                kind=BP_OPCODE,
                location=mnemonic,
                temporary=temporary,
                condition=condition,
                opcode=mnemonic.upper(),
            )
            self.breakpoints[bp.number] = bp
            return bp

    def add_function(
        self,
        location: str,
        file_id: int,
        line: int,
        pcs: Sequence[int],
        temporary: bool = False,
        condition: str | None = None,
        contract: str | None = None,
    ) -> Breakpoint:
        with self._lock:
            bp = Breakpoint(
                number=self._next_number(),
                kind=BP_FUNCTION,
                location=location,
                temporary=temporary,
                condition=condition,
                file_id=file_id,
                line=line,
                pcs=set(pcs),
                pending=not pcs,
                contract=contract,
            )
            self.breakpoints[bp.number] = bp
            return bp

    def add_watch(
        self,
        expression: str,
        kind: str = "storage",
        mode: str = WATCH_WRITE,
        address: bytes | None = None,
        slot: int | None = None,
        offset: int | None = None,
        size: int = 32,
    ) -> Watchpoint:
        with self._lock:
            wp = Watchpoint(
                number=self._next_number(),
                kind=kind,
                expression=expression,
                mode=mode,
                address=address,
                slot=slot,
                offset=offset,
                size=size,
            )
            self.watchpoints[wp.number] = wp
            return wp

    # -- management ---------------------------------------------------------

    def remove(self, number: int) -> bool:
        with self._lock:
            return (
                self.breakpoints.pop(number, None) is not None
                or self.watchpoints.pop(number, None) is not None
            )

    def clear(self) -> None:
        with self._lock:
            self.breakpoints.clear()
            self.watchpoints.clear()

    def set_enabled(self, number: int | None, enabled: bool) -> int:
        with self._lock:
            targets = (
                list(self.breakpoints.values()) + list(self.watchpoints.values())
                if number is None
                else [
                    x
                    for x in (
                        self.breakpoints.get(number),
                        self.watchpoints.get(number),
                    )
                    if x is not None
                ]
            )
            for t in targets:
                t.enabled = enabled
            return len(targets)

    def listing(self) -> list[str]:
        with self._lock:
            rows = [
                bp.describe()
                for bp in sorted(self.breakpoints.values(), key=lambda b: b.number)
            ]
            rows += [
                wp.describe()
                for wp in sorted(self.watchpoints.values(), key=lambda w: w.number)
            ]
            return rows

    @property
    def is_empty(self) -> bool:
        with self._lock:
            return not self.breakpoints and not self.watchpoints

    @property
    def has_watchpoints(self) -> bool:
        with self._lock:
            return any(w.enabled for w in self.watchpoints.values())

    # -- matching (hot path) ------------------------------------------------

    def match(
        self,
        pc: int,
        mnemonic: str,
        file_id: int,
        line: int,
        contract_name: str | None,
    ) -> list[Breakpoint]:
        """Breakpoints whose *static* location matches. Conditions are evaluated later."""
        hits: list[Breakpoint] = []
        with self._lock:
            for bp in self.breakpoints.values():
                if not bp.enabled:
                    continue
                if bp.kind == BP_OPCODE:
                    if bp.opcode == mnemonic:
                        hits.append(bp)
                elif bp.kind == BP_PC:
                    if pc in bp.pcs:
                        hits.append(bp)
                else:  # line, function
                    # A contract-scoped breakpoint fires only in that contract's code. If
                    # the running code is unrecognised (contract_name is None, e.g. a
                    # constructor's creation code), a scoped breakpoint must NOT fire, or a
                    # coincidental pc match in another same-file contract triggers it.
                    if bp.contract and bp.contract != contract_name:
                        continue
                    if pc in bp.pcs:
                        hits.append(bp)
        return hits

    def resolve_pending(
        self, file_id: int, line: int, pcs: Sequence[int]
    ) -> list[Breakpoint]:
        """Attach pcs to breakpoints set before the relevant contract was loaded."""
        resolved: list[Breakpoint] = []
        with self._lock:
            for bp in self.breakpoints.values():
                if bp.pending and bp.file_id == file_id and bp.line == line:
                    bp.pcs.update(pcs)
                    bp.pending = not bp.pcs
                    resolved.append(bp)
        return resolved

    def active_watchpoints(self) -> list[Watchpoint]:
        with self._lock:
            return [w for w in self.watchpoints.values() if w.enabled]
