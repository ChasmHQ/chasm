"""Solidity compilation and the artifact model.

solcx only, by decision: everything downstream depends only on the `Artifact` dataclass, so
a Foundry `out/*.json` importer could be added later by producing the same dataclass.

Debug builds compile with optimizer OFF and via-IR OFF, as a requirement not a default:
optimized codegen fuses/reorders instructions, degrading the source map and breaking the
stack-slot heuristic local-variable support will need. `compile_project` enforces this.
"""

from __future__ import annotations

import hashlib
import os
import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

import solcx
from packaging.specifiers import SpecifierSet
from packaging.version import Version

from . import artifacts, cache
from .libs import (
    Dependency,
    LibError,
    import_closure,
    install,
    missing_prefixes,
    package_of,
    write_remappings,
)

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


# -- compilation -------------------------------------------------------------


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
        return solcx.compile_standard(payload, solc_version=solc_version)
    except solcx.exceptions.SolcError as exc:
        raise CompileError(str(exc)) from exc


def ensure_solc(version: str = DEFAULT_SOLC_VERSION) -> None:
    """Install the pinned solc if this machine does not have it yet."""
    installed = {str(v) for v in solcx.get_installed_solc_versions()}
    if version not in installed:
        solcx.install_solc(version)


# -- version resolution ------------------------------------------------------
#
# solc rejects a source whose `pragma solidity` does not match the compiler version, so a
# file pinned to 0.8.21 cannot be built with 0.8.28. Foundry (via svm) solves this by
# reading every source's pragma, intersecting the constraints, and picking the highest
# compatible release, installing it on demand. We do the same, reusing solcx's pragma
# comparator semantics (correct solidity caret handling: `^0.8.0` means `>=0.8.0 <0.9.0`).

_PRAGMA_RE = re.compile(r"pragma\s+solidity\s+([^;]+);")
# One comparator token from a pragma, e.g. `^0.8.0`, `>=0.6.2`, `<0.9.0`, `0.8.21`.
_COMPARATOR_RE = re.compile(r"(([<>]?=?|\^)\d+\.\d+\.\d+)")


def _extract_pragmas(sources: dict[str, SourceFile]) -> dict[str, list[str]]:
    """Each `pragma solidity ...` constraint -> the source keys that declare it.

    Insertion-ordered and deduped, so the keys are the distinct constraints to intersect
    and the values name the files behind each (used to explain an unsatisfiable set).
    """
    by_constraint: dict[str, list[str]] = {}
    for key in sorted(sources):
        for match in _PRAGMA_RE.finditer(sources[key].text):
            files = by_constraint.setdefault(match.group(1).strip(), [])
            if key not in files:
                files.append(key)
    return by_constraint


def _comparator_set_spec(comparator_set: str) -> SpecifierSet:
    """Translate one solidity comparator set (space/comma-joined ANDs) into a SpecifierSet.

    Mirrors solcx's own mapping so caret behaves the solidity way: `^0.8.0` -> `~=0.8.0`
    (i.e. `>=0.8.0 <0.9.0`), and a bare `0.8.21` -> `==0.8.21`.
    """

    def as_spec(item: str) -> str:
        ret = item.replace("^", "~=")
        if ret and ret[0].isnumeric():
            return f"=={ret}"
        if len(ret) >= 2 and ret[0] == "=" and ret[1] != "=":
            return f"={ret}"
        return ret

    specs = ",".join(as_spec(tok[0]) for tok in _COMPARATOR_RE.findall(comparator_set))
    return SpecifierSet(specs)


def _pragma_matches(constraint: str, versions: Sequence[Version]) -> set[Version]:
    """Subset of `versions` satisfying one pragma constraint (handles `||` OR groups)."""
    matched: set[Version] = set()
    for comparator_set in constraint.replace(" ", "").split("||"):
        matched.update(_comparator_set_spec(comparator_set).filter(versions))
    return matched


def _select_from(
    constraints: Sequence[str], versions: Sequence[Version]
) -> Version | None:
    """Highest version satisfying every pragma in `constraints`, or None."""
    candidates = set(versions)
    for constraint in constraints:
        candidates &= _pragma_matches(constraint, versions)
        if not candidates:
            return None
    return max(candidates) if candidates else None


