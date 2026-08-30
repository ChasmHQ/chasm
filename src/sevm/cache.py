"""On-disk build cache: don't run solc again for sources that have not changed.

Foundry-shaped. A compilation unit (settings + every source's content hash) is stored as
one gzipped copy of solc's standard-JSON output, so a re-run with nothing edited skips
solc entirely and rebuilds the Project from the cached document.

On a miss, the newest unit with the same settings is the *base* for a partial build: solc
is still given every source, only `outputSelection` narrows to the files that changed and
the files importing them. Emitting the ASTs of a 40-source project costs ~0.9s of a 1.0s
solc call, so narrowing the request is most of the win, and because the source set handed
to solc is unchanged the file ids, analysis and source maps stay identical to a full
build. `merge_output` re-checks those ids anyway and refuses the merge if any moved.

Every failure here is a cache miss, never an error: a corrupt, truncated or stale entry
must only cost a recompile.
"""

from __future__ import annotations

import contextlib
import gzip
import hashlib
import json
import os
import time
from collections.abc import Callable, Mapping, Sequence
from typing import Any

# Bump when the cached document's meaning changes (new solc output field, different
# _build_project semantics). Settings and output selection are hashed, not versioned.
CACHE_SCHEMA = 1

# Units kept per project. Enough to switch between a couple of branches and still hit.
_MAX_UNITS = 5

_VERSIONS_TTL = 24 * 3600

# Above this share of dirty sources a partial build saves nothing worth the merge.
_PARTIAL_LIMIT = 0.6


def user_cache_dir() -> str:
    base = os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache")
    return os.path.join(base, "sevm")


def cache_dir(root: str) -> str:
    """Where this root's units live.

    A Foundry project gets `cache/sevm/`, which its .gitignore already covers and
    `forge clean` already removes. Anywhere else sevm keeps out of the directory.
    """
    if os.path.isfile(os.path.join(root, "foundry.toml")):
        return os.path.join(root, "cache", "sevm")
    digest = hashlib.sha256(os.path.abspath(root).encode()).hexdigest()[:16]
    return os.path.join(user_cache_dir(), "projects", digest)


def hash_sources(sources: Mapping[str, str]) -> dict[str, str]:
    """Source key -> content hash."""
    return {k: hashlib.sha256(v.encode()).hexdigest() for k, v in sources.items()}


def settings_hash(
    solc_version: str,
    optimize: bool,
    evm_version: str | None,
    remappings: Sequence[str],
    output_selection: Any,
) -> str:
    payload = json.dumps(
        {
            "schema": CACHE_SCHEMA,
            "solc": solc_version,
            "optimize": bool(optimize),
            "evm": evm_version,
            "remappings": sorted(remappings),
            "selection": output_selection,
        },
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode()).hexdigest()


def unit_hash(settings: str, hashes: Mapping[str, str]) -> str:
    payload = json.dumps([settings, sorted(hashes.items())], sort_keys=True)
    return hashlib.sha256(payload.encode()).hexdigest()


# -- the dirty set -----------------------------------------------------------


def dependents(edges: Mapping[str, Sequence[str]], seed: set[str]) -> set[str]:
    """`seed` plus every source that transitively imports one of them.

    An edited library changes the bytecode of everything that inherits from or calls into
    it, so its importers cannot be reused.
    """
    importers: dict[str, list[str]] = {}
    for importer, imported in edges.items():
        for target in imported:
            importers.setdefault(target, []).append(importer)
    reached = set(seed)
    queue = list(seed)
    while queue:
        for importer in importers.get(queue.pop(), ()):
            if importer not in reached:
                reached.add(importer)
                queue.append(importer)
    return reached


def dirty_sources(
    current: Mapping[str, str],
    base: Mapping[str, str],
    edges: Mapping[str, Sequence[str]],
) -> set[str]:
    """Sources whose output cannot be reused from a build of `base`."""
    changed = {k for k, h in current.items() if base.get(k) != h}
    return dependents(edges, changed) & set(current)


def worth_partial(dirty: set[str], total: int) -> bool:
    return 0 < len(dirty) <= max(1, int(total * _PARTIAL_LIMIT))


