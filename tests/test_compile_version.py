"""Auto-detection of the solc version from source pragmas.

The resolver mirrors Foundry: read every `pragma solidity` line, intersect the constraints,
and pick the highest compatible release. These tests exercise the selection logic against
mocked version lists, so they neither hit the network nor invoke solc.
"""

from __future__ import annotations

import pytest
from packaging.version import Version

from sevm.compile import CompileError, SourceFile, resolve_solc_version
from sevm.compile import solc as C
from sevm.compile.versions import _extract_pragmas, _pragma_matches

# A stand-in for what solcx would report as installable, spanning the versions the tests
# care about (a pinned 0.8.21, a caret range, and a couple of neighbours to pick between).
INSTALLABLE = [
    Version(v)
    for v in (
        "0.6.12",
        "0.7.6",
        "0.8.0",
        "0.8.19",
        "0.8.20",
        "0.8.21",
        "0.8.28",
        "0.8.30",
    )
]


def _sources(*texts: str) -> dict[str, SourceFile]:
    return {
        f"F{i}.sol": SourceFile(key=f"F{i}.sol", abs_path=f"/tmp/F{i}.sol", text=t)
        for i, t in enumerate(texts)
    }


@pytest.fixture
def mock_versions(monkeypatch):
    """Point the resolver at a fixed installable list and an empty installed set."""
    monkeypatch.setattr(
        C.solcx, "get_installable_solc_versions", lambda: list(INSTALLABLE)
    )
    monkeypatch.setattr(C.solcx, "get_installed_solc_versions", lambda: [])


# -- pragma extraction -------------------------------------------------------


def test_extract_pragmas_dedupes_and_tracks_files():
    # _sources keys files F0.sol, F1.sol, F2.sol in order.
    srcs = _sources(
        "// SPDX\npragma solidity 0.8.21;\ncontract A {}",
        "pragma solidity 0.8.21;\ncontract B {}",
        "pragma solidity >=0.6.2 <0.9.0;\ncontract C {}",
    )
    assert _extract_pragmas(srcs) == {
        "0.8.21": ["F0.sol", "F1.sol"],
        ">=0.6.2 <0.9.0": ["F2.sol"],
    }


def test_extract_pragmas_none():
    assert _extract_pragmas(_sources("contract A {}")) == {}


# -- single-pragma matching semantics ---------------------------------------


def test_pinned_pragma_matches_one_version():
    assert _pragma_matches("0.8.21", INSTALLABLE) == {Version("0.8.21")}


def test_caret_is_solidity_caret_not_pep440():
    # ^0.8.0 means >=0.8.0 <0.9.0, so 0.7.x and any 0.9 are excluded.
    matched = _pragma_matches("^0.8.0", INSTALLABLE)
    assert Version("0.8.30") in matched
    assert Version("0.7.6") not in matched


def test_range_pragma():
    matched = _pragma_matches(">=0.6.2 <0.9.0", INSTALLABLE)
    assert Version("0.6.12") in matched and Version("0.8.30") in matched
    assert Version("0.6.12") == min(matched)


# -- full resolution ---------------------------------------------------------


def test_reported_regression_pinned_version(mock_versions):
    # The bug: sources pinned to 0.8.21 were built with the default 0.8.28 and failed.
    srcs = _sources("pragma solidity 0.8.21;\ncontract Setup {}")
    assert resolve_solc_version(srcs) == "0.8.21"


def test_caret_picks_highest_compatible(mock_versions):
    # Foundry-exact: highest release satisfying the range, even if newer than the default.
    srcs = _sources("pragma solidity ^0.8.0;\ncontract A {}")
    assert resolve_solc_version(srcs) == "0.8.30"


def test_intersection_of_pinned_and_permissive(mock_versions):
    # A pinned target plus a permissive lib (forge-std shape) collapses to the pin.
    srcs = _sources(
        "pragma solidity 0.8.21;\ncontract Target {}",
        "pragma solidity >=0.6.2 <0.9.0;\ncontract Lib {}",
    )
    assert resolve_solc_version(srcs) == "0.8.21"


def test_intersection_of_two_carets(mock_versions):
    srcs = _sources(
        "pragma solidity ^0.8.20;\ncontract A {}",
        "pragma solidity ^0.8.0;\ncontract B {}",
    )
    assert resolve_solc_version(srcs) == "0.8.30"


def test_explicit_overrides_pragma(mock_versions):
    srcs = _sources("pragma solidity 0.8.21;\ncontract A {}")
    assert resolve_solc_version(srcs, explicit="0.8.19") == "0.8.19"


def test_config_pin_overrides_pragma(mock_versions):
    srcs = _sources("pragma solidity ^0.8.0;\ncontract A {}")
    assert resolve_solc_version(srcs, config_pinned="0.8.20") == "0.8.20"


def test_no_pragma_falls_back_to_default(mock_versions):
    assert resolve_solc_version(_sources("contract A {}")) == C.DEFAULT_SOLC_VERSION


def test_conflicting_pragmas_raise_and_name_files(mock_versions):
    srcs = _sources(
        "pragma solidity 0.8.21;\ncontract A {}",
        "pragma solidity ^0.8.28;\ncontract B {}",
    )
    with pytest.raises(CompileError) as exc:
        resolve_solc_version(srcs)
    msg = str(exc.value)
    # Both the conflicting constraints and the files behind them are surfaced.
    assert "0.8.21" in msg and "^0.8.28" in msg
    assert "F0.sol" in msg and "F1.sol" in msg


def test_offline_falls_back_to_installed(monkeypatch):
    # Installable lookup fails (offline); an installed version still satisfies the pragma.
    def boom():
        raise ConnectionError("offline")

    monkeypatch.setattr(C.solcx, "get_installable_solc_versions", boom)
    monkeypatch.setattr(
        C.solcx, "get_installed_solc_versions", lambda: [Version("0.8.21")]
    )
    srcs = _sources("pragma solidity ^0.8.0;\ncontract A {}")
    assert resolve_solc_version(srcs) == "0.8.21"
