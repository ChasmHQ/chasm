"""Dependency resolution and install: imports in, cloned libraries and remappings out.

Everything here is hermetic. Libraries are cloned from local git repos built by
`conftest.make_repo`, so the real `git clone`/tag-selection/remapping code runs with no
network. The live-network equivalents are marked `network` and skipped by default.
"""

from __future__ import annotations

import os
import re
import shutil

import pytest
from conftest import FAKE_NEWEST, FIXTURES, make_repo

from sevm import libs
from sevm.compile import CompileError, compile_foundry_project, read_foundry_config
from sevm.compile.build import _build_project
from sevm.foundry import discover_tests, prepare_project


def read_text(path: str) -> str:
    with open(path, encoding="utf-8") as fh:
        return fh.read()


# Marked tests reach the real repositories; conftest skips them without
# SEVM_NETWORK_TESTS=1.
NETWORK = pytest.mark.network


# ==================================================================
# reading imports
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


# ==================================================================
# picking a version
# ==================================================================


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
    shutil.rmtree(os.path.join(root, "lib", "forge-std", ".git"))
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


# ==================================================================
# finding the repo behind an import
# ==================================================================


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


# ==================================================================
# deriving the remapping from the layout
# ==================================================================


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


# ==================================================================
# installing
# ==================================================================


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


# ==================================================================
# the compile pipeline around it
# ==================================================================


def _standalone(tmp_path, body: str = "") -> str:
    root = tmp_path / "scratch"
    root.mkdir()
    (root / "Demo.t.sol").write_text(
        "// SPDX-License-Identifier: MIT\n"
        "pragma solidity ^0.8.20;\n"
        'import {Test} from "forge-std/Test.sol";\n'
        "contract DemoTest is Test {\n"
        "    function testOne() public pure {\n"
        f"        {body or 'assertEq(uint256(1), uint256(1));'}\n"
        "    }\n"
        "}\n"
    )
    return str(root)


def test_standalone_gets_a_foundry_toml_without_src_or_test(tmp_path, local_forge_std):
    root = _standalone(tmp_path)
    prepared = prepare_project(os.path.join(root, "Demo.t.sol"), assume_yes=True)
    assert prepared.existing is False
    toml = read_text(os.path.join(root, "foundry.toml"))
    assert 'libs = ["lib"]' in toml
    assert "src =" not in toml and "test =" not in toml
    # The config still reads back with Foundry's own defaults for the missing keys.
    cfg = read_foundry_config(root)
    assert (cfg.src, cfg.test, cfg.libs) == ("src", "test", ("lib",))


def test_prompt_names_every_library_it_would_install(
    tmp_path, monkeypatch, local_forge_std
):
    root = _standalone(tmp_path)
    with open(os.path.join(root, "Demo.t.sol"), "a", encoding="utf-8") as fh:
        fh.write('import {X} from "mystery/X.sol";\n')
    seen = {}
    monkeypatch.setattr("sys.stdin.isatty", lambda: True)
    monkeypatch.setattr(
        "builtins.input", lambda prompt: seen.setdefault("p", prompt) and "n"
    )
    prepare_project(os.path.join(root, "Demo.t.sol"), assume_yes=False)
    assert "forge-std" in seen["p"] and "mystery" in seen["p"]
    assert "foundry.toml" in seen["p"]


def test_contracts_that_need_nothing_are_left_alone(tmp_path):
    """A web3 driver's contracts must not collect a foundry.toml they never needed."""
    contracts = tmp_path / "contracts"
    contracts.mkdir()
    (contracts / "Plain.sol").write_text(
        "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.20;\ncontract Plain {}\n"
    )
    prepared = prepare_project(str(contracts), assume_yes=True, needs_forge_std=False)
    assert prepared.may_install is True
    assert not os.path.exists(contracts / "foundry.toml")
    assert not os.path.exists(contracts / "lib")


def test_a_sol_target_gets_forge_std_even_if_it_imports_nothing(
    tmp_path, local_forge_std
):
    root = tmp_path / "scratch"
    root.mkdir()
    sol = root / "Bare.t.sol"
    sol.write_text(
        "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.20;\ncontract BareTest {}\n"
    )
    prepared = prepare_project(str(sol), assume_yes=True)
    assert prepared.missing == ("forge-std",)
    compile_foundry_project(root=str(root), ensure_forge_std=True)
    assert os.path.isdir(root / "lib" / "forge-std")


def test_a_src_test_layout_gets_the_standard_config(tmp_path, local_forge_std):
    root = tmp_path / "proj"
    (root / "src").mkdir(parents=True)
    (root / "test").mkdir()
    (root / "test" / "T.t.sol").write_text("pragma solidity ^0.8.20;\ncontract T {}\n")
    prepared = prepare_project(str(root / "test" / "T.t.sol"), assume_yes=True)
    # The project root, not test/, is where foundry.toml and lib/ belong.
    assert prepared.root == str(root)
    toml = read_text(os.path.join(str(root), "foundry.toml"))
    assert 'src = "src"' in toml and 'test = "test"' in toml


