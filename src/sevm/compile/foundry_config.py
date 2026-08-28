"""Reading a Foundry project: foundry.toml, remappings, sources and dependencies.

Only the project's own sources are collected by directory walk; library sources enter
solely through the import closure, so a library's `test/` tree never reaches solc.
"""

from __future__ import annotations

import os
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from ..libs import (
    Dependency,
    LibError,
    import_closure,
    install,
    missing_prefixes,
    package_of,
)
from .model import CompileError, SourceFile
from .solc import _read_sol

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