def merge_output(base_doc: dict, new_doc: dict, dirty: set[str]) -> dict | None:
    """Overlay a narrowed compile onto the base document, or None if it cannot be trusted.

    Source ids are only stable while the source *set* is; adding or removing a file shifts
    them, and a reused source map would then point at the wrong file. Comparing every
    reused id against the one solc just reported catches that for free.
    """
    base_sources = base_doc.get("sources") or {}
    new_sources = new_doc.get("sources") or {}
    base_contracts = base_doc.get("contracts") or {}
    new_contracts = new_doc.get("contracts") or {}

    sources: dict[str, Any] = {}
    contracts: dict[str, Any] = {}
    for key, entry in new_sources.items():
        if key in dirty:
            if "ast" not in entry:
                return None
            sources[key] = entry
            if key in new_contracts:
                contracts[key] = new_contracts[key]
            continue
        cached = base_sources.get(key)
        if cached is None or cached.get("id") != entry.get("id"):
            return None
        sources[key] = cached
        if key in base_contracts:
            contracts[key] = base_contracts[key]
    return {"sources": sources, "contracts": contracts}


# -- storage -----------------------------------------------------------------


class BuildCache:
    """The unit store for one project root."""

    def __init__(self, directory: str) -> None:
        self.dir = directory

    def _path(self, unit: str) -> str:
        return os.path.join(self.dir, f"{unit}.json.gz")

    def load(self, unit: str) -> dict | None:
        try:
            with gzip.open(self._path(unit), "rb") as fh:
                doc = json.loads(fh.read())
        except Exception:
            return None
        return doc if isinstance(doc, dict) and "sources" in doc else None

    def base_for(
        self, settings: str, keys: set[str]
    ) -> tuple[dict, dict[str, str]] | None:
        """Newest unit built with the same settings from the same source set, if any."""
        entries = [
            (name, meta)
            for name, meta in self._index().get("units", {}).items()
            if meta.get("settings") == settings and set(meta.get("sources", {})) == keys
        ]
        for name, meta in sorted(
            entries, key=lambda e: e[1].get("created", 0), reverse=True
        ):
            doc = self.load(name)
            if doc is not None:
                return doc, meta.get("sources", {})
        return None

    def store(
        self, unit: str, doc: dict, settings: str, hashes: Mapping[str, str]
    ) -> None:
        try:
            os.makedirs(self.dir, exist_ok=True)
            _write_atomic(self._path(unit), gzip.compress(json.dumps(doc).encode(), 1))
            index = self._index()
            units = index.setdefault("units", {})
            units[unit] = {
                "settings": settings,
                "sources": dict(hashes),
                "created": time.time(),
            }
            self._prune(units)
            index["schema"] = CACHE_SCHEMA
            _write_atomic(
                os.path.join(self.dir, "index.json"), json.dumps(index, indent=1).encode()
            )
        except Exception:
            pass

    def _index(self) -> dict:
        try:
            with open(os.path.join(self.dir, "index.json"), "rb") as fh:
                index = json.loads(fh.read())
        except Exception:
            return {}
        if not isinstance(index, dict) or index.get("schema") != CACHE_SCHEMA:
            return {}
        return index

    def _prune(self, units: dict) -> None:
        stale = sorted(units, key=lambda n: units[n].get("created", 0), reverse=True)
        for name in stale[_MAX_UNITS:]:
            units.pop(name, None)
            with contextlib.suppress(OSError):
                os.remove(self._path(name))


def open_cache(root: str, enabled: bool = True) -> BuildCache | None:
    if not enabled or os.environ.get("SEVM_NO_CACHE"):
        return None
    return BuildCache(cache_dir(root))


def _write_atomic(path: str, payload: bytes) -> None:
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "wb") as fh:
        fh.write(payload)
    os.replace(tmp, path)


# -- solc release lists ------------------------------------------------------


def cached_json(name: str, fetch: Callable[[], Any], ttl: float = _VERSIONS_TTL) -> Any:
    """`fetch()`'s result, kept in `<cache>/<name>` for a day.

    Version auto-detection otherwise pays a network round-trip on every run, and silently
    degrades to the installed set when offline; a cached list keeps resolution the same
    off the network as on it. A fetch that raises returns `{}` rather than propagating,
    for the same reason a stale cache entry only costs a recompile.
    """
    path = os.path.join(user_cache_dir(), name)
    try:
        with open(path, "rb") as fh:
            entry = json.loads(fh.read())
        if time.time() - float(entry["fetched"]) < ttl:
            return entry["payload"]
    except Exception:
        pass
    try:
        payload = fetch()
    except Exception:
        return {}
    try:
        os.makedirs(user_cache_dir(), exist_ok=True)
        _write_atomic(
            path, json.dumps({"fetched": time.time(), "payload": payload}).encode()
        )
    except Exception:
        pass
    return payload
