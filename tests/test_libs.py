"""Resolving an import to a library, and installing it.

Everything here is hermetic. Libraries are cloned from local git repos built by
`conftest.make_repo`, so the real `git clone`/tag-selection/remapping code runs with no
network.
"""

from __future__ import annotations

import os
import shutil
import sys

import pytest
from conftest import FAKE_NEWEST, FIXTURES, make_repo

from sevm import libs


def remove_tree(path: str) -> None:
    """shutil.rmtree that clears read-only files first.

    Git writes its object files read-only, which makes rmtree fail on Windows with
    PermissionError; on POSIX the retry handler is never needed but costs nothing.
    """

    def clear_readonly(func, p, _exc):  # type: ignore[no-untyped-def]
        os.chmod(p, 0o777)
        func(p)

    if sys.version_info >= (3, 12):
        shutil.rmtree(path, onexc=clear_readonly)
    else:
        shutil.rmtree(path, onerror=clear_readonly)


def read_text(path: str) -> str:
    with open(path, encoding="utf-8") as fh:
        return fh.read()


# Marked tests reach the real repositories; conftest skips them without
# SEVM_NETWORK_TESTS=1.
NETWORK = pytest.mark.network


# ==================================================================


def test_iter_imports_covers_every_form():
    text = """
    // import "commented/out.sol";
    /* import "also/out.sol"; */
    import "plain/A.sol";
    import {B, C} from "named/B.sol";
    import * as D from "aliased/D.sol";
    import E from 'quoted/E.sol';
    """
    assert libs.iter_imports(text) == [
        "plain/A.sol",
        "named/B.sol",
        "aliased/D.sol",
        "quoted/E.sol",
    ]


def test_apply_remappings_longest_prefix_then_last_wins():
    rms = [
        "@oz/=lib/a/",
        "@oz/contracts/=lib/b/",
        "forge-std/=lib/x/",
        "forge-std/=lib/y/",
    ]
    assert libs.apply_remappings("@oz/contracts/T.sol", rms) == "lib/b/T.sol"
    assert libs.apply_remappings("@oz/other/T.sol", rms) == "lib/a/other/T.sol"
    # An explicit remapping added later replaces an earlier guess for the same prefix.
    assert libs.apply_remappings("forge-std/T.sol", rms) == "lib/y/T.sol"
    assert libs.apply_remappings("./Local.sol", rms) == "./Local.sol"


def test_package_of():
    assert libs.package_of("@openzeppelin/contracts/token/ERC20/ERC20.sol") == (
        "@openzeppelin/contracts"
    )
    assert libs.package_of("forge-std/Test.sol") == "forge-std"
    assert libs.package_of("solady/utils/LibString.sol") == "solady"


def test_import_closure_follows_and_reports(tmp_path):
    root = tmp_path
    (root / "src").mkdir()
    (root / "lib" / "dep" / "src").mkdir(parents=True)
    (root / "src" / "A.sol").write_text(
        'import {B} from "dep/B.sol";\nimport "./C.sol";\nimport "missing/M.sol";\n'
    )
    (root / "src" / "C.sol").write_text("contract C {}\n")
    (root / "lib" / "dep" / "src" / "B.sol").write_text('import "./D.sol";\n')
    (root / "lib" / "dep" / "src" / "D.sol").write_text("contract D {}\n")

    closure = libs.import_closure(
        {"src/A.sol": (root / "src" / "A.sol").read_text()},
        str(root),
        ["dep/=lib/dep/src/"],
    )
    # The library file and everything it imports come along; the local one does too.
    assert set(closure.extra) == {
        "src/C.sol",
        "lib/dep/src/B.sol",
        "lib/dep/src/D.sol",
    }
    assert closure.unresolved == {"missing/M.sol": "src/A.sol"}


def test_newest_tag_skips_prereleases_and_sorts_semver(tmp_path):
    url = make_repo(
        os.path.join(FIXTURES, "forge_std_fake"),
        str(tmp_path / "repo"),
        tags=("v0.9.0", "v0.10.0", "v2.0.0-rc.1", "v0.10.0-beta"),
    )
    assert libs.newest_tag(url) == "v0.10.0"


def test_newest_tag_none_without_releases(tmp_path):
    url = make_repo(
        os.path.join(FIXTURES, "forge_std_fake"), str(tmp_path / "r"), tags=()
    )
    assert libs.newest_tag(url) is None