def resolve_solc_version(
    sources: dict[str, SourceFile],
    *,
    explicit: str | None = None,
    config_pinned: str | None = None,
) -> str:
    """Pick the solc version to build `sources` with.

    Precedence, matching Foundry: an explicit `--solc` wins, then a foundry.toml `solc`
    pin, then auto-detection from the sources' `pragma solidity` lines (highest compatible
    release, installed on demand), then `DEFAULT_SOLC_VERSION` when no pragma is present.

    Raises CompileError when the pragmas cannot all be satisfied by any known release.
    """
    if explicit:
        return explicit
    if config_pinned:
        return config_pinned

    pragmas = _extract_pragmas(sources)
    if not pragmas:
        return DEFAULT_SOLC_VERSION
    constraints = list(pragmas)

    # Foundry-exact: consider every installable release and pick the highest match, then
    # install it if missing. Fall back to the installed set when offline.
    try:
        installable = [
            Version(v)
            for v in cache.installable_versions(solcx.get_installable_solc_versions)
        ]
    except Exception:
        installable = []
    selected = _select_from(constraints, installable)
    if selected is None:
        installed = solcx.get_installed_solc_versions()
        selected = _select_from(constraints, installed)
    if selected is None:
        raise CompileError(_conflict_message(pragmas))
    return str(selected)


def _conflict_message(pragmas: dict[str, list[str]]) -> str:
    """Explain an unsatisfiable pragma set by naming the files behind each constraint.

    Files import each other into one compilation unit, so a single solc must satisfy them
    all; when none does, the fix is to align the pragmas (or pass --solc).
    """
    lines = ["no solc release satisfies every pragma (they conflict):"]
    for constraint, files in pragmas.items():
        shown = ", ".join(files[:3]) + (
            f" (+{len(files) - 3} more)" if len(files) > 3 else ""
        )
        lines.append(f"  {constraint:<18} {shown}")
    lines.append(
        "Align the pragmas so one version fits (e.g. set the test's pragma to match the "
        "contracts it imports), or force one with --solc."
    )
    return "\n".join(lines)


def compile_project(
    paths: Sequence[str],
    solc_version: str | None = None,
    optimize: bool = False,
    evm_version: str | None = None,
) -> Project:
    """Compile every .sol under `paths` into a debuggable Project.

    With no `solc_version`, the version is auto-detected from the sources' pragmas.
    """
    sources = _read_sources(list(paths))
    resolved = resolve_solc_version(sources, explicit=solc_version)
    ensure_solc(resolved)
    out = compile_standard(
        {k: s.text for k, s in sources.items()},
        solc_version=resolved,
        optimize=optimize,
        evm_version=evm_version,
    )
    return _build_project(out, sources, resolved, optimize)


def _object_bytes(obj: str) -> bytes:
    """Bytecode as bytes, or empty for an object solc left unlinked.

    A contract using an unlinked library keeps `__$<hash>$__` placeholders where the
    library address goes; it has no runnable code until linked, so it gets no bytecode
    rather than crashing the whole project.
    """
    return b"" if "_" in obj else bytes.fromhex(obj)