def test_no_answer_declines(tmp_path, monkeypatch):
    root = _standalone(tmp_path)
    monkeypatch.setattr("sys.stdin.isatty", lambda: True)

    def eof(_prompt):
        raise EOFError

    monkeypatch.setattr("builtins.input", eof)
    prepared = prepare_project(os.path.join(root, "Demo.t.sol"), assume_yes=False)
    assert prepared.may_install is False and prepared.declined is True


def test_declining_writes_nothing(tmp_path, monkeypatch):
    root = _standalone(tmp_path)
    monkeypatch.setattr("sys.stdin.isatty", lambda: True)
    monkeypatch.setattr("builtins.input", lambda _prompt: "n")
    prepared = prepare_project(os.path.join(root, "Demo.t.sol"), assume_yes=False)
    assert prepared.may_install is False
    assert not os.path.exists(os.path.join(root, "foundry.toml"))
    assert not os.path.exists(os.path.join(root, "lib"))


def test_non_tty_declines(tmp_path, monkeypatch):
    root = _standalone(tmp_path)
    monkeypatch.setattr("sys.stdin.isatty", lambda: False)
    prepared = prepare_project(os.path.join(root, "Demo.t.sol"), assume_yes=False)
    assert prepared.may_install is False
    assert not os.path.exists(os.path.join(root, "foundry.toml"))


def test_no_install_error_names_the_command(tmp_path):
    root = _standalone(tmp_path)
    with pytest.raises(CompileError) as exc:
        compile_foundry_project(
            root,
            target_file=os.path.join(root, "Demo.t.sol"),
            install_missing=False,
            ensure_forge_std=True,
        )
    assert "forge install" in str(exc.value)
    assert "forge-std/Test.sol" in str(exc.value)


def test_missing_relative_import_is_not_an_install(tmp_path, local_forge_std):
    root = tmp_path / "scratch"
    root.mkdir()
    (root / "A.sol").write_text('pragma solidity ^0.8.20;\nimport "./Gone.sol";\n')
    with pytest.raises(CompileError, match=re.escape("no such import './Gone.sol'")):
        compile_foundry_project(str(root), ensure_forge_std=False)


def test_standalone_end_to_end_install_and_compile(tmp_path, local_forge_std):
    root = _standalone(tmp_path)
    notices: list[str] = []
    prepare_project(os.path.join(root, "Demo.t.sol"), assume_yes=True)
    project = compile_foundry_project(
        root,
        target_file=os.path.join(root, "Demo.t.sol"),
        ensure_forge_std=True,
        on_notice=notices.append,
    )
    assert project.artifact("DemoTest") is not None
    assert "forge-std/=lib/forge-std/src/" in project.remappings
    # What sevm resolved is written down, so `forge` resolves the project the same way.
    written = read_text(os.path.join(root, "remappings.txt")).splitlines()
    assert "forge-std/=lib/forge-std/src/" in written
    assert any("installing forge-std" in n for n in notices)


def test_a_librarys_own_tests_never_reach_solc(solo_project):
    keys = set(solo_project.sources)
    assert "lib/forge-std/src/Test.sol" in keys
    # Unlinked.t.sol lives in the fake forge-std's test/ dir and compiles to a link
    # placeholder; the closure must not pull it in.
    assert not any(k.startswith("lib/forge-std/test/") for k in keys)
    assert all(a.name != "UsesUnlinkable" for a in solo_project.artifacts.values())


def test_library_contracts_are_not_test_targets(solo_project):
    contracts = {t.contract for t in discover_tests(solo_project)}
    assert contracts == {"AllCheatsTest"}


def test_unlinked_bytecode_does_not_break_the_project():
    out = {
        "sources": {"A.sol": {"id": 0}},
        "contracts": {
            "A.sol": {
                "Linked": {
                    "abi": [],
                    "evm": {
                        "bytecode": {"object": "60ff"},
                        "deployedBytecode": {"object": "60ff"},
                    },
                },
                "Unlinked": {
                    "abi": [],
                    "evm": {
                        "bytecode": {"object": "73__$aa$__6000"},
                        "deployedBytecode": {"object": "73__$aa$__6000"},
                    },
                },
            }
        },
    }
    project = _build_project(out, {}, "0.8.28", False)
    assert project.artifacts["A.sol:Linked"].bytecode == b"\x60\xff"
    assert project.artifacts["A.sol:Unlinked"].bytecode == b""


# ==================================================================
# the .py driver path gets the same treatment
# ==================================================================


