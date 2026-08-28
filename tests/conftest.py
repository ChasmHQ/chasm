"""Fixtures every test file shares.

The debugger fixtures each get a *fresh* chain, because stepping mutates state and a shared
one would make failures order-dependent. Compilation is cached process-wide (see
`harness.project`) since solc is the slow part.

The rest stands up the dependency-install path: sevm fetches forge-std from its real
repository, so the suite builds a local git repo from `fixtures/forge_std_fake/` and points
the resolver at it over `file://`.

The real clone, tag-selection and remapping-derivation code runs; nothing touches the
network.
"""

from __future__ import annotations

import os
import shutil
import subprocess

import pytest
from eth_abi import encode as abi_encode
from eth_utils import function_signature_to_4byte_selector
from harness import Debugger, bank_fixture, deploy, make_web3, project

from sevm import libs

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

# Tags planted on the fake repo. v2.0.0-rc.1 is newer than v1.2.3 and must never be picked.
FAKE_TAGS = ("v0.9.0", "v1.2.3", "v2.0.0-rc.1")
FAKE_NEWEST = "v1.2.3"


def pytest_collection_modifyitems(config, items):
    """Skip `network`-marked tests unless SEVM_NETWORK_TESTS=1 asks for the real thing."""
    if os.environ.get("SEVM_NETWORK_TESTS") == "1":
        return
    skip = pytest.mark.skip(
        reason="set SEVM_NETWORK_TESTS=1 to run tests that hit the network"
    )
    for item in items:
        if "network" in item.keywords:
            item.add_marker(skip)


@pytest.fixture(autouse=True)
def isolated_cache(tmp_path, monkeypatch):
    """Point the user-level cache at tmp for every test.

    Without this a test would read the developer's real `~/.cache/sevm/solc-versions.json`
    and quietly ignore its own mocked release list, and the suite would leave entries
    behind on the machine that ran it.
    """
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "xdg"))


def _git(*args: str, cwd: str) -> None:
    subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        env={
            **os.environ,
            "GIT_AUTHOR_NAME": "sevm tests",
            "GIT_AUTHOR_EMAIL": "tests@example.com",
            "GIT_COMMITTER_NAME": "sevm tests",
            "GIT_COMMITTER_EMAIL": "tests@example.com",
        },
    )


def make_repo(source_dir: str, dest: str, tags: tuple[str, ...] = FAKE_TAGS) -> str:
    """Copy a tree into a fresh git repo with `tags` on it; returns a clonable file:// URL."""
    shutil.copytree(source_dir, dest)
    _git("init", "-q", "-b", "main", cwd=dest)
    _git("add", "-A", cwd=dest)
    _git("commit", "-q", "-m", "fixture", cwd=dest)
    for tag in tags:
        _git("tag", tag, cwd=dest)
    return f"file://{dest}"


@pytest.fixture(scope="session")
def fake_forge_std_url(tmp_path_factory) -> str:
    """A local git repo shaped like forge-std, tagged so tag selection is exercised."""
    dest = str(tmp_path_factory.mktemp("repos") / "forge-std")
    return make_repo(os.path.join(FIXTURES, "forge_std_fake"), dest)


@pytest.fixture(scope="session")
def local_forge_std(fake_forge_std_url):
    """Resolve `forge-std` to the local repo for the whole session."""
    saved = libs.ALIASES["forge-std"]
    libs.ALIASES["forge-std"] = fake_forge_std_url
    yield fake_forge_std_url
    libs.ALIASES["forge-std"] = saved


# ---- compiled Foundry fixtures --------------------------------------------
#
# Session-scoped: solc dominates the suite's runtime, and each project is compiled once.


@pytest.fixture(scope="session")
def solo_root(tmp_path_factory, local_forge_std) -> str:
    """tests/foundry_solo copied out of the repo, so its forge-std install lands in tmp."""
    dest = str(tmp_path_factory.mktemp("solo") / "foundry_solo")
    shutil.copytree(os.path.join(HERE, "foundry_solo"), dest)
    return dest


@pytest.fixture(scope="session")
def solo_project(solo_root):
    from sevm.foundry import compile_test

    return compile_test(os.path.join(solo_root, "AllCheats.t.sol"), solo_root)


@pytest.fixture(scope="session")
def token_root(tmp_path_factory) -> str:
    dest = str(tmp_path_factory.mktemp("token") / "foundry_project")
    shutil.copytree(os.path.join(HERE, "foundry_project"), dest)
    return dest


@pytest.fixture(scope="session")
def token_project(token_root):
    from sevm.compile import DEFAULT_SOLC_VERSION, compile_foundry_project

    return compile_foundry_project(
        token_root,
        target_file=os.path.join(token_root, "test", "Token.t.sol"),
        solc_version=DEFAULT_SOLC_VERSION,
        install_missing=False,
    )


@pytest.fixture(scope="session")
def failing_project(tmp_path_factory, local_forge_std):
    """A standalone test whose assertion fails, to prove failures surface as reverts."""
    from sevm.foundry import compile_test, prepare_project

    root = str(tmp_path_factory.mktemp("failing"))
    sol = os.path.join(root, "Demo.t.sol")
    with open(sol, "w", encoding="utf-8") as fh:
        fh.write(
            "// SPDX-License-Identifier: MIT\n"
            "pragma solidity ^0.8.20;\n"
            'import {Test} from "forge-std/Test.sol";\n'
            "contract DemoTest is Test {\n"
            "    function testFails() public pure {\n"
            "        assertEq(uint256(1), uint256(2));\n"
            "    }\n"
            "}\n"
        )
    prepare_project(sol, assume_yes=True)
    return compile_test(sol, root)


# ==================================================================
# a live debug session
# ==================================================================


@pytest.fixture(scope="module")
def proj():
    return project()


@pytest.fixture
def bank():
    """Fresh chain with Bank + Callee deployed and a funded second account."""
    w3, proj_, contract, callee, alice = bank_fixture()
    yield w3, proj_, contract, callee, alice


@pytest.fixture
def deposit_debugger(bank):
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    yield dbg
    dbg.close()


@pytest.fixture
def forward_debugger(bank):
    """Stopped in `forward(address,uint256)`, a frame whose calldata carries arguments.

    Yields the debugger and the calldata the transaction was sent with, computed here
    rather than read back off the frame so the assertions have something to compare to.
    """
    w3, proj_, contract, callee, _alice = bank
    calldata = function_signature_to_4byte_selector(
        "forward(address,uint256)"
    ) + abi_encode(["address", "uint256"], [callee.address, 21])

    def txfn():
        tx = contract.functions.forward(callee.address, 21).transact({"gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    yield dbg, calldata
    dbg.close()


@pytest.fixture
def locals_contract(proj):
    """A fresh chain with `Locals` deployed, for the hard shapes."""
    w3 = make_web3()
    return w3, proj, deploy(w3, proj, "Locals")


@pytest.fixture
def fake_clipboard(monkeypatch):
    """Capture what would reach the system clipboard, without touching it."""
    from sevm import clipboard

    captured: list = []

    def fake_copy(text: str) -> str:
        captured.append(text)
        return "pbcopy"

    monkeypatch.setattr(clipboard, "copy", fake_copy)
    return captured