def _build_project(
    out: dict,
    sources: dict[str, SourceFile],
    solc_version: str,
    optimize: bool,
    remappings: Sequence[str] | None = None,
) -> Project:
    """Assemble a Project from solc standard-JSON output and the source map. Shared by the
    plain and Foundry compile paths."""
    # solc assigns each source a numeric id; source maps reference it, so carry it.
    asts: dict[str, dict] = {}
    for key, entry in out.get("sources", {}).items():
        file_id = int(entry.get("id", -1))
        if key in sources:
            sources[key] = SourceFile(
                key=key,
                abs_path=sources[key].abs_path,
                text=sources[key].text,
                file_id=file_id,
            )
        if "ast" in entry:
            asts[key] = entry["ast"]

    ranges_by_source = {key: _contract_ranges(ast) for key, ast in asts.items()}

    artifacts: dict[str, Artifact] = {}
    for source_key, contracts in out.get("contracts", {}).items():
        for name, data in contracts.items():
            evm = data.get("evm", {})
            art = Artifact(
                name=name,
                source_key=source_key,
                abi=data.get("abi", []),
                bytecode=_object_bytes(evm.get("bytecode", {}).get("object", "")),
                deployed_bytecode=_object_bytes(
                    evm.get("deployedBytecode", {}).get("object", "")
                ),
                source_map=evm.get("bytecode", {}).get("sourceMap", "") or "",
                deployed_source_map=evm.get("deployedBytecode", {}).get("sourceMap", "")
                or "",
                storage_layout=data.get("storageLayout", {}) or {},
                method_identifiers=evm.get("methodIdentifiers", {}) or {},
                immutable_references=evm.get("deployedBytecode", {}).get(
                    "immutableReferences", {}
                )
                or {},
                source_range=ranges_by_source.get(source_key, {}).get(name, (-1, -1)),
            )
            artifacts[art.qualified_name] = art

    return Project(
        sources=sources,
        artifacts=artifacts,
        asts=asts,
        solc_version=solc_version,
        optimized=optimize,
        remappings=list(remappings or []),
    )


# ======================================================================
# Foundry project support
# ======================================================================

# Directories never walked when collecting a project's sources.
_SKIP_DIRS = frozenset({".git", "node_modules", "out", "cache", "broadcast", ".venv"})

# The import every Foundry test needs; installed for a `.sol` target even when the test
# happens not to import it yet, so the directory is a working Foundry project afterwards.
FORGE_STD = "forge-std"
FORGE_STD_SAMPLE_IMPORT = "forge-std/Test.sol"

DEFAULT_FOUNDRY_TOML = """\
[profile.default]
src = "src"
test = "test"
libs = ["lib"]

# sevm forces the optimizer OFF for debuggable builds regardless of this file.
"""

# For a lone .sol outside any project: no src/test layout to declare, just somewhere to
# install libraries.
STANDALONE_FOUNDRY_TOML = """\
[profile.default]
libs = ["lib"]

# sevm forces the optimizer OFF for debuggable builds regardless of this file.
"""


def _load_toml(text: str) -> dict:
    """Parse TOML, preferring the stdlib tomllib (3.11+) with a tiny fallback for 3.10."""
    try:
        import tomllib

        return tomllib.loads(text)
    except ModuleNotFoundError:
        pass
    # Minimal fallback: only the flat/[section] key = value shapes a foundry.toml needs.
    data: dict[str, Any] = {}
    section = data
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            path = line[1:-1].split(".")
            section = data
            for part in path:
                section = section.setdefault(part.strip(), {})  # type: ignore[assignment]
            continue
        if "=" not in line:
            continue
        key, _, val = line.partition("=")
        key = key.strip()
        val = val.strip()
        if val.startswith("[") and val.endswith("]"):
            items = [v.strip().strip("\"'") for v in val[1:-1].split(",") if v.strip()]
            section[key] = items
        elif val.lower() in ("true", "false"):
            section[key] = val.lower() == "true"
        else:
            section[key] = val.strip("\"'")
    return data


def find_foundry_root(start: str) -> str | None:
    """Walk up from `start` (a file or directory) to the nearest dir with a foundry.toml."""
    here = os.path.abspath(start)
    if os.path.isfile(here):
        here = os.path.dirname(here)
    while True:
        if os.path.isfile(os.path.join(here, "foundry.toml")):
            return here
        parent = os.path.dirname(here)
        if parent == here:
            return None
        here = parent


@dataclass
class FoundryConfig:
    root: str
    src: str = "src"
    test: str = "test"
    out: str = "out"
    libs: tuple[str, ...] = ("lib",)
    solc_version: str | None = None
    evm_version: str | None = None
    remappings: list[str] = field(default_factory=list)


