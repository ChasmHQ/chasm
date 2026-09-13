"""Python Web3 drivers execute on the REVM-backed provider."""

from __future__ import annotations

from web3 import Web3

from sevm.provider import RevmWeb3Driver
from sevm.session import Finished, Paused, RevmDebugSession, StepMode

TIMEOUT = 30.0


def test_unmodified_tester_provider_script_runs_on_revm(token_project):
    artifact = token_project.artifact("Token")
    observed = {}

    def script() -> None:
        from web3 import EthereumTesterProvider, Web3

        web3 = Web3(EthereumTesterProvider())
        web3.eth.default_account = web3.eth.accounts[0]
        factory = web3.eth.contract(abi=artifact.abi, bytecode=artifact.bytecode.hex())
        tx_hash = factory.constructor().transact({"gas": 3_000_000})
        receipt = web3.eth.wait_for_transaction_receipt(tx_hash)
        token = web3.eth.contract(address=receipt.contractAddress, abi=artifact.abi)

        tx_hash = token.functions.mint(
            "0x0000000000000000000000000000000000000B0b", 100
        ).transact({"gas": 300_000})
        observed["receipt"] = web3.eth.wait_for_transaction_receipt(tx_hash)
        observed["balance"] = token.functions.balanceOf(
            "0x0000000000000000000000000000000000000B0b"
        ).call()
        observed["code"] = web3.eth.get_code(receipt.contractAddress)

    session = RevmDebugSession(token_project, stop_at_start=False)
    session.start(RevmWeb3Driver(script))
    event = session.wait(timeout=TIMEOUT)

    assert isinstance(event, Finished)
    assert event.ok, session.exit_error
    assert observed["receipt"].status == 1
    assert observed["balance"] == 100
    assert observed["code"] == artifact.deployed_bytecode


def test_revm_provider_supports_plain_value_transfers(token_project):
    recipient = "0x0000000000000000000000000000000000001234"
    observed = {}

    def script() -> None:
        from web3 import EthereumTesterProvider, Web3

        web3 = Web3(EthereumTesterProvider())
        tx_hash = web3.eth.send_transaction(
            {
                "from": web3.eth.accounts[0],
                "to": recipient,
                "value": 42,
                "gas": 21_000,
            }
        )
        observed["receipt"] = web3.eth.wait_for_transaction_receipt(tx_hash)
        observed["balance"] = web3.eth.get_balance(recipient)

    session = RevmDebugSession(token_project, stop_at_start=False)
    session.start(RevmWeb3Driver(script))
    event = session.wait(timeout=TIMEOUT)

    assert isinstance(event, Finished)
    assert event.ok, session.exit_error
    assert observed["receipt"].status == 1
    assert observed["balance"] == 42
    assert Web3.is_checksum_address(observed["receipt"]["to"])


def test_revm_web3_driver_opens_at_recognised_solidity(token_project):
    artifact = token_project.artifact("Token")
    observed = {}

    def script() -> None:
        from web3 import EthereumTesterProvider, Web3

        web3 = Web3(EthereumTesterProvider())
        web3.eth.default_account = web3.eth.accounts[0]
        factory = web3.eth.contract(abi=artifact.abi, bytecode=artifact.bytecode.hex())
        tx_hash = factory.constructor().transact({"gas": 3_000_000})
        observed["receipt"] = web3.eth.wait_for_transaction_receipt(tx_hash)

    session = RevmDebugSession(token_project)
    session.start(RevmWeb3Driver(script))
    try:
        event = session.wait(timeout=TIMEOUT)
        assert isinstance(event, Paused)
        assert event.snapshot.contract_name == "Token"

        event = session.resume(StepMode.RUN, timeout=TIMEOUT)
        assert isinstance(event, Finished)
        assert event.ok
        assert observed["receipt"].status == 1
    finally:
        session.detach(timeout=TIMEOUT)
