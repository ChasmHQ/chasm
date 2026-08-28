# Deploy Bank on an in-process chain and deposit into it.

import os

from eth_account import Account
from web3 import EthereumTesterProvider, Web3

from sevm.compile import compile_project

CONTRACTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "bank", "src")


def main():
    project = compile_project([CONTRACTS])
    w3 = Web3(EthereumTesterProvider())
    w3.eth.default_account = w3.eth.accounts[0]

    art = project.artifact("Bank")
    factory = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())
    tx = factory.constructor("sevm-bank").transact(
        {"value": w3.to_wei(1, "ether"), "gas": 3_000_000}
    )
    bank_address = w3.eth.wait_for_transaction_receipt(tx)["contractAddress"]
    bank = w3.eth.contract(address=bank_address, abi=art.abi)

    alice = Account.create()
    w3.eth.send_transaction(
        {"to": alice.address, "value": w3.to_wei(10, "ether"), "gas": 21000}
    )
    w3.provider.ethereum_tester.backend.add_account(alice.key)

    print("bank :", bank_address)
    print("alice:", alice.address)

    # The transaction the debugger will stop inside.
    tx = bank.functions.deposit().transact(
        {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
    )
    w3.eth.wait_for_transaction_receipt(tx)

    print("total deposits:", bank.functions.totalDeposits().call())
    print("alice balance :", bank.functions.balances(alice.address).call())


if __name__ == "__main__":
    main()
