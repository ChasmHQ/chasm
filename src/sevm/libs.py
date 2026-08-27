"""Solidity dependency resolution: read imports, find the repo, clone it, remap it.

sevm no longer ships its own forge-std. A `.t.sol` (or any contract) gets the real library
from the real source, installed the way `forge install` leaves it: a shallow clone under
`lib/<name>`, pinned to the newest stable tag, with an explicit remapping so `forge` and
sevm resolve the import identically.

Git is the only requirement: `git ls-remote --tags` lists releases unauthenticated (no
GitHub API rate limit) and `git clone --depth 1 --branch <tag>` fetches one. The `forge`
binary is never invoked.

An unknown import prefix is resolved through npm's registry metadata
(`repository.url` -> the GitHub repo), which covers most published Solidity libraries;
`ALIASES` short-circuits the common ones and the git-only ones npm does not carry.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass

from packaging.version import InvalidVersion, Version

# Import prefix -> git URL, for libraries npm cannot resolve (forge-std, ds-test) or where
# the npm package points somewhere less useful than the canonical repo.
ALIASES: dict[str, str] = {
    "forge-std": "https://github.com/foundry-rs/forge-std",
    "ds-test": "https://github.com/dapphub/ds-test",
    "solmate": "https://github.com/transmissions11/solmate",
    "solady": "https://github.com/Vectorized/solady",
    "@openzeppelin/contracts": "https://github.com/OpenZeppelin/openzeppelin-contracts",
    "@openzeppelin/contracts-upgradeable": (
        "https://github.com/OpenZeppelin/openzeppelin-contracts-upgradeable"
    ),
    "openzeppelin-contracts": ("https://github.com/OpenZeppelin/openzeppelin-contracts"),
}

NPM_REGISTRY = "https://registry.npmjs.org"

# Never searched when deriving a remapping: a library's own tests import the same paths as
# its sources, so matching there yields a remapping that points at the wrong directory.
_IGNORED_DIRS = frozenset({".git", "test", "tests", "script", "node_modules", "out"})

_HTTP_TIMEOUT = 15.0
_GIT_TIMEOUT = 300.0


class LibError(RuntimeError):
    """A dependency could not be resolved or installed. The message names the fix."""


@dataclass(frozen=True)
class Dependency:
    """One installed library and the remapping that makes its imports resolve."""

    prefix: str  # the import prefix, e.g. "@openzeppelin/contracts"
    name: str  # the directory under libs/, e.g. "openzeppelin-contracts"
    repo_url: str
    version: str  # tag, or a branch name when the repo has no releases
    path: str  # absolute path of the clone
    remapping: (
        str  # e.g. "@openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/"
    )


# ---- imports ---------------------------------------------------------------

# All four import forms: plain, named, aliased-namespace, and default-ish. Only the quoted
# path is captured; solc allows either quote style.
_IMPORT_RE = re.compile(
    r"""^\s*import\s+(?:[^'";]*?\bfrom\s+)?['"]([^'"]+)['"]\s*;""",
    re.MULTILINE,
)
_LINE_COMMENT_RE = re.compile(r"//[^\n]*")
_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)


def iter_imports(text: str) -> list[str]:
    """Every import path in a Solidity source, comments stripped first."""
    stripped = _BLOCK_COMMENT_RE.sub("", _LINE_COMMENT_RE.sub("", text))
    return _IMPORT_RE.findall(stripped)


def apply_remappings(path: str, remappings: Sequence[str]) -> str:
    """Rewrite an import through the longest matching remapping, as solc does.

    Ties go to the last entry, so an explicit remapping overrides the autodetected one for
    the same prefix.
    """
    best_from = ""
    best_to = ""
    for entry in remappings:
        prefix, _, target = entry.partition("=")
        if path.startswith(prefix) and len(prefix) >= len(best_from):
            best_from, best_to = prefix, target
    if not best_from:
        return path
    return best_to + path[len(best_from) :]


def resolve_import(
    path: str, importer_key: str, root: str, remappings: Sequence[str]
) -> str | None:
    """Resolve one import to a root-relative source key, or None if it is not on disk.

    `importer_key` is the importing file's own key, so `./X.sol` resolves beside it.
    """
    if path.startswith("./") or path.startswith("../"):
        joined = os.path.normpath(os.path.join(os.path.dirname(importer_key), path))
    else:
        joined = os.path.normpath(apply_remappings(path, remappings))
    key = joined.replace(os.sep, "/")
    if key.startswith("../"):
        return None
    return key if os.path.isfile(os.path.join(root, key)) else None


@dataclass
class Closure:
    """Result of walking the import graph: what to compile, and what is missing."""

    extra: dict[str, str]  # source key -> file text, for files outside the roots
    unresolved: dict[str, str]  # import path -> the source key that imports it