def test_python_run_resolves_libraries_and_enables_cheatcodes(
    tmp_path, local_forge_std, monkeypatch
):
    import sevm.cli as cli

    contracts = tmp_path / "contracts"
    contracts.mkdir()
    (contracts / "Logger.sol").write_text(
        "// SPDX-License-Identifier: MIT\n"
        "pragma solidity ^0.8.20;\n"
        'import {console} from "forge-std/console.sol";\n'
        "contract Logger {\n"
        "    function ping() external view returns (uint256) {\n"
        "        console.log('ping at', block.timestamp);\n"
        "        return block.timestamp;\n"
        "    }\n"
        "}\n"
    )
    script = tmp_path / "drive.py"
    script.write_text("pass\n")

    captured: dict = {}

    def fake_debug(console, project, target, args, foundry_mode, stop_functions=None):
        captured["project"] = project
        captured["foundry_mode"] = foundry_mode
        return 0

    monkeypatch.setattr(cli, "_debug", fake_debug)
    assert (
        cli.main(
            ["run", "--yes", "--contracts", str(contracts), str(script)],
        )
        == 0
    )
    project = captured["project"]
    assert project.artifact("Logger") is not None
    assert any("forge-std/=" in r for r in project.remappings)
    # console.log and vm.* are intercepted for a .py driver too.
    assert captured["foundry_mode"] is True
    assert os.path.isdir(tmp_path / "contracts" / "lib" / "forge-std")


# ==================================================================
# the real thing (opt-in)
# ==================================================================


@NETWORK
def test_real_forge_std_installs_and_runs(tmp_path):
    from sevm.evaluate import Evaluator, make_eval_hook
    from sevm.foundry import compile_test, make_test_driver, select_test
    from sevm.session import DebugSession, Finished, StepMode

    root = str(tmp_path / "solo")
    shutil.copytree(os.path.join(os.path.dirname(FIXTURES), "foundry_solo"), root)
    sol = os.path.join(root, "AllCheats.t.sol")
    project = compile_test(sol, root)

    target = select_test(discover_tests(project), match="testPrankValue")
    session = DebugSession(project)
    session.foundry_mode = True
    session.set_eval_hook(make_eval_hook(Evaluator(project)))
    session.start(make_test_driver(project, target))
    event = session.wait(timeout=60)
    for _ in range(400):
        if isinstance(event, Finished):
            break
        event = session.resume(StepMode.RUN, count=1, timeout=60)
    try:
        session.detach(timeout=30)
    except Exception:
        session.uninstall()
    assert isinstance(event, Finished) and event.ok, session.exit_error


@NETWORK
def test_real_forge_std_declares_every_assert_we_implement(tmp_path):
    from eth_utils import function_signature_to_4byte_selector

    from sevm.cheatcodes import _REGISTRY

    root = str(tmp_path / "std")
    libs.clone(
        libs.ALIASES["forge-std"], libs.newest_tag(libs.ALIASES["forge-std"]), root
    )
    text = read_text(os.path.join(root, "src", "Vm.sol"))
    declared = [
        f"{m.group(1)}({','.join(p.strip().split()[0] for p in m.group(2).split(',') if p.strip())})"
        for m in re.finditer(r"function\s+(assert[A-Za-z]*)\s*\(([^)]*)\)", text, re.S)
    ]
    missing = [
        sig
        for sig in declared
        if function_signature_to_4byte_selector(sig) not in _REGISTRY
    ]
    assert not missing, (
        f"forge-std asserts sevm does not implement: {sorted(set(missing))}"
    )


@NETWORK
def test_real_openzeppelin_import_installs_itself(tmp_path):
    root = str(tmp_path / "proj")
    os.makedirs(root)
    with open(os.path.join(root, "Vault.sol"), "w", encoding="utf-8") as fh:
        fh.write(
            "// SPDX-License-Identifier: MIT\n"
            "pragma solidity ^0.8.20;\n"
            'import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";\n'
            "contract Vault {\n"
            "    function balance(IERC20 token) external view returns (uint256) {\n"
            "        return token.balanceOf(address(this));\n"
            "    }\n"
            "}\n"
        )
    project = compile_foundry_project(root, ensure_forge_std=False)
    assert project.artifact("Vault") is not None
    assert (
        "@openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/"
        in project.remappings
    )
    assert os.path.isfile(
        os.path.join(
            root,
            "lib",
            "openzeppelin-contracts",
            "contracts",
            "token",
            "ERC20",
            "IERC20.sol",
        )
    )


@NETWORK
def test_npm_resolves_a_real_scoped_package():
    assert libs.npm_repo_url("@openzeppelin/contracts") == (
        "https://github.com/OpenZeppelin/openzeppelin-contracts"
    )
