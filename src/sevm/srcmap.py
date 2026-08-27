"""Source maps: turn a program counter into a Solidity line.

Three pieces stacked on top of each other:

  1. `parse_source_map`   solc's ";"-separated, field-eliding format -> one entry per
                          *instruction* (not per byte).
  2. `instruction_pcs`    PUSH-aware walk of the bytecode giving pc <-> instruction index.
                          A PUSH's immediate bytes are not instructions, so a naive
                          byte-indexed map is wrong for every contract.
  3. `PcMap`              joins the two, plus a line index over the source text.

The `jump` field ('i' into, 'o' out of, '-' neither) is the part that matters most.
Solidity compiles *internal* function calls to JUMP, not CALL, so `message.depth` never
changes across them. Without the jump markers, `step` and `next` cannot tell an internal
call from ordinary control flow.
"""

from __future__ import annotations

import bisect
from collections.abc import Iterable
from dataclasses import dataclass

PUSH0 = 0x5F
PUSH1 = 0x60
PUSH32 = 0x7F

JUMP_IN = "i"
JUMP_OUT = "o"
JUMP_NONE = "-"


@dataclass(frozen=True)
class SrcMapEntry:
    """One instruction's source attribution."""

    start: int  # byte offset into the source file
    length: int  # byte length of the range
    file_id: int  # solc source index; -1 means compiler-generated
    jump: str  # 'i' | 'o' | '-'
    modifier_depth: int

    @property
    def is_generated(self) -> bool:
        """True when solc attributed this instruction to no real source file."""
        return self.file_id < 0


def parse_source_map(source_map: str) -> list[SrcMapEntry]:
    """Decode solc's compressed source map into one entry per instruction.

    The format elides repeated fields: an empty field means "same as previous entry",
    and a truncated entry means every remaining field repeats.
    """
    if not source_map:
        return []
    entries: list[SrcMapEntry] = []
    prev = [0, 0, -1, JUMP_NONE, 0]
    for chunk in source_map.split(";"):
        cur = list(prev)
        if chunk:
            for i, part in enumerate(chunk.split(":")[:5]):
                if part == "":
                    continue
                if i == 3:
                    cur[i] = part
                    continue
                try:
                    cur[i] = int(part)
                except ValueError:
                    # A field we cannot parse keeps its inherited value rather than
                    # killing the whole map: one bad entry must not blind the debugger.
                    continue
        entries.append(
            SrcMapEntry(
                start=int(cur[0]),
                length=int(cur[1]),
                file_id=int(cur[2]),
                jump=str(cur[3]),
                modifier_depth=int(cur[4]),
            )
        )
        prev = cur
    return entries


def instruction_pcs(code: bytes) -> list[int]:
    """Program counters of real instructions, in order, skipping PUSH immediates."""
    pcs: list[int] = []
    pc = 0
    n = len(code)
    while pc < n:
        pcs.append(pc)
        op = code[pc]
        # PUSH0 (0x5f) has no immediate; PUSH1..PUSH32 carry op-0x5f bytes.
        pc += 1 + (op - PUSH0 if PUSH1 <= op <= PUSH32 else 0)
    return pcs


class LineIndex:
    """Byte offset <-> (line, column) over one source file. Lines are 1-based."""

    def __init__(self, text: str) -> None:
        self.text = text
        self.lines = text.split("\n")
        self._offsets: list[int] = [0]
        cursor = 0
        for line in self.lines:
            cursor += len(line) + 1
            self._offsets.append(cursor)

    def line_col(self, offset: int) -> tuple[int, int]:
        idx = bisect.bisect_right(self._offsets, offset) - 1
        idx = max(0, min(idx, len(self.lines) - 1))
        return idx + 1, offset - self._offsets[idx] + 1

    def line_text(self, line: int) -> str:
        if 1 <= line <= len(self.lines):
            return self.lines[line - 1]
        return ""

    def line_range(self, start: int, length: int) -> tuple[int, int]:
        """First and last line touched by a source range."""
        first, _ = self.line_col(start)
        last, _ = self.line_col(max(start, start + max(0, length - 1)))
        return first, last

    def offset_of_line(self, line: int) -> int:
        idx = max(0, min(line - 1, len(self._offsets) - 1))
        return self._offsets[idx]

    @property
    def line_count(self) -> int:
        return len(self.lines)