def test_clone_without_tag_takes_the_default_branch(tmp_path):
    url = make_repo(
        os.path.join(FIXTURES, "forge_std_fake"), str(tmp_path / "r"), tags=()
    )
    dest = str(tmp_path / "clone")
    libs.clone(url, None, dest)
    assert os.path.isfile(os.path.join(dest, "src", "Test.sol"))


def test_non_semver_tags_do_not_break_selection(tmp_path):
    url = make_repo(
        os.path.join(FIXTURES, "forge_std_fake"),
        str(tmp_path / "repo"),
        tags=("release-candidate", "v1.0.0", "nightly"),
    )
    # A tag no version parser understands must never outrank a real release.
    assert libs.newest_tag(url) == "v1.0.0"


def test_missing_git_is_reported_as_such(monkeypatch, tmp_path):
    def no_git(*a, **k):
        raise FileNotFoundError("git")

    monkeypatch.setattr(libs.subprocess, "run", no_git)
    with pytest.raises(libs.LibError, match="git is not installed"):
        libs.newest_tag("https://example.invalid/repo")


def test_git_timeout_is_reported_as_such(monkeypatch):
    def slow(*a, **k):
        raise libs.subprocess.TimeoutExpired(cmd="git", timeout=1)

    monkeypatch.setattr(libs.subprocess, "run", slow)
    with pytest.raises(libs.LibError, match="timed out"):
        libs.newest_tag("https://example.invalid/repo")


def test_npm_lookup_survives_an_unreachable_registry(monkeypatch):
    monkeypatch.setattr(libs, "NPM_REGISTRY", "https://127.0.0.1:1")
    assert libs.npm_repo_url("anything") is None


def test_a_clone_without_git_metadata_is_still_usable(tmp_path, local_forge_std):
    """A library copied in by hand has no tags to describe; that is not an error."""
    root = str(tmp_path)
    libs.install("forge-std", "forge-std/Test.sol", root)
    remove_tree(os.path.join(root, "lib", "forge-std", ".git"))
    dep = libs.install("forge-std", "forge-std/Test.sol", root)
    assert dep.version == ""
    assert dep.remapping == "forge-std/=lib/forge-std/src/"


def test_an_import_escaping_the_root_is_unresolved(tmp_path):
    (tmp_path / "A.sol").write_text('import "../outside/B.sol";\n')
    closure = libs.import_closure(
        {"A.sol": (tmp_path / "A.sol").read_text()}, str(tmp_path), []
    )
    assert closure.unresolved == {"../outside/B.sol": "A.sol"}


def test_clone_leaves_nothing_behind_on_failure(tmp_path):
    dest = str(tmp_path / "clone")
    with pytest.raises(libs.LibError):
        libs.clone(f"file://{tmp_path / 'nope'}", None, dest)
    assert not os.path.exists(dest)


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("git+https://github.com/OpenZeppelin/openzeppelin-contracts.git", None),
        ("git://github.com/OpenZeppelin/openzeppelin-contracts.git", None),
        ("git+ssh://git@github.com/OpenZeppelin/openzeppelin-contracts.git", None),
        ("git@github.com:OpenZeppelin/openzeppelin-contracts.git", None),
        ("https://github.com/OpenZeppelin/openzeppelin-contracts", None),
    ],
)
def test_normalize_repo_url(raw, expected):
    assert libs.normalize_repo_url(raw) == (
        expected or "https://github.com/OpenZeppelin/openzeppelin-contracts"
    )


def test_npm_repo_url_reads_registry_metadata(monkeypatch):
    payload = {
        "dist-tags": {"latest": "5.7.0"},
        "versions": {
            "5.7.0": {
                "repository": {
                    "url": "git+https://github.com/OpenZeppelin/openzeppelin-contracts.git"
                }
            }
        },
    }
    seen = {}

    def fake_fetch(url):
        seen["url"] = url
        return payload

    monkeypatch.setattr(libs, "_fetch_json", fake_fetch)
    assert libs.npm_repo_url("@openzeppelin/contracts") == (
        "https://github.com/OpenZeppelin/openzeppelin-contracts"
    )
    # The scope's slash must survive quoting or the registry 404s.
    assert seen["url"].endswith("@openzeppelin%2Fcontracts")


def test_npm_repo_url_missing_package(monkeypatch):
    monkeypatch.setattr(libs, "_fetch_json", lambda url: None)
    assert libs.npm_repo_url("no-such-package-xyz") is None


