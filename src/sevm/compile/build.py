"""Running a build: solc in, `Project` out.

`compile_foundry_project` is the entry point everything else uses. Results are cached per
compilation unit (see `cache.py`) and written out as forge-shaped artifacts (see
`artifacts.py`).
"""

from __future__ import annotations

import os
from collections.abc import Callable, Sequence

from .. import artifacts, cache
from ..libs import write_remappings
from . import solc
from .foundry_config import (
    FORGE_STD,
    FORGE_STD_SAMPLE_IMPORT,
    _collect_project_sources,
    read_foundry_config,
    resolve_dependencies,
)
from .model import Artifact, Project, SourceFile
from .solc import (
    _OUTPUT_SELECTION,
    _contract_ranges,
    _read_sources,
    ensure_solc,
)
from .versions import resolve_solc_version


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
    out = solc.compile_standard(
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
    version = resolve_solc_version(
        sources, explicit=solc_version, config_pinned=cfg.solc_version
    )
    ensure_solc(version)

    texts = {k: s.text for k, s in sources.items()}
    store = cache.open_cache(root, use_cache)
    settings = cache.settings_hash(
        version, optimize, evm_version or cfg.evm_version, remappings, _OUTPUT_SELECTION
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
            version=version,
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

    project = _build_project(out, sources, version, optimize, remappings=remappings)
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
    version: str,
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
            out = solc.compile_standard(
                texts,
                solc_version=version,
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

    out = solc.compile_standard(
        texts,
        solc_version=version,
        optimize=optimize,
        evm_version=evm_version,
        remappings=remappings,
    )
    return _cacheable(out)
