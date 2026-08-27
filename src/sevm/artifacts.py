"""Forge-shaped artifacts on disk, so a build leaves something readable behind.

The layout and field names are forge's own (`<File.sol>/<Contract>.json`), nested one level
under the project's `out` directory. That nesting is deliberate: sevm compiles with the
optimizer off, forge's cache would still call an overwritten artifact fresh, and the next
`forge test` would silently run sevm's build. `metadata`/`rawMetadata` are the one thing
forge writes and this does not; sevm never asks solc for them.
"""

from __future__ import annotations

import json
import os
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .compile import Artifact, Project

SUBDIR = "sevm"


def out_dir(root: str, out: str = "out") -> str:
    return os.path.join(root, out, SUBDIR)


def write_out(project: Project, root: str, out: str = "out") -> int:
    """Write one JSON per compiled contract; returns how many. No-op outside a project.

    A directory with no foundry.toml is never written into, the same rule the build cache
    follows.
    """
    if not os.path.isfile(os.path.join(root, "foundry.toml")):
        return 0
    target = out_dir(root, out)
    seen: set[str] = set()
    for art in sorted(project.artifacts.values(), key=lambda a: a.qualified_name):
        directory = os.path.join(target, os.path.basename(art.source_key))
        path = os.path.join(directory, f"{art.name}.json")
        # Sources sharing a basename can declare the same contract name (a vendored copy
        # of a library, say); keep both, and `sourceName` says which is which.
        suffix = 1
        while path in seen:
            path = os.path.join(directory, f"{art.name}.{suffix}.json")
            suffix += 1
        seen.add(path)
        os.makedirs(directory, exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(_document(art, project), fh, indent=2)
    return len(seen)


def _document(art: Artifact, project: Project) -> dict:
    source = project.sources.get(art.source_key)
    return {
        "contractName": art.name,
        "sourceName": art.source_key,
        "abi": art.abi,
        "bytecode": {
            "object": _hex(art.bytecode),
            "sourceMap": art.source_map,
            "linkReferences": {},
        },
        "deployedBytecode": {
            "object": _hex(art.deployed_bytecode),
            "sourceMap": art.deployed_source_map,
            "linkReferences": {},
            "immutableReferences": art.immutable_references,
        },
        "methodIdentifiers": art.method_identifiers,
        "storageLayout": art.storage_layout,
        "id": source.file_id if source is not None else -1,
    }


def _hex(code: bytes) -> str:
    return "0x" + code.hex()
