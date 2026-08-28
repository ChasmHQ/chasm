"""Picking the solc version to build with.

solc rejects a source whose `pragma solidity` does not match the compiler version, so like
Foundry we intersect every source's pragma and install the highest compatible release.
solcx's comparator gets solidity's caret right (`^0.8.0` means `>=0.8.0 <0.9.0`).
"""

from __future__ import annotations

import re
from collections.abc import Sequence

import solcx
from packaging.specifiers import SpecifierSet
from packaging.version import Version

from .. import cache
from .model import CompileError, SourceFile
from .solc import DEFAULT_SOLC_VERSION

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
