"""The artifact model: what a compiled project looks like to the rest of sevm.

Everything downstream depends only on these dataclasses, so a Foundry `out/*.json`
importer could be added later by producing the same shapes.
"""

from __future__ import annotations

import hashlib
import os
from dataclasses import dataclass, field


class CompileError(RuntimeError):
    """solc rejected the input. Carries the raw diagnostics."""

    def __init__(self, message: str, diagnostics: list[dict] | None = None) -> None:
        super().__init__(message)
        self.diagnostics = diagnostics or []


@dataclass(frozen=True)
class SourceFile:
    """One .sol file as solc sees it."""

    key: str  # the identifier used inside solc input/output, e.g. "Vault.sol"
    abs_path: str
    text: str
    file_id: int = -1  # solc's numeric source index, used by source maps

    @property
    def name(self) -> str:
        return os.path.basename(self.abs_path)


@dataclass
class Artifact:
    """One compiled contract, with everything the debugger maps against."""

    name: str
    source_key: str
    abi: list[dict]
    bytecode: bytes  # creation code
    deployed_bytecode: bytes  # runtime code
    source_map: str  # for `bytecode` (constructor debugging)
    deployed_source_map: str  # for `deployed_bytecode` (the common case)
    storage_layout: dict
    method_identifiers: dict[str, str]
    immutable_references: dict[str, list[dict]] = field(default_factory=dict)
    # Byte range of the `contract X { ... }` declaration. The eval injector needs this to
    # splice into the right contract's closing brace when a file holds several.
    source_range: tuple[int, int] = (-1, -1)

    @property
    def qualified_name(self) -> str:
        return f"{self.source_key}:{self.name}"

    @property
    def selectors(self) -> dict[bytes, str]:
        """4-byte selector -> full signature, e.g. b'\\x26\\x78\\x45\\x90' -> 'unsafeStore(uint256,uint256)'."""
        return {bytes.fromhex(sel): sig for sig, sel in self.method_identifiers.items()}


@dataclass
class Project:
    """A compiled set of sources plus the lookups the debugger performs at runtime."""

    sources: dict[str, SourceFile]  # by solc source key
    artifacts: dict[str, Artifact]  # by qualified name
    asts: dict[str, dict]  # by solc source key
    solc_version: str
    optimized: bool = False
    # solc import remappings (Foundry projects); needed to recompile for evaluation.
    remappings: list[str] = field(default_factory=list)

    _code_index: dict[bytes, str] = field(default_factory=dict, repr=False)

    def __post_init__(self) -> None:
        for art in self.artifacts.values():
            if art.deployed_bytecode:
                self._code_index[_code_fingerprint(art.deployed_bytecode, art)] = (
                    art.qualified_name
                )

    # -- lookups ------------------------------------------------------------

    def source_by_id(self, file_id: int) -> SourceFile | None:
        for src in self.sources.values():
            if src.file_id == file_id:
                return src
        return None

    def artifact_for_code(self, runtime_code: bytes) -> Artifact | None:
        """Identify a deployed contract from the code sitting at its address.

        Matches on the bytecode with the trailing CBOR metadata stripped and any
        immutable slots zeroed, so a contract with immutables still resolves.
        """
        if not runtime_code:
            return None
        exact = self._code_index.get(_code_fingerprint(runtime_code, None))
        if exact is not None:
            return self.artifacts[exact]
        # Retry per-artifact so each one's immutable offsets can be masked.
        for art in self.artifacts.values():
            if not art.deployed_bytecode:
                continue
            if len(_strip_metadata(runtime_code)) != len(
                _strip_metadata(art.deployed_bytecode)
            ):
                continue
            if _code_fingerprint(runtime_code, art) == _code_fingerprint(
                art.deployed_bytecode, art
            ):
                return art
        return None

    def artifact(self, name: str) -> Artifact | None:
        """Look up by contract name or by qualified 'File.sol:Name'."""
        if name in self.artifacts:
            return self.artifacts[name]
        matches = [a for a in self.artifacts.values() if a.name == name]
        return matches[0] if len(matches) == 1 else None


# -- bytecode identity -------------------------------------------------------


def _strip_metadata(code: bytes) -> bytes:
    """Drop solc's trailing CBOR metadata blob.

    Layout is `<code> <cbor> <uint16 be length of cbor>`. The length is only trusted
    when it lands inside the buffer, so non-solc code passes through untouched.
    """
    if len(code) < 2:
        return code
    meta_len = int.from_bytes(code[-2:], "big")
    total = meta_len + 2
    if 0 < total <= len(code):
        return code[: len(code) - total]
    return code


def _code_fingerprint(code: bytes, artifact: Artifact | None) -> bytes:
    """Metadata-stripped, immutable-masked hash of runtime code."""
    body = bytearray(_strip_metadata(code))
    if artifact is not None:
        for refs in artifact.immutable_references.values():
            for ref in refs:
                start, length = int(ref["start"]), int(ref["length"])
                if start + length <= len(body):
                    body[start : start + length] = b"\x00" * length
    return hashlib.sha256(bytes(body)).digest()
