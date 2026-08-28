"""Per-code-object lookups, cached by code identity.

A contract's code is resolved once, not once per opcode: which artifact it came from, its
source map, its disassembly, and the pc -> declaration table that makes local-variable
observation affordable (the hook does one dict lookup per instruction instead of resolving
a source location).

The cache key is a prefix of the code plus its length, which is enough to tell deployed
contracts apart, and creation code is keyed separately because it has its own source map.
"""

from __future__ import annotations

from typing import Any

from ..compile import Artifact, Project
from ..disasm import Disassembly
from ..locals import LocalsIndex, LocalVar, declaration_pcs
from ..srcmap import LineIndex, PcMap


def _key(code: bytes, is_create: bool) -> bytes:
    return bytes(code[:64]) + len(code).to_bytes(4, "big") + (b"C" if is_create else b"R")


class CodeIndex:
    """Resolves running bytecode back to the source it was compiled from."""

    def __init__(
        self,
        project: Project,
        line_indexes: dict[int, LineIndex],
        locals_index: LocalsIndex,
        breakpoints: Any,
    ) -> None:
        self.project = project
        self.line_indexes = line_indexes
        self.locals_index = locals_index
        self.breakpoints = breakpoints
        self._artifact_cache: dict[bytes, Artifact | None] = {}
        self._pcmap_cache: dict[bytes, PcMap | None] = {}
        self._declpc_cache: dict[bytes, dict[int, LocalVar]] = {}
        self._disasm_cache: dict[bytes, Disassembly] = {}

    def file_id_for(self, source_key: str) -> int | None:
        """Accepts 'Bank.sol' or a suffix of the path, as gdb accepts a basename."""
        src = self.project.sources.get(source_key)
        if src is not None:
            return src.file_id
        for key, candidate in self.project.sources.items():
            if key.endswith(source_key) or candidate.abs_path.endswith(source_key):
                return candidate.file_id
        return None

    def artifact_for(self, code: bytes, is_create: bool) -> Artifact | None:
        key = _key(code, is_create)
        if key in self._artifact_cache:
            return self._artifact_cache[key]
        art: Artifact | None = None
        if is_create:
            # Creation code is `constructor bytecode + abi-encoded args`, so match on prefix.
            for candidate in self.project.artifacts.values():
                if candidate.bytecode and code.startswith(candidate.bytecode):
                    art = candidate
                    break
        else:
            art = self.project.artifact_for_code(code)
        self._artifact_cache[key] = art
        return art

    def pcmap_for(
        self, code: bytes, artifact: Artifact | None, is_create: bool
    ) -> PcMap | None:
        if artifact is None:
            return None
        key = _key(code, is_create)
        if key in self._pcmap_cache:
            return self._pcmap_cache[key]
        source_map = artifact.source_map if is_create else artifact.deployed_source_map
        pcmap = PcMap(code, source_map, self.line_indexes) if source_map else None
        self._pcmap_cache[key] = pcmap
        if pcmap is not None:
            self._resolve_pending(pcmap)
        return pcmap

    def declpcs_for(
        self, code: bytes, pcmap: PcMap | None, is_create: bool
    ) -> dict[int, LocalVar]:
        """pc -> declaration AST id for this code object, built once and shared.

        This is the table that makes local-variable observation affordable: the hook
        does one dict lookup per opcode instead of resolving a source location.
        """
        if pcmap is None:
            return {}
        key = _key(code, is_create)
        cached = self._declpc_cache.get(key)
        if cached is None:
            cached = declaration_pcs(pcmap, self.locals_index)
            self._declpc_cache[key] = cached
        return cached

    def disassembly_for(self, code: bytes) -> Disassembly:
        key = _key(code, is_create=False)
        cached = self._disasm_cache.get(key)
        if cached is None:
            cached = Disassembly(code)
            self._disasm_cache[key] = cached
        return cached

    def _resolve_pending(self, pcmap: PcMap) -> None:
        for bp in list(self.breakpoints.breakpoints.values()):
            if bp.pending and bp.file_id >= 0 and bp.line > 0:
                pcs = pcmap.pcs_for_line(bp.file_id, bp.line)
                if pcs:
                    self.breakpoints.resolve_pending(bp.file_id, bp.line, [min(pcs)])

    def resolve_line(self, file_id: int, line: int) -> tuple[int, list[int]]:
        """Snap a source line to the nearest line with code and return its pcs.

        Searches every artifact, because a `break Foo.sol:12` should work before the
        contract in question has been deployed.
        """
        maps = []
        for art in self.project.artifacts.values():
            if not art.deployed_bytecode or not art.deployed_source_map:
                continue
            maps.append(
                PcMap(art.deployed_bytecode, art.deployed_source_map, self.line_indexes)
            )

        # Pick the snapped line FIRST, across every artifact, then collect pcs only for
        # that line. Doing it per-artifact would mix pcs from different lines: a file's
        # second contract snaps line 48 forward to its own first executable line, and
        # those pcs would silently join the breakpoint.
        candidates = [
            snapped
            for snapped in (
                pcmap.nearest_executable_line(file_id, line) for pcmap in maps
            )
            if snapped is not None
        ]
        if not candidates:
            return line, []
        best_line = min(candidates)
        pcs: list[int] = []
        for pcmap in maps:
            found = pcmap.pcs_for_line(file_id, best_line)
            if found:
                pcs.append(min(found))
        return best_line, sorted(set(pcs))
