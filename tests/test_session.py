"""Session lifecycle: install/uninstall, detach, and what survives a run."""

from __future__ import annotations

import pytest
from harness import (
    TIMEOUT,
    Debugger,
    line_of,
)

from sevm.session import DebugSession, Finished, Paused, SessionError, StepMode


def test_gas_estimation_is_not_debugged(bank):
    """A transaction without an explicit `gas=` must still stop exactly once.

    web3 calls eth_estimateGas, which binary-searches by RUNNING the transaction from the
    intrinsic gas upward, so the early probes fail with OutOfGas by design. Those passes
    run with the hook suspended; without that, the user sees a bogus out-of-gas inside a
    transaction that actually succeeds.
    """
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(1, "ether")}  # no gas= on purpose
        )
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        assert isinstance(dbg.first, Paused)
        assert dbg.snap.stop_reason != "error", dbg.snap.annotation
        assert dbg.snap.function.display_name == "Bank.deposit"
        line = line_of(proj_, "history.push(amount);")
        dbg.session.break_at_line("Bank.sol", line)
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        assert event.snapshot.line == line
        assert dbg.session.estimations > 0, "estimation passes should have been counted"
    finally:
        dbg.close()


def test_estimate_gas_patch_is_restored(bank):
    from eth.chains.base import Chain

    w3, proj_, contract, _callee, _alice = bank
    original = Chain.__dict__["estimate_gas"]

    def txfn():
        tx = contract.functions.deposit().transact({"value": 1, "gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    assert Chain.__dict__["estimate_gas"] is not original
    dbg.close()
    assert Chain.__dict__["estimate_gas"] is original


def test_ether_hint_only_for_plausible_wei(deposit_debugger):
    """An address cast to uint256 is not 721 billion billion ether."""
    dbg = deposit_debugger
    assert "ether" in dbg.session.inspect("evaluate", "1 ether").display
    assert (
        "ether" not in dbg.session.inspect("evaluate", "uint256(uint160(owner))").display
    )
    assert "ether" not in dbg.session.inspect("evaluate", "type(uint256).max").display
    assert "ether" not in dbg.session.inspect("evaluate", "feeBps").display


def test_debugs_inline_assembly_in_the_original_vault():
    """The article's Vault.sol: a breakpoint inside an `assembly { sstore(...) }` block."""
    import os as _os

    from eth_account import Account
    from web3 import EthereumTesterProvider, Web3

    from sevm.compile import DEFAULT_SOLC_VERSION
    from sevm.compile import compile_project as _compile

    here = _os.path.dirname(_os.path.abspath(__file__))
    vault_project = _compile(
        [_os.path.join(here, "contracts", "Vault.sol")], solc_version=DEFAULT_SOLC_VERSION
    )
    art = vault_project.artifact("Vault")
    assert art is not None

    w3 = Web3(EthereumTesterProvider())
    w3.eth.default_account = w3.eth.accounts[0]
    factory = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())
    tx = factory.constructor().transact(
        {"value": w3.to_wei(1, "ether"), "gas": 3_000_000}
    )
    address = w3.eth.wait_for_transaction_receipt(tx)["contractAddress"]
    vault = w3.eth.contract(address=address, abi=art.abi)
    attacker = Account.create()
    w3.eth.send_transaction(
        {"to": attacker.address, "value": w3.to_wei(1, "ether"), "gas": 21000}
    )
    w3.provider.ethereum_tester.backend.add_account(attacker.key)

    def txfn():
        t = vault.functions.unsafeStore(0, int(attacker.address, 16)).transact(
            {"from": attacker.address, "gas": 200_000}
        )
        w3.eth.wait_for_transaction_receipt(t)

    dbg = Debugger(vault_project, txfn)
    try:
        sstore_line = None
        for n, text in enumerate(
            vault_project.sources["Vault.sol"].text.split("\n"), start=1
        ):
            if "sstore(slot, value)" in text:
                sstore_line = n
        assert sstore_line
        dbg.session.break_at_line("Vault.sol", sstore_line)
        event = dbg.step(StepMode.RUN)
        assert isinstance(event, Paused)
        assert event.snapshot.line == sstore_line
        assert event.snapshot.function.name == "unsafeStore"
        # The arbitrary-storage-write bug, seen before it lands.
        assert dbg.run("info args").ok
        assert dbg.session.inspect("evaluate", "owner").value != attacker.address.lower()
        dbg.session.break_at_opcode("SSTORE")
        assert isinstance(dbg.step(StepMode.RUN), Paused)
        assert dbg.snap.stack[0].value == 0, "the SSTORE targets slot 0 (owner)"
    finally:
        dbg.close()


def test_patch_is_restored_after_detach(bank):
    from eth.vm.computation import BaseComputation

    w3, proj_, contract, _callee, _alice = bank
    original = BaseComputation.__dict__["apply_computation"]

    def txfn():
        tx = contract.functions.deposit().transact({"value": 1, "gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    assert BaseComputation.__dict__["apply_computation"] is not original
    dbg.close()
    assert BaseComputation.__dict__["apply_computation"] is original
    # And the chain still works untraced.
    assert contract.functions.totalDeposits().call() > 0


def test_inspect_after_finish_raises(bank):
    w3, proj_, contract, _callee, _alice = bank

    def txfn():
        tx = contract.functions.deposit().transact({"value": 1, "gas": 300_000})
        w3.eth.wait_for_transaction_receipt(tx)

    dbg = Debugger(proj_, txfn)
    try:
        dbg.step(StepMode.RUN)
        assert dbg.session.finished
        with pytest.raises(SessionError):
            dbg.session.inspect("read_storage", 0)
    finally:
        dbg.close()


def test_script_exception_is_reported_not_swallowed(proj):
    def txfn():
        raise ValueError("script blew up")

    session = DebugSession(proj)
    session.start(txfn)
    event = session.wait(timeout=TIMEOUT)
    assert isinstance(event, Finished)
    assert not event.ok and "script blew up" in event.error
    session.detach(timeout=TIMEOUT)
