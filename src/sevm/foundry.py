"""Foundry test entry point for `sevm run <Test.t.sol>`.

`forge test` is a Rust test runner. This is a minimal Python equivalent, just enough to
drop the debugger inside a test: resolve the project, then for each selected test do a fresh
deploy + `setUp()` + call to `testXxx()` as a transaction so sevm stops inside it. With a
breakpoint on every test body, the debugger opens at the first and `continue` steps to each
in turn. The transactions run on the same in-process Py-EVM chain the example scripts use,
so the existing session machinery attaches unchanged. Cheatcode calls originate from the
deployed test contract and are intercepted in the patched opcode loop (see cheatcodes.py).
"""

from __future__ import annotations

import os
import sys
from collections.abc import Callable, Sequence
from dataclasses import dataclass

from .compile import (
    DEFAULT_FOUNDRY_TOML,
    Project,
    compile_foundry_project,
    find_foundry_root,
)

# Base contracts bundled with sevm or ubiquitous in forge-std; never a test target.
_NON_TEST_CONTRACTS = frozenset({"Test", "Vm", "console", "console2", "StdAssertions"})


@dataclass(frozen=True)
class TestTarget:
    contract: str  # contract name, e.g. "CounterTest"
    function: str  # test function name, e.g. "testInc"
    has_setup: bool


def resolve_project(sol_path: str, assume_yes: bool = False) -> tuple[str, bool]:
    """Find the Foundry project root for `sol_path`.

    Returns (root, is_existing_project). If no foundry.toml is found, the file's directory
    becomes the root; the user is prompted to write a default foundry.toml (auto-yes with
    `assume_yes`). Either way compilation proceeds against the bundled forge-std.
    """
    root = find_foundry_root(sol_path)
    if root is not None:
        return root, True

    root = os.path.dirname(os.path.abspath(sol_path))
    toml_path = os.path.join(root, "foundry.toml")
    create = assume_yes
    if not assume_yes and sys.stdin.isatty():
        prompt = (
            f"No foundry.toml found. Create a default one at {toml_path}?\n"
            f"{DEFAULT_FOUNDRY_TOML}\n[y/N] "
        )
        try:
            create = input(prompt).strip().lower() in ("y", "yes")
        except EOFError:
            create = False
    if create and not os.path.exists(toml_path):
        with open(toml_path, "w", encoding="utf-8") as fh:
            fh.write(DEFAULT_FOUNDRY_TOML)
    return root, False


def compile_test(
    sol_path: str,
    root: str,
    *,
    solc_version: str | None = None,
    evm_version: str | None = None,
) -> Project:
    return compile_foundry_project(
        root,
        target_file=os.path.abspath(sol_path),
        solc_version=solc_version,
        evm_version=evm_version,
    )


def discover_tests(project: Project) -> list[TestTarget]:
    """Every no-argument `test*`/`invariant*` function across the project's contracts."""
    targets: list[TestTarget] = []
    for art in project.artifacts.values():
        if art.name in _NON_TEST_CONTRACTS:
            continue
        fns = [e for e in art.abi if e.get("type") == "function"]
        has_setup = any(e.get("name") == "setUp" for e in fns)
        for entry in fns:
            name = entry.get("name", "")
            if (
                name.startswith("test") or name.startswith("invariant")
            ) and not entry.get("inputs"):
                targets.append(
                    TestTarget(contract=art.name, function=name, has_setup=has_setup)
                )
    return targets


def select_tests(
    targets: list[TestTarget],
    match: str | None = None,
    match_contract: str | None = None,
) -> list[TestTarget]:
    """Filter targets by optional function/contract substrings. No filter selects all."""
    pool = targets
    if match_contract:
        pool = [t for t in pool if match_contract in t.contract]
    if match:
        pool = [t for t in pool if match in t.function]
    return pool


def select_test(
    targets: list[TestTarget],
    match: str | None = None,
    match_contract: str | None = None,
) -> TestTarget | None:
    """The first target matching the filters (or the first overall), else None."""
    pool = select_tests(targets, match, match_contract)
    return pool[0] if pool else None


def _run_one_test(w3: object, art: object, target: TestTarget) -> None:
    """Fresh deploy + setUp + the test call, as forge isolates each test."""
    factory = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())  # type: ignore[attr-defined]
    tx = factory.constructor().transact({"gas": 30_000_000})
    address = w3.eth.wait_for_transaction_receipt(tx)["contractAddress"]  # type: ignore[attr-defined]
    instance = w3.eth.contract(address=address, abi=art.abi)  # type: ignore[attr-defined]
    if target.has_setup:
        tx = instance.functions.setUp().transact({"gas": 30_000_000})
        w3.eth.wait_for_transaction_receipt(tx)  # type: ignore[attr-defined]
    tx = instance.functions[target.function]().transact({"gas": 30_000_000})
    w3.eth.wait_for_transaction_receipt(tx)  # type: ignore[attr-defined]


def make_test_driver(project: Project, target: TestTarget) -> Callable[[], None]:
    """Driver for a single test: deploy, setUp, then the test call."""
    return make_tests_driver(project, [target])


def make_tests_driver(
    project: Project, targets: Sequence[TestTarget]
) -> Callable[[], None]:
    """Driver that runs each test in turn (fresh deploy + setUp before each), so the
    debugger, with a breakpoint on every test body, stops at each one in sequence."""
    arts = [(project.artifact(t.contract), t) for t in targets]
    for art, t in arts:
        if art is None:
            raise ValueError(f"no artifact for test contract {t.contract!r}")

    def driver() -> None:
        from web3 import EthereumTesterProvider, Web3

        w3 = Web3(EthereumTesterProvider())
        w3.eth.default_account = w3.eth.accounts[0]
        for art, target in arts:
            _run_one_test(w3, art, target)

    return driver