def read_foundry_config(root: str) -> FoundryConfig:
    """Read `<root>/foundry.toml` (profile.default) if present, else sensible defaults."""
    cfg = FoundryConfig(root=root)
    toml_path = os.path.join(root, "foundry.toml")
    if os.path.isfile(toml_path):
        with open(toml_path, encoding="utf-8") as fh:
            parsed = _load_toml(fh.read())
        profile = (parsed.get("profile", {}) or {}).get("default", {}) or {}
        cfg.src = profile.get("src", cfg.src)
        cfg.test = profile.get("test", cfg.test)
        cfg.out = profile.get("out", cfg.out)
        libs = profile.get("libs", list(cfg.libs))
        cfg.libs = tuple(libs) if isinstance(libs, list) else (libs,)
        cfg.solc_version = profile.get("solc") or profile.get("solc_version")
        cfg.evm_version = profile.get("evm_version")
        rms = profile.get("remappings", [])
        cfg.remappings = list(rms) if isinstance(rms, list) else [rms]
    # A standalone remappings.txt overlays / supplements foundry.toml remappings.
    rm_txt = os.path.join(root, "remappings.txt")
    if os.path.isfile(rm_txt):
        with open(rm_txt, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    cfg.remappings.append(line)
    return cfg


def _autodetect_lib_remappings(root: str, libs: Sequence[str]) -> list[str]:
    """Foundry's implicit `<name>/=lib/<name>/src/` remappings for each installed lib."""
    out: list[str] = []
    for lib in libs:
        lib_dir = os.path.join(root, lib)
        if not os.path.isdir(lib_dir):
            continue
        for name in sorted(os.listdir(lib_dir)):
            dep = os.path.join(lib_dir, name)
            if not os.path.isdir(dep):
                continue
            src_sub = os.path.join(dep, "src")
            rel = f"{lib}/{name}/src/" if os.path.isdir(src_sub) else f"{lib}/{name}/"
            out.append(f"{name}/={rel}")
    return out


def _collect_project_sources(
    root: str,
    extra_files: Sequence[str] = (),
    source_dirs: Sequence[str] | None = None,
    libs: Sequence[str] = ("lib",),
) -> dict[str, SourceFile]:
    """The project's own .sol files, keyed by path relative to root.

    Library sources are deliberately excluded here and enter through the import closure
    instead: a library's `test/`/`script/` trees import the same paths its sources do, and
    forge-std's tests carry unlinked library placeholders that have no debuggable artifact.
    """
    skip = set(_SKIP_DIRS) | {os.path.basename(lib.rstrip("/")) for lib in libs}
    found: dict[str, SourceFile] = {}
    for base in source_dirs or [root]:
        for cur, dirs, files in os.walk(base):
            dirs[:] = [d for d in dirs if d not in skip]
            for fname in sorted(files):
                if fname.endswith(".sol"):
                    full = os.path.join(cur, fname)
                    key = os.path.relpath(full, root).replace(os.sep, "/")
                    found[key] = _read_sol(full, key)
    for extra in extra_files:
        full = os.path.abspath(extra)
        key = os.path.relpath(full, root).replace(os.sep, "/")
        if key not in found and os.path.isfile(full):
            found[key] = _read_sol(full, key)
    return found


def _install_hint(import_path: str, importer: str) -> str:
    """Said when installs are off, whether by --no-install or by a declined prompt."""
    return (
        f"unresolved import {import_path!r} in {importer}, and sevm was told not to "
        f"install it. Run again with -y to let it, or install it yourself:\n"
        f"  forge install <org>/<repo>\n"
        f"  echo '{package_of(import_path)}/=lib/<repo>/src/' >> remappings.txt"
    )


def resolve_dependencies(
    root: str,
    sources: dict[str, SourceFile],
    cfg: FoundryConfig,
    *,
    install_missing: bool = True,
    ensure: Sequence[tuple[str, str]] = (),
    on_notice: Callable[[str], None] | None = None,
) -> tuple[dict[str, SourceFile], list[str], list[Dependency], dict[str, list[str]]]:
    """Walk the imports, install what is missing, and return (extra sources, remappings,
    installed, import edges).

    `ensure` names (prefix, sample import) pairs to install even when nothing imports them
    yet, which is how a `.sol` target always ends up with a real forge-std.
    """
    remappings = _autodetect_lib_remappings(root, cfg.libs) + list(cfg.remappings)
    libs_dir = cfg.libs[0] if cfg.libs else "lib"
    installed: list[Dependency] = []

    def add(prefix: str, sample: str) -> None:
        dep = install(prefix, sample, root, libs_dir, on_notice=on_notice)
        # A derived remapping replaces the autodetected `<name>/=lib/<name>/src/` guess:
        # it is built from where the imported file actually is.
        remappings[:] = [r for r in remappings if r.split("=")[0] != f"{prefix}/"]
        remappings.append(dep.remapping)
        installed.append(dep)

    try:
        for prefix, sample in ensure:
            if install_missing and not _already_resolved(prefix, root, remappings):
                add(prefix, sample)

        texts = {k: s.text for k, s in sources.items()}
        # Each install can pull in imports of its own, so re-walk until it settles.
        for _ in range(9):
            closure = import_closure(texts, root, remappings)
            if not closure.unresolved:
                break
            for path, importer in closure.unresolved.items():
                if path.startswith("."):
                    raise CompileError(f"no such import {path!r} from {importer}")
            if not install_missing:
                path, importer = next(iter(closure.unresolved.items()))
                raise CompileError(_install_hint(path, importer))
            for prefix in missing_prefixes(closure.unresolved):
                sample = next(p for p in closure.unresolved if package_of(p) == prefix)
                add(prefix, sample)
        else:
            path, importer = next(iter(closure.unresolved.items()))
            raise CompileError(
                f"{path!r} in {importer} is still unresolved after installing "
                f"{len(installed)} libraries; add a remapping for it by hand"
            )
    except LibError as exc:
        raise CompileError(str(exc)) from exc

    extra = {
        key: SourceFile(key=key, abs_path=os.path.join(root, key), text=text)
        for key, text in closure.extra.items()
    }
    return extra, remappings, installed, closure.edges


def _already_resolved(prefix: str, root: str, remappings: Sequence[str]) -> bool:
    """True when `prefix` already maps to something on disk."""
    for entry in remappings:
        left, _, target = entry.partition("=")
        if left.rstrip("/") == prefix and os.path.isdir(os.path.join(root, target)):
            return True
    return False


def unresolved_prefixes(
    root: str,
    source_dirs: Sequence[str] | None = None,
    cfg: FoundryConfig | None = None,
) -> list[str]:
    """Library prefixes the sources import but cannot reach on disk.

    Reads files only, so `sevm run` can say what it would install before compiling.
    """
    cfg = cfg or read_foundry_config(root)
    sources = _collect_project_sources(root, (), source_dirs, cfg.libs)
    remappings = _autodetect_lib_remappings(root, cfg.libs) + list(cfg.remappings)
    closure = import_closure({k: s.text for k, s in sources.items()}, root, remappings)
    return missing_prefixes(p for p in closure.unresolved if not p.startswith("."))


def compile_foundry_project(
    root: str,
    *,
    target_file: str | None = None,
    source_dirs: Sequence[str] | None = None,
    solc_version: str | None = None,
    evm_version: str | None = None,
    optimize: bool = False,
    install_missing: bool = True,
    ensure_forge_std: bool = False,
    on_notice: Callable[[str], None] | None = None,
    use_cache: bool = True,
    force: bool = False,
) -> Project:
    """Compile a Foundry project (or a standalone directory) into a debuggable Project.

    Remappings come from foundry.toml + remappings.txt + the implicit `lib/` ones; anything
    still unresolved is installed from its real repository and remapped. New remappings are
    written to `<root>/remappings.txt` so `forge` resolves the project the same way.

    Results are cached per compilation unit (see `cache.py`) and written out as forge-shaped
    artifacts (see `artifacts.py`); `force` recompiles and rewrites the entry,
    `use_cache=False` does neither.
    """
    cfg = read_foundry_config(root)
    extra_files = [target_file] if target_file else []
    sources = _collect_project_sources(root, extra_files, source_dirs, cfg.libs)

    ensure = [(FORGE_STD, FORGE_STD_SAMPLE_IMPORT)] if ensure_forge_std else []
    lib_sources, remappings, installed, edges = resolve_dependencies(
        root,
        sources,
        cfg,
        install_missing=install_missing,
        ensure=ensure,
        on_notice=on_notice,
    )
    sources.update(lib_sources)

    if installed:
        added = write_remappings(root, [d.remapping for d in installed])
        if added and on_notice:
            on_notice(f"wrote {len(added)} remapping(s) to remappings.txt")

    # `--solc` wins, then a foundry.toml pin, else auto-detect from the pragmas. Every
    # source, libraries included, is collected by now, so the intersection is complete.
    solc = resolve_solc_version(
        sources, explicit=solc_version, config_pinned=cfg.solc_version
    )
    ensure_solc(solc)

    texts = {k: s.text for k, s in sources.items()}
    store = cache.open_cache(root, use_cache)
    settings = cache.settings_hash(
        solc, optimize, evm_version or cfg.evm_version, remappings, _OUTPUT_SELECTION
    )
    hashes = cache.hash_sources(texts)
    unit = cache.unit_hash(settings, hashes)

    hit = None if store is None or force else store.load(unit)
    if hit is not None:
        out = hit
        if on_notice:
            on_notice(f"cache hit ({len(texts)} sources)")
    else:
        out = _compile_unit(
            texts,
            solc=solc,
            optimize=optimize,
            evm_version=evm_version or cfg.evm_version,
            remappings=remappings,
            base=None if store is None or force else store.base_for(settings, set(texts)),
            hashes=hashes,
            edges=edges,
            on_notice=on_notice,
        )
        if store is not None:
            store.store(unit, out, settings, hashes)

    project = _build_project(out, sources, solc, optimize, remappings=remappings)
    # A hit means the artifacts are already there from the build that filled the cache,
    # unless they have since been removed.
    if store is not None and (
        hit is None or not os.path.isdir(artifacts.out_dir(root, cfg.out))
    ):
        _write_artifacts(project, root, cfg.out, on_notice)
    return project


def _write_artifacts(
    project: Project, root: str, out: str, on_notice: Callable[[str], None] | None
) -> None:
    """Leave forge-shaped artifacts behind. Never worth failing a debug session over."""
    try:
        count = artifacts.write_out(project, root, out)
    except OSError:
        return
    if count and on_notice:
        on_notice(f"wrote {count} artifact(s) to {out}/{artifacts.SUBDIR}")


def _cacheable(out: dict) -> dict:
    """The parts of solc's output the cache and `_build_project` need."""
    return {"sources": out.get("sources", {}), "contracts": out.get("contracts", {})}


def _partial_selection(dirty: Sequence[str]) -> dict:
    """Ask solc for output on the dirty sources only; everything else is reused."""
    return {key: dict(_OUTPUT_SELECTION["*"]) for key in sorted(dirty)}


def _compile_unit(
    texts: dict[str, str],
    *,
    solc: str,
    optimize: bool,
    evm_version: str | None,
    remappings: Sequence[str],
    base: tuple[dict, dict[str, str]] | None,
    hashes: dict[str, str],
    edges: dict[str, list[str]],
    on_notice: Callable[[str], None] | None,
) -> dict:
    """Run solc for this unit, narrowing the request to what changed when possible."""
    if base is not None:
        base_doc, base_hashes = base
        dirty = cache.dirty_sources(hashes, base_hashes, edges)
        if cache.worth_partial(dirty, len(texts)):
            out = compile_standard(
                texts,
                solc_version=solc,
                optimize=optimize,
                evm_version=evm_version,
                remappings=remappings,
                output_selection=_partial_selection(sorted(dirty)),
            )
            merged = cache.merge_output(base_doc, out, dirty)
            if merged is not None:
                if on_notice:
                    on_notice(f"recompiled {len(dirty)} of {len(texts)} sources")
                return merged

    out = compile_standard(
        texts,
        solc_version=solc,
        optimize=optimize,
        evm_version=evm_version,
        remappings=remappings,
    )
    return _cacheable(out)
