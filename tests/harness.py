"""Shared fixtures: compile the test contracts, spin up an in-process chain, deploy."""

from __future__ import annotations

import os
from typing import Any

from eth_account import Account
from web3 import EthereumTesterProvider, Web3

from sevm.compile import Project, compile_project

CONTRACTS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "contracts")

_project_cache: dict[str, Project] = {}


def project() -> Project:
    """Compile tests/contracts once per process; solc is the slow part."""
    if "p" not in _project_cache:
        _project_cache["p"] = compile_project([CONTRACTS_DIR])
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
