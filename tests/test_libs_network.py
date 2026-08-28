"""The live-network equivalents, skipped unless SEVM_NETWORK_TESTS=1.

These reach the real forge-std, npm and openzeppelin repositories. One asserts sevm
implements every `assert*` the current forge-std declares, so a new overload upstream fails
the suite rather than surfacing as "unimplemented cheatcode" at run time.
"""

from __future__ import annotations

import os
import re
import shutil

import pytest
from conftest import FIXTURES

from sevm import libs
from sevm.compile import compile_foundry_project
from sevm.foundry import discover_tests


def read_text(path: str) -> str:
    with open(path, encoding="utf-8") as fh:
        return fh.read()


# Marked tests reach the real repositories; conftest skips them without
# SEVM_NETWORK_TESTS=1.
NETWORK = pytest.mark.network


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

    from sevm.cheatcodes.registry import _REGISTRY

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
