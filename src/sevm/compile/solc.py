"""Calling solc, and reading .sol files in.

Debug builds compile with optimizer OFF and via-IR OFF, as a requirement not a default:
optimized codegen fuses and reorders instructions, degrading the source map and breaking
the stack-slot heuristic local-variable support depends on.
"""

from __future__ import annotations

import os
from collections.abc import Sequence
from typing import Any

import solcx

from . import solcbin
from .model import CompileError, SourceFile

# Used when a source carries no `pragma solidity` at all (rare). When a pragma is present,
# the version is auto-detected from it (Foundry-style), so this is only the last resort.
DEFAULT_SOLC_VERSION = "0.8.28"

# Everything the debugger needs, requested in one pass.
_OUTPUT_SELECTION = {
    "*": {
        "*": [
            "abi",
            "evm.bytecode.object",
            "evm.bytecode.sourceMap",
            "evm.deployedBytecode.object",
            "evm.deployedBytecode.sourceMap",
            "evm.deployedBytecode.immutableReferences",
            "evm.methodIdentifiers",
            "storageLayout",
        ],
        "": ["ast"],
    }
}


def _contract_ranges(ast: dict) -> dict[str, tuple[int, int]]:
    """Contract name -> (start, end) byte offsets, from a source unit's AST."""
    ranges: dict[str, tuple[int, int]] = {}

    def walk(node: Any) -> None:
        if isinstance(node, list):
            for item in node:
                walk(item)
            return
        if not isinstance(node, dict):
            return
        if node.get("nodeType") in ("ContractDefinition", "LibraryDefinition"):
            parts = (node.get("src") or "0:0:0").split(":")
            start, length = int(parts[0]), int(parts[1])
            ranges[node.get("name", "")] = (start, start + length)
        for value in node.values():
            if isinstance(value, (dict, list)):
                walk(value)

    walk(ast)
    return ranges


def _read_sol(abs_path: str, key: str) -> SourceFile:
    with open(abs_path, encoding="utf-8") as f:
        return SourceFile(key=key, abs_path=abs_path, text=f.read())


def _read_sources(paths: Sequence[str]) -> dict[str, SourceFile]:
    """Expand paths (files or directories) into solc source entries keyed by basename."""
    found: dict[str, SourceFile] = {}
    for path in paths:
        abs_path = os.path.abspath(path)
        if os.path.isdir(abs_path):
            for root, _dirs, files in os.walk(abs_path):
                for fname in sorted(files):
                    if fname.endswith(".sol"):
                        full = os.path.join(root, fname)
                        key = os.path.relpath(full, abs_path)
                        found[key] = _read_sol(full, key)
        elif os.path.isfile(abs_path):
            key = os.path.basename(abs_path)
            found[key] = _read_sol(abs_path, key)
        else:
            raise CompileError(f"no such source path: {path}")
    if not found:
        raise CompileError(f"no .sol files found under: {', '.join(paths)}")
    return found


def compile_standard(
    sources: dict[str, str],
    solc_version: str = DEFAULT_SOLC_VERSION,
    optimize: bool = False,
    evm_version: str | None = None,
    output_selection: dict | None = None,
    remappings: Sequence[str] | None = None,
) -> dict:
    """Thin solc standard-JSON wrapper. Raises CompileError with parsed diagnostics."""
    settings: dict[str, Any] = {
        "optimizer": {"enabled": bool(optimize)},
        "outputSelection": output_selection or _OUTPUT_SELECTION,
    }
    if remappings:
        settings["remappings"] = list(remappings)
    if evm_version:
        settings["evmVersion"] = evm_version
    payload = {
        "language": "Solidity",
        "sources": {k: {"content": v} for k, v in sources.items()},
        "settings": settings,
    }
    try:
        return solcx.compile_standard(payload, solc_binary=solcbin.ensure(solc_version))
    except solcx.exceptions.SolcError as exc:
        raise CompileError(str(exc)) from exc


def ensure_solc(version: str = DEFAULT_SOLC_VERSION) -> str:
    """Path to a solc `version` that runs here, downloading it if the machine lacks one.

    Provisioning is `solcbin`'s, not py-solc-x's, which fetches x86-64 whatever the
    machine is; solcx is still what runs the binary.
    """
    return solcbin.ensure(version)
