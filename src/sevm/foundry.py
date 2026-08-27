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
from typing import Any

from .compile import (
    DEFAULT_FOUNDRY_TOML,
    STANDALONE_FOUNDRY_TOML,
    Project,
    compile_foundry_project,
    find_foundry_root,
    read_foundry_config,
    unresolved_prefixes,
)


@dataclass(frozen=True)
class TestTarget:
    contract: str  # contract name, e.g. "CounterTest"
    function: str  # test function name, e.g. "testInc"
    has_setup: bool


@dataclass(frozen=True)
class Prepared:
    """What `sevm run` may do to the target directory, after asking."""

    root: str
    existing: bool  # a foundry.toml was already there
    may_install: bool  # the user let sevm fetch missing libraries
    declined: bool = False  # asked, and the answer was no (or there was no terminal)
    missing: tuple[str, ...] = ()  # libraries that would have been installed


def _forge_std_present(root: str) -> bool:
    cfg = read_foundry_config(root)
    if any(r.split("=")[0].rstrip("/") == "forge-std" for r in cfg.remappings):
        return True
    return any(os.path.isdir(os.path.join(root, lib, "forge-std")) for lib in cfg.libs)


def _infer_root(start: str) -> str:
    """Root for a target with no foundry.toml above it.

    A file's own directory, unless a directory within a few levels up holds `src/` and
    `test/`: that is a Foundry project someone has not written a foundry.toml for yet, and
    `test/lib/forge-std` is not where it belongs.
    """
    here = start if os.path.isdir(start) else os.path.dirname(os.path.abspath(start))
    probe = here
    for _ in range(4):
        if os.path.isdir(os.path.join(probe, "src")) and os.path.isdir(
            os.path.join(probe, "test")
        ):
            return probe
        parent = os.path.dirname(probe)
        if not parent or parent == probe:
            break
        probe = parent
    return here


def prepare_project(
    path: str,
    *,
    assume_yes: bool = False,
    allow_install: bool = True,
    needs_forge_std: bool = True,
    source_dirs: Sequence[str] | None = None,
) -> Prepared:
    """Find the Foundry root for `path`, asking before writing anything into it.

    With no foundry.toml above it, the target's own directory becomes the root. Libraries
    the sources import but cannot reach are cloned into `lib/`. Both are gated by one
    prompt, which `assume_yes` skips; declining still runs, against what is on disk.

    A `.sol` target always needs forge-std, so it always gets a foundry.toml. A web3
    driver's contracts get one only when something has to be installed for them.
    """
    found = find_foundry_root(path)
    existing = found is not None
    root = found or _infer_root(path)

    missing: list[str] = []
    if allow_install:
        missing = unresolved_prefixes(root, source_dirs)
        if (
            needs_forge_std
            and not _forge_std_present(root)
            and "forge-std" not in missing
        ):
            missing.insert(0, "forge-std")

    write_toml = not existing and (needs_forge_std or bool(missing))
    planned = []
    if write_toml:
        planned.append(f"create {os.path.join(root, 'foundry.toml')}")
    if missing:
        libs = ", ".join(missing)
        planned.append(f"install {libs} into {os.path.join(root, 'lib')}")
    if not planned:
        return Prepared(
            root=root,
            existing=existing,
            may_install=allow_install,
            missing=tuple(missing),
        )

    approved = assume_yes
    if not approved and sys.stdin.isatty():
        prompt = "sevm will:\n" + "".join(f"  - {p}\n" for p in planned) + "[y/N] "
        try:
            approved = input(prompt).strip().lower() in ("y", "yes")
        except EOFError:
            approved = False
    if not approved:
        return Prepared(
            root=root,
            existing=existing,
            may_install=False,
            declined=True,
            missing=tuple(missing),
        )

    if write_toml:
        _write_default_toml(root)
    return Prepared(
        root=root, existing=existing, may_install=True, missing=tuple(missing)
    )


def _write_default_toml(root: str) -> None:
    """A project layout gets the standard src/test config; a lone file gets neither."""
    toml_path = os.path.join(root, "foundry.toml")
    if os.path.exists(toml_path):
        return
    has_layout = os.path.isdir(os.path.join(root, "src")) and os.path.isdir(
        os.path.join(root, "test")
    )
    body = DEFAULT_FOUNDRY_TOML if has_layout else STANDALONE_FOUNDRY_TOML
    with open(toml_path, "w", encoding="utf-8") as fh:
        fh.write(body)


def compile_test(
    sol_path: str,
    root: str,
    *,
    solc_version: str | None = None,
    evm_version: str | None = None,
    install_missing: bool = True,
    on_notice: Callable[[str], None] | None = None,
) -> Project:
    return compile_foundry_project(
        root,
        target_file=os.path.abspath(sol_path),
        solc_version=solc_version,
        evm_version=evm_version,
        install_missing=install_missing,
        ensure_forge_std=True,
        on_notice=on_notice,
    )


def discover_tests(project: Project, libs: Sequence[str] = ("lib",)) -> list[TestTarget]:
    """Every no-argument `test*`/`invariant*` function across the project's own contracts.

    Library sources are skipped wholesale: real forge-std brings ~15 base contracts, and a
    name list would have to chase every release.
    """
    prefixes = tuple(f"{lib.rstrip('/')}/" for lib in libs)
    targets: list[TestTarget] = []
    for art in project.artifacts.values():
        if art.source_key.startswith(prefixes):
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


class TestFailed(RuntimeError):
    """A test transaction reverted. forge would print a failure; sevm ends the run with one."""


def _receipt(w3: object, tx: object, what: str) -> Any:
    """Wait for a receipt and refuse a failed one.

    eth-tester reports a reverted transaction as `status = 0` rather than raising, so
    without this check a test whose assertion fails looks like a test that passed.
    """
    receipt = w3.eth.wait_for_transaction_receipt(tx)  # type: ignore[attr-defined]
    if receipt.get("status") == 0:
        raise TestFailed(f"{what} reverted")
    return receipt


def _run_one_test(w3: object, art: object, target: TestTarget) -> None:
    """Fresh deploy + setUp + the test call, as forge isolates each test."""
    name = f"{target.contract}.{target.function}"
    factory = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())  # type: ignore[attr-defined]
    tx = factory.constructor().transact({"gas": 30_000_000})
    address = _receipt(w3, tx, f"{target.contract} deployment")["contractAddress"]
    instance = w3.eth.contract(address=address, abi=art.abi)  # type: ignore[attr-defined]
    if target.has_setup:
        tx = instance.functions.setUp().transact({"gas": 30_000_000})
        _receipt(w3, tx, f"{target.contract}.setUp()")
    tx = instance.functions[target.function]().transact({"gas": 30_000_000})
    _receipt(w3, tx, name)


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
