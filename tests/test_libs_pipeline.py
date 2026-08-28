"""The compile pipeline around dependency resolution.

Both entry points get the same treatment: a `.sol` target and a `.py` web3 driver resolve,
install and remap identically.
"""

from __future__ import annotations

import os
import re

import pytest

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