@dataclass(frozen=True)
class Location:
    """Where a program counter sits in Solidity source."""

    pc: int
    entry: SrcMapEntry
    file_id: int
    line: int
    col: int
    end_line: int

    @property
    def jump(self) -> str:
        return self.entry.jump

    @property
    def modifier_depth(self) -> int:
        return self.entry.modifier_depth

    @property
    def is_generated(self) -> bool:
        return self.entry.is_generated

    def key(self) -> tuple[int, int]:
        """Identity used by source-line stepping: which file, which line."""
        return (self.file_id, self.line)


class PcMap:
    """pc -> Location for a single code object (creation or runtime)."""

    def __init__(
        self,
        code: bytes,
        source_map: str,
        line_indexes: dict[int, LineIndex] | None = None,
    ) -> None:
        self.code = code
        self.entries = parse_source_map(source_map)
        self.pcs = instruction_pcs(code)
        self.pc_to_index: dict[int, int] = {pc: i for i, pc in enumerate(self.pcs)}
        self._line_indexes = line_indexes or {}
        self._locations: dict[int, Location] = {}
        self._line_to_pcs: dict[tuple[int, int], list[int]] | None = None

    # -- core lookup --------------------------------------------------------

    def entry_at(self, pc: int) -> SrcMapEntry | None:
        idx = self.pc_to_index.get(pc)
        if idx is None or idx >= len(self.entries):
            return None
        return self.entries[idx]

    def at(self, pc: int) -> Location | None:
        """Resolve a pc to a source location, or None if unmapped."""
        cached = self._locations.get(pc)
        if cached is not None:
            return cached
        entry = self.entry_at(pc)
        if entry is None:
            return None
        index = self._line_indexes.get(entry.file_id)
        if index is None:
            loc = Location(
                pc=pc, entry=entry, file_id=entry.file_id, line=0, col=0, end_line=0
            )
        else:
            line, col = index.line_col(entry.start)
            _, end_line = index.line_range(entry.start, entry.length)
            loc = Location(
                pc=pc,
                entry=entry,
                file_id=entry.file_id,
                line=line,
                col=col,
                end_line=end_line,
            )
        self._locations[pc] = loc
        return loc

    # -- reverse lookup, for setting breakpoints ----------------------------

    def _build_line_table(self) -> dict[tuple[int, int], list[int]]:
        table: dict[tuple[int, int], list[int]] = {}
        for pc in self.pcs:
            loc = self.at(pc)
            if loc is None or loc.is_generated or loc.line == 0:
                continue
            table.setdefault(loc.key(), []).append(pc)
        return table

    def pcs_for_line(self, file_id: int, line: int) -> list[int]:
        if self._line_to_pcs is None:
            self._line_to_pcs = self._build_line_table()
        return self._line_to_pcs.get((file_id, line), [])

    def first_pc_for_line(self, file_id: int, line: int) -> int | None:
        """The pc a `break FILE:LINE` should attach to.

        Statements can compile to instructions scattered across the code (a loop body's
        condition check, for one), so the lowest pc is the entry the user means.
        """
        pcs = self.pcs_for_line(file_id, line)
        return min(pcs) if pcs else None

    def executable_lines(self, file_id: int) -> list[int]:
        """Lines that have at least one instruction. Drives breakpoint gutter marks."""
        if self._line_to_pcs is None:
            self._line_to_pcs = self._build_line_table()
        return sorted({line for (fid, line) in self._line_to_pcs if fid == file_id})

    def nearest_executable_line(self, file_id: int, line: int) -> int | None:
        """Snap a requested breakpoint line down to the next line that has code."""
        lines = self.executable_lines(file_id)
        if not lines:
            return None
        idx = bisect.bisect_left(lines, line)
        return lines[idx] if idx < len(lines) else None


def build_line_indexes(sources: Iterable) -> dict[int, LineIndex]:
    """Map solc file_id -> LineIndex for every source in a Project."""
    return {
        src.file_id: LineIndex(src.text)
        for src in sources
        if getattr(src, "file_id", -1) >= 0
    }
