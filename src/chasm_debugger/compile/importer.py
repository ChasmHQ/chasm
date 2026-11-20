"""Import embedded standard-JSON build input/output without invoking solc."""

from __future__ import annotations

import copy
import json
import re
from pathlib import Path
from typing import Any

from .build import _build_project
from .model import CompileError, Project, SourceFile

_VERSION = re.compile(r"^(\d+\.\d+\.\d+)(?:\+[^\s]+)?$")
_ADDRESS = re.compile(r"^(?:0x)?[0-9a-fA-F]{40}$")


def _linked_object(entry: dict, libraries: dict) -> str:
    obj = entry.get("object", "")
    if not isinstance(obj, str):
        raise CompileError("build-info bytecode object must be a string")
    for source, names in entry.get("linkReferences", {}).items():
        for name, references in names.items():
            address = libraries.get(source, {}).get(name)
            for reference in references:
                start, length = reference["start"], reference["length"]
                if (
                    not isinstance(start, int)
                    or not isinstance(length, int)
                    or start < 0
                    or length != 20
                    or (start + length) * 2 > len(obj)
                ):
                    raise CompileError("invalid build-info library reference")
                segment = obj[start * 2 : (start + length) * 2]
                if "_" not in segment:
                    continue
                if not isinstance(address, str) or not _ADDRESS.fullmatch(address):
                    raise CompileError(f"missing library address for {source}:{name}")
                obj = (
                    obj[: start * 2]
                    + address.removeprefix("0x")
                    + obj[(start + length) * 2 :]
                )
    try:
        bytes.fromhex(obj)
    except ValueError as exc:
        raise CompileError("build-info contains invalid or unlinked bytecode") from exc
    return obj


def project_from_build_info(document: dict[str, Any], *, base: str = ".") -> Project:
    if not isinstance(document, dict):
        raise CompileError("build-info must be a JSON object")
    version = document.get("solcVersion", document.get("solcLongVersion", ""))
    match = _VERSION.fullmatch(version) if isinstance(version, str) else None
    if match is None:
        raise CompileError("build-info must include a solcVersion")
    source_input, output = document.get("input"), document.get("output")
    if not isinstance(source_input, dict) or not isinstance(output, dict):
        raise CompileError("expected build-info with embedded input and output")
    if source_input.get("language") != "Solidity":
        raise CompileError("only Solidity build-info is supported")
    errors = [
        entry for entry in output.get("errors", []) if entry.get("severity") == "error"
    ]
    if errors:
        raise CompileError("build-info contains compiler errors", errors)
    settings = copy.deepcopy(source_input.get("settings", {}))
    if not isinstance(settings, dict):
        raise CompileError("build-info settings must be an object")
    entries = source_input.get("sources", {})
    if not isinstance(entries, dict) or not entries:
        raise CompileError("build-info must embed its source contents")
    sources = {}
    for key, entry in entries.items():
        if not isinstance(key, str) or not isinstance(entry, dict):
            raise CompileError("invalid build-info source entry")
        content = entry.get("content")
        if not isinstance(content, str):
            raise CompileError(f"build-info source lacks embedded content: {key}")
        # Imported source keys are display identifiers, never filesystem reads.
        sources[key] = SourceFile(key, str(Path(base) / key), content)
    built = copy.deepcopy(output)
    libraries = settings.get("libraries", {})
    for contracts in built.get("contracts", {}).values():
        for contract in contracts.values():
            evm = contract.get("evm", {})
            for field in ("bytecode", "deployedBytecode"):
                if field in evm:
                    evm[field]["object"] = _linked_object(evm[field], libraries)
    try:
        project = _build_project(
            built,
            sources,
            match[1],
            bool(settings.get("optimizer", {}).get("enabled", False)),
            remappings=settings.get("remappings", []),
        )
    except (KeyError, TypeError, ValueError) as exc:
        raise CompileError("malformed build-info compiler output") from exc
    if not project.artifacts:
        raise CompileError("build-info contains no contract artifacts")
    project.compiler_settings = settings
    return project


def load_build_info(path: str) -> Project:
    source = Path(path)
    if source.is_dir():
        files = sorted(source.glob("*.json"))
        if len(files) != 1:
            raise CompileError("select one build-info JSON file explicitly")
        source = files[0]
    try:
        document = json.loads(source.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError) as exc:
        raise CompileError("cannot read build-info JSON") from exc
    return project_from_build_info(document, base=str(source.parent))