def import_closure(
    sources: dict[str, str], root: str, remappings: Sequence[str]
) -> Closure:
    """Follow imports out of `sources` (key -> text) until nothing new is reachable.

    Only the closure is compiled, so a library's own `test/` and `script/` trees never
    reach solc: forge-std's tests carry unlinked library placeholders that the artifact
    model cannot represent, and compiling them costs time for contracts nobody debugs.
    """
    extra: dict[str, str] = {}
    unresolved: dict[str, str] = {}
    queue: list[tuple[str, str]] = list(sources.items())
    while queue:
        key, text = queue.pop()
        for path in iter_imports(text):
            target = resolve_import(path, key, root, remappings)
            if target is None:
                unresolved.setdefault(path, key)
                continue
            if target in sources or target in extra:
                continue
            with open(os.path.join(root, target), encoding="utf-8") as fh:
                target_text = fh.read()
            extra[target] = target_text
            queue.append((target, target_text))
    return Closure(extra=extra, unresolved=unresolved)


def package_of(import_path: str) -> str:
    """The dependency prefix an import belongs to: `@scope/name`, or the first segment."""
    parts = import_path.split("/")
    if import_path.startswith("@") and len(parts) >= 2:
        return "/".join(parts[:2])
    return parts[0]


# ---- repo lookup -----------------------------------------------------------