def test_repo_url_prefers_the_alias_table(monkeypatch):
    monkeypatch.setattr(
        libs, "npm_repo_url", lambda pkg: pytest.fail("npm should not be consulted")
    )
    assert libs.repo_url_for("forge-std") == libs.ALIASES["forge-std"]


def _layout(tmp_path, name: str, *files: str) -> str:
    root = tmp_path / name
    for rel in files:
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("contract X {}\n")
    return str(root)


@pytest.mark.parametrize(
    "layout,sample,expected",
    [
        (("src/Test.sol",), "forge-std/Test.sol", "forge-std/=lib/dep/src/"),
        (
            ("contracts/token/ERC20.sol",),
            "@oz/contracts/token/ERC20.sol",
            "@oz/contracts/=lib/dep/contracts/",
        ),
        (("LibString.sol",), "solady/LibString.sol", "solady/=lib/dep/"),
    ],
)
def test_remapping_derived_from_where_the_file_landed(tmp_path, layout, sample, expected):
    root = str(tmp_path)
    dep = _layout(tmp_path / "lib", "dep", *layout)
    prefix = libs.package_of(sample)
    assert libs.remapping_for(prefix, dep, sample, root) == expected


def test_remapping_ignores_a_librarys_own_tests(tmp_path):
    root = str(tmp_path)
    dep = _layout(tmp_path / "lib", "dep", "test/Test.sol", "src/Test.sol")
    assert (
        libs.remapping_for("forge-std", dep, "forge-std/Test.sol", root)
        == "forge-std/=lib/dep/src/"
    )


def test_remapping_error_names_the_manual_fix(tmp_path):
    dep = _layout(tmp_path / "lib", "dep", "src/Other.sol")
    with pytest.raises(libs.LibError, match="by hand"):
        libs.remapping_for("forge-std", dep, "forge-std/Test.sol", str(tmp_path))


def test_install_clones_pinned_and_derives_the_remapping(tmp_path, local_forge_std):
    root = str(tmp_path)
    dep = libs.install("forge-std", "forge-std/Test.sol", root)
    assert dep.remapping == "forge-std/=lib/forge-std/src/"
    assert dep.version == FAKE_NEWEST
    assert os.path.isfile(os.path.join(root, "lib", "forge-std", "src", "Test.sol"))


def test_install_reuses_an_existing_clone(tmp_path, local_forge_std, monkeypatch):
    root = str(tmp_path)
    libs.install("forge-std", "forge-std/Test.sol", root)
    marker = os.path.join(root, "lib", "forge-std", "src", "MARKER.sol")
    open(marker, "w").close()

    monkeypatch.setattr(
        libs, "clone", lambda *a, **k: pytest.fail("existing clone must be reused")
    )
    dep = libs.install("forge-std", "forge-std/Test.sol", root)
    assert dep.remapping == "forge-std/=lib/forge-std/src/"
    assert os.path.isfile(marker)


def test_install_unknown_library_names_the_manual_fix(tmp_path, monkeypatch):
    monkeypatch.setattr(libs, "npm_repo_url", lambda pkg: None)
    with pytest.raises(libs.LibError) as exc:
        libs.install("mystery", "mystery/A.sol", str(tmp_path))
    assert "forge install" in str(exc.value)
    assert "mystery/=lib/<repo>/src/" in str(exc.value)


def test_write_remappings_appends_once(tmp_path):
    root = str(tmp_path)
    assert libs.write_remappings(root, ["a/=lib/a/src/"]) == ["a/=lib/a/src/"]
    assert libs.write_remappings(root, ["a/=lib/a/src/", "b/=lib/b/src/"]) == [
        "b/=lib/b/src/"
    ]
    assert libs.write_remappings(root, ["a/=lib/a/src/"]) == []
    text = (tmp_path / "remappings.txt").read_text()
    assert text == "a/=lib/a/src/\nb/=lib/b/src/\n"


def test_write_remappings_keeps_a_file_without_a_trailing_newline_valid(tmp_path):
    (tmp_path / "remappings.txt").write_text("a/=lib/a/src/")
    libs.write_remappings(str(tmp_path), ["b/=lib/b/src/"])
    assert (tmp_path / "remappings.txt").read_text() == "a/=lib/a/src/\nb/=lib/b/src/\n"
