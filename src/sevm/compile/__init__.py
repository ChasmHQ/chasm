"""Solidity compilation and the artifact model.

solcx only, by decision: everything downstream depends only on the `Artifact` dataclass,
so a Foundry `out/*.json` importer could be added later by producing the same dataclass.

Where things live:

  model.py            `SourceFile`, `Artifact`, `Project`, and bytecode identity
  solc.py             calling solc, and reading .sol files in
  versions.py         picking the solc version from the sources' pragmas
  foundry_config.py   foundry.toml, remappings, project sources, dependencies
  build.py            `compile_project` / `compile_foundry_project`
"""

from __future__ import annotations

from .build import compile_foundry_project, compile_project
from .foundry_config import (
    DEFAULT_FOUNDRY_TOML,
    FORGE_STD,
    FORGE_STD_SAMPLE_IMPORT,
    STANDALONE_FOUNDRY_TOML,
    FoundryConfig,
    find_foundry_root,
    read_foundry_config,
    resolve_dependencies,
    unresolved_prefixes,
)
from .model import Artifact, CompileError, Project, SourceFile
from .solc import DEFAULT_SOLC_VERSION, compile_standard, ensure_solc
from .versions import resolve_solc_version

__all__ = [
    "DEFAULT_FOUNDRY_TOML",
    "DEFAULT_SOLC_VERSION",
    "FORGE_STD",
    "FORGE_STD_SAMPLE_IMPORT",
    "STANDALONE_FOUNDRY_TOML",
    "Artifact",
    "CompileError",
    "FoundryConfig",
    "Project",
    "SourceFile",
    "compile_foundry_project",
    "compile_project",
    "compile_standard",
    "ensure_solc",
    "find_foundry_root",
    "read_foundry_config",
    "resolve_dependencies",
    "resolve_solc_version",
    "unresolved_prefixes",
]