def _run_git(args: Sequence[str], timeout: float = _GIT_TIMEOUT) -> str:
    try:
        done = subprocess.run(
            ["git", *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError as exc:
        raise LibError(
            "git is not installed; sevm needs it to fetch dependencies"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise LibError(f"git {' '.join(args)} timed out") from exc
    if done.returncode != 0:
        raise LibError(f"git {' '.join(args[:2])} failed: {done.stderr.strip()}")
    return done.stdout


def _fetch_json(url: str) -> dict | None:
    try:
        with urllib.request.urlopen(url, timeout=_HTTP_TIMEOUT) as response:
            return json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, ValueError, OSError):
        return None


def normalize_repo_url(url: str) -> str:
    """npm records repos as `git+https://...git`, `git://...`, or `git@host:org/repo`."""
    url = url.strip()
    if url.startswith("git+"):
        url = url[len("git+") :]
    if url.startswith("ssh://git@"):
        url = "https://" + url[len("ssh://git@") :]
    elif url.startswith("git://"):
        url = "https://" + url[len("git://") :]
    elif url.startswith("git@"):
        url = "https://" + url[len("git@") :].replace(":", "/", 1)
    if url.endswith(".git"):
        url = url[: -len(".git")]
    return url


def npm_repo_url(package: str) -> str | None:
    """The git repo behind an npm package, from its registry metadata."""
    data = _fetch_json(f"{NPM_REGISTRY}/{urllib.parse.quote(package, safe='@')}")
    if not data:
        return None
    latest = (data.get("dist-tags") or {}).get("latest")
    versions = data.get("versions") or {}
    meta = versions.get(latest) or {}
    repo = meta.get("repository") or data.get("repository") or {}
    url = repo.get("url") if isinstance(repo, dict) else repo
    if not isinstance(url, str) or not url:
        return None
    return normalize_repo_url(url)


def repo_url_for(prefix: str) -> str | None:
    """Git URL for an import prefix: the alias table first, then npm."""
    if prefix in ALIASES:
        return ALIASES[prefix]
    return npm_repo_url(prefix)


def _tag_sort_key(tag: str) -> tuple[int, Version | None, str]:
    """Sort tags by semver where possible; unparseable tags rank below parseable ones."""
    try:
        return (1, Version(tag.lstrip("vV")), tag)
    except InvalidVersion:
        return (0, None, tag)


def newest_tag(repo_url: str) -> str | None:
    """Newest stable release tag of a remote, or None when it publishes no releases.

    Prereleases are skipped so a `-rc` never becomes the pinned version.
    """
    out = _run_git(["ls-remote", "--tags", "--refs", repo_url], timeout=60.0)
    tags = []
    for line in out.splitlines():
        _, _, ref = line.partition("refs/tags/")
        ref = ref.strip()
        if not ref:
            continue
        try:
            if Version(ref.lstrip("vV")).is_prerelease:
                continue
        except InvalidVersion:
            if "-" in ref:  # unparseable and dash-suffixed: treat as a prerelease
                continue
        tags.append(ref)
    return max(tags, key=_tag_sort_key) if tags else None


def clone(repo_url: str, version: str | None, dest: str) -> None:
    """Shallow-clone one ref. A failed clone leaves no half-written directory behind."""
    os.makedirs(os.path.dirname(dest) or ".", exist_ok=True)
    args = ["clone", "--depth", "1", "--quiet"]
    if version:
        args += ["--branch", version]
    args += [repo_url, dest]
    try:
        _run_git(args)
    except LibError:
        shutil.rmtree(dest, ignore_errors=True)
        raise


# ---- remapping derivation --------------------------------------------------


def _find_suffix_dir(dep_dir: str, suffix: str) -> str | None:
    """Directory D under `dep_dir` such that D/<suffix> exists; shallowest wins."""
    suffix = suffix.lstrip("/")
    if not suffix:
        return None
    candidates: list[tuple[int, str]] = []
    for cur, dirs, _files in os.walk(dep_dir):
        dirs[:] = [d for d in dirs if d not in _IGNORED_DIRS and not d.startswith(".")]
        if os.path.isfile(os.path.join(cur, *suffix.split("/"))):
            rel = os.path.relpath(cur, dep_dir)
            candidates.append((0 if rel == "." else rel.count(os.sep) + 1, cur))
    if not candidates:
        return None
    return min(candidates)[1]


def remapping_for(prefix: str, dep_dir: str, sample_import: str, root: str) -> str:
    """Build the remapping that makes `sample_import` resolve inside the clone.

    Derived from where the imported file actually landed, so `src/`, `contracts/` and flat
    layouts all work without a per-library rule.
    """
    suffix = sample_import[len(prefix) :].lstrip("/")
    base = _find_suffix_dir(dep_dir, suffix)
    if base is None:
        raise LibError(
            f"cloned {os.path.basename(dep_dir)} but found no {suffix} inside it; "
            f"add a remapping for {prefix}/ by hand"
        )
    rel = os.path.relpath(base, root).replace(os.sep, "/").rstrip("/")
    return f"{prefix}/={rel}/"


# ---- install ---------------------------------------------------------------


def dep_dir_name(prefix: str, repo_url: str) -> str:
    """Directory name under lib/, matching what `forge install` would create."""
    return repo_url.rstrip("/").rsplit("/", 1)[-1] or prefix.replace("/", "-")


def install(
    prefix: str,
    sample_import: str,
    root: str,
    libs_dir: str = "lib",
    *,
    on_notice: Callable[[str], None] | None = None,
) -> Dependency:
    """Install one dependency into `<root>/<libs_dir>/` and return its remapping.

    An existing clone is reused as-is, never re-fetched: the pin belongs to the project,
    the same way `forge install` pins a submodule commit.
    """
    notice = on_notice or (lambda _m: None)
    repo_url = repo_url_for(prefix)
    if repo_url is None:
        raise LibError(
            f"cannot resolve import {sample_import!r}: no library named {prefix!r} in "
            f"sevm's table or on npm.\n"
            f"Install it yourself and add the remapping:\n"
            f"  forge install <org>/<repo>\n"
            f"  echo '{prefix}/=lib/<repo>/src/' >> remappings.txt"
        )
    name = dep_dir_name(prefix, repo_url)
    dep_dir = os.path.join(root, libs_dir, name)
    if os.path.isdir(dep_dir):
        version = _installed_version(dep_dir)
    else:
        version = newest_tag(repo_url)
        notice(f"installing {prefix} from {repo_url} @ {version or 'default branch'}")
        clone(repo_url, version, dep_dir)
    return Dependency(
        prefix=prefix,
        name=name,
        repo_url=repo_url,
        version=version or "",
        path=dep_dir,
        remapping=remapping_for(prefix, dep_dir, sample_import, root),
    )


def _installed_version(dep_dir: str) -> str | None:
    """Best-effort version of an existing clone; absent git metadata is not an error."""
    try:
        out = _run_git(["-C", dep_dir, "describe", "--tags", "--always"], timeout=30.0)
    except LibError:
        return None
    return out.strip() or None


def missing_prefixes(unresolved: Iterable[str]) -> list[str]:
    """Distinct dependency prefixes behind a set of unresolved imports, in first-seen order."""
    seen: list[str] = []
    for path in unresolved:
        prefix = package_of(path)
        if prefix not in seen:
            seen.append(prefix)
    return seen


def write_remappings(root: str, entries: Sequence[str]) -> list[str]:
    """Append missing lines to `<root>/remappings.txt` so `forge` resolves as sevm does.

    Returns the lines actually added; existing entries are never rewritten or reordered.
    """
    path = os.path.join(root, "remappings.txt")
    text = ""
    if os.path.isfile(path):
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
    have = {line.strip() for line in text.splitlines() if line.strip()}
    added = [e for e in entries if e not in have]
    if not added:
        return []
    with open(path, "a", encoding="utf-8") as fh:
        if text and not text.endswith("\n"):
            fh.write("\n")
        fh.write("\n".join(added) + "\n")
    return added
