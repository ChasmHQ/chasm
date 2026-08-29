"""The test harness: compile the contracts, stand up a chain, drive a debug session.

`Debugger` is what most tests hold: a started session plus its command processor, so a
test can say `dbg.run("b Bank.sol:46")` and assert on the result.
"""

from __future__ import annotations

import os
from typing import Any

from eth_account import Account
from web3 import EthereumTesterProvider, Web3

from sevm.commands import CommandProcessor
from sevm.compile import DEFAULT_SOLC_VERSION, Project, compile_project
from sevm.evaluate import Evaluator, make_eval_hook
from sevm.session import DebugSession, StepMode
from sevm.srcmap import build_line_indexes

# Generous: a stuck VM thread should fail the test, not hang the suite.
TIMEOUT = 30.0

CONTRACTS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "contracts")

_project_cache: dict[str, Project] = {}


def project() -> Project:
    """Compile tests/contracts once per process; solc is the slow part.

    Pinned to DEFAULT_SOLC_VERSION on purpose: the suite asserts on source maps and stack
    layouts, so it must not float to whatever the newest pragma-compatible release happens
    to be (nor download one mid-test). CLI runs auto-detect; this fixture does not.
    """
    if "p" not in _project_cache:
        _project_cache["p"] = compile_project(
            [CONTRACTS_DIR], solc_version=DEFAULT_SOLC_VERSION
        )
    return _project_cache["p"]


def make_web3() -> Web3:
    w3 = Web3(EthereumTesterProvider())
    w3.eth.default_account = w3.eth.accounts[0]
    return w3


def deploy(w3: Web3, proj: Project, name: str, *args: Any, value_wei: int = 0) -> Any:
    art = proj.artifact(name)
    assert art is not None, f"no artifact named {name}"
    contract = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())
    tx = contract.constructor(*args).transact(
        {"from": w3.eth.default_account, "value": value_wei, "gas": 3_000_000}
    )
    receipt = w3.eth.wait_for_transaction_receipt(tx)
    return w3.eth.contract(address=receipt["contractAddress"], abi=art.abi)


def funded_account(w3: Web3, ether: int = 10) -> Any:
    acct = Account.create()
    w3.eth.send_transaction(
        {
            "from": w3.eth.accounts[0],
            "to": acct.address,
            "value": w3.to_wei(ether, "ether"),
            "gas": 21000,
        }
    )
    w3.provider.ethereum_tester.backend.add_account(acct.key)
    return acct


def bank_fixture(value_ether: int = 1) -> tuple[Web3, Project, Any, Any, Any]:
    """A deployed Bank plus Callee and a funded second account."""
    proj = project()
    w3 = make_web3()
    bank = deploy(
        w3, proj, "Bank", "sevm-bank", value_wei=w3.to_wei(value_ether, "ether")
    )
    callee = deploy(w3, proj, "Callee")
    alice = funded_account(w3)
    return w3, proj, bank, callee, alice


class Debugger:
    """A started session plus the command processor, torn down cleanly."""

    def __init__(self, proj, txfn, **session_kwargs):
        self.session = DebugSession(proj, **session_kwargs)
        self.evaluator = Evaluator(proj)
        self.session.set_eval_hook(make_eval_hook(self.evaluator))
        self.commands = CommandProcessor(self.session, self.evaluator)
        # Bind a restart target so `reset` / `run` work under test as via `sevm run`.
        self.session.set_restart_factory(lambda argv: txfn, [])
        self.session.start(txfn)
        self.first = self.session.wait(timeout=TIMEOUT)

    def step(self, mode=StepMode.STEP, count=1):
        return self.session.resume(mode, count=count, timeout=TIMEOUT)

    def run(self, line):
        return self.commands.execute(line)

    @property
    def snap(self):
        return self.session.last_snapshot

    def close(self):
        try:
            self.session.detach(timeout=TIMEOUT)
        except Exception:
            self.session.uninstall()


def line_indexes(proj):
    return build_line_indexes(proj.sources.values())


def line_of(proj, needle):
    """The 1-based line in Bank.sol containing `needle`, so tests name code not numbers."""
    for n, text in enumerate(proj.sources["Bank.sol"].text.split("\n"), start=1):
        if needle in text:
            return n
    raise AssertionError(f"{needle!r} not found in Bank.sol")


def locals_debugger(w3, proj_, contract, function, *args, gas=900_000):
    def txfn():
        tx = getattr(contract.functions, function)(*args).transact({"gas": gas})
        w3.eth.wait_for_transaction_receipt(tx)

    return Debugger(proj_, txfn)


def stop_at(dbg, line, contract="Locals.sol"):
    dbg.run(f"b {contract}:{line}")
    result = dbg.run("c")
    assert result.ok, result.error
    return result


def locals_map(dbg):
    """`info locals` as {name: value text}, with markup stripped."""
    out = {}
    for row in dbg.commands.read_locals():
        out[row["name"]] = row["value"] if row["available"] else "<unavailable>"
    return out
