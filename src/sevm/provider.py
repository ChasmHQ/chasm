"""Web3 provider and script adapter backed by the live REVM session."""

from __future__ import annotations

from collections.abc import Callable
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any

from eth_utils import keccak
from web3.providers import BaseProvider

DEFAULT_ACCOUNT = "0x2000000000000000000000000000000000000002"
ZERO_ADDRESS = "0x" + "00" * 20
ZERO_HASH = "0x" + "00" * 32


class _Backend:
    def add_account(self, _private_key: Any) -> None:
        pass


class _EthereumTesterFacade:
    def __init__(self) -> None:
        self.backend = _Backend()


class RevmProvider(BaseProvider):
    """Synchronous Web3 provider that executes requests on `RevmDebugSession`."""

    def __init__(self, session: Any) -> None:
        super().__init__()
        self.session = session
        self.ethereum_tester = _EthereumTesterFacade()
        self._request_id = 0
        self._transaction_index = 0
        self._receipts: dict[str, dict[str, Any]] = {}
        self._transactions: dict[str, dict[str, Any]] = {}
        self._logs: list[dict[str, Any]] = []

    def is_connected(self, show_traceback: bool = False) -> bool:
        return not self.session.finished

    def make_request(self, method: Any, params: Any) -> dict[str, Any]:
        self._request_id += 1
        try:
            result = self._dispatch(str(method), list(params))
        except Exception as exc:
            return self._error(-32000, f"{type(exc).__name__}: {exc}")
        return self._result(result)

    def _dispatch(self, method: str, params: list[Any]) -> Any:
        if method == "web3_clientVersion":
            return "sevm-revm/0.1.0"
        if method == "net_version":
            return str(self.session._chain.read_chain_id())
        if method == "eth_chainId":
            return _quantity(self.session._chain.read_chain_id())
        if method == "eth_accounts":
            return [DEFAULT_ACCOUNT]
        if method == "eth_blockNumber":
            return _quantity(self.session._chain.read_block_number())
        if method in {"eth_gasPrice", "eth_maxPriorityFeePerGas"}:
            return "0x0"
        if method in {"eth_getBlockByNumber", "eth_getBlockByHash"}:
            return self._block()
        if method == "eth_getBalance":
            return self.session._chain.read_balance(params[0])
        if method == "eth_getCode":
            return _hex_bytes(self.session._chain.read_code_at(params[0]))
        if method == "eth_getStorageAt":
            return _word(self.session._chain.read_storage_at(params[0], _int(params[1])))
        if method == "eth_getTransactionCount":
            return _quantity(self.session._chain.read_nonce(params[0]))
        if method == "eth_estimateGas":
            self.session.estimations += 1
            return _quantity(_int(params[0].get("gas", 30_000_000)))
        if method == "eth_sendTransaction":
            return self._send_transaction(params[0])
        if method == "eth_call":
            return self._call(params[0])
        if method == "eth_getTransactionReceipt":
            return self._receipts.get(str(params[0]).lower())
        if method == "eth_getTransactionByHash":
            return self._transactions.get(str(params[0]).lower())
        if method == "eth_getLogs":
            return list(self._logs)
        raise NotImplementedError(f"REVM provider does not implement {method}")

    def _send_transaction(self, transaction: dict[str, Any]) -> str:
        result = self._execute(transaction, commit=True)
        tx_hash = "0x" + keccak(self._transaction_index.to_bytes(32, "big")).hex()
        block_hash = _block_hash()
        logs = [
            {
                "address": item["address"],
                "topics": [_word(topic) for topic in item["topics"]],
                "data": _hex_bytes(item["data"]),
                "blockNumber": _quantity(self.session._chain.read_block_number()),
                "transactionHash": tx_hash,
                "transactionIndex": _quantity(self._transaction_index),
                "blockHash": block_hash,
                "logIndex": _quantity(index),
                "removed": False,
            }
            for index, item in enumerate(result.get("logs", []))
        ]
        receipt = {
            "transactionHash": tx_hash,
            "transactionIndex": _quantity(self._transaction_index),
            "blockHash": block_hash,
            "blockNumber": _quantity(self.session._chain.read_block_number()),
            "from": transaction.get("from", DEFAULT_ACCOUNT),
            "to": transaction.get("to"),
            "cumulativeGasUsed": _quantity(result.get("gas_used", 0)),
            "gasUsed": _quantity(result.get("gas_used", 0)),
            "contractAddress": result.get("created_address"),
            "logs": logs,
            "logsBloom": "0x" + "00" * 256,
            "status": "0x1" if result.get("success") else "0x0",
            "effectiveGasPrice": "0x0",
            "type": "0x2",
        }
        stored_transaction = {
            "hash": tx_hash,
            "nonce": "0x0",
            "blockHash": block_hash,
            "blockNumber": receipt["blockNumber"],
            "transactionIndex": receipt["transactionIndex"],
            "from": receipt["from"],
            "to": receipt["to"],
            "value": _quantity(transaction.get("value", 0)),
            "gas": _quantity(transaction.get("gas", 30_000_000)),
            "gasPrice": "0x0",
            "input": transaction.get("data", "0x"),
            "v": "0x0",
            "r": "0x0",
            "s": "0x0",
            "type": "0x2",
        }
        key = tx_hash.lower()
        self._receipts[key] = receipt
        self._transactions[key] = stored_transaction
        self._logs.extend(logs)
        self._transaction_index += 1
        return tx_hash

    def _call(self, transaction: dict[str, Any]) -> str:
        result = self._execute(transaction, commit=False)
        if not result.get("success"):
            data = _hex_bytes(result.get("output", b""))
            raise RuntimeError(f"execution reverted: {data}")
        return _hex_bytes(result.get("output", b""))

    def _execute(
        self,
        transaction: dict[str, Any],
        *,
        commit: bool,
    ) -> dict[str, Any]:
        return self.session.transact(
            to=transaction.get("to") or None,
            data=_bytes(transaction.get("data", transaction.get("input", "0x"))),
            caller=transaction.get("from", DEFAULT_ACCOUNT),
            value=_int(transaction.get("value", 0)),
            gas_limit=_int(transaction.get("gas", 30_000_000)),
            commit=commit,
        )

    def _block(self) -> dict[str, Any]:
        number = _quantity(self.session._chain.read_block_number())
        return {
            "number": number,
            "hash": _block_hash(),
            "parentHash": ZERO_HASH,
            "nonce": "0x" + "00" * 8,
            "sha3Uncles": ZERO_HASH,
            "logsBloom": "0x" + "00" * 256,
            "transactionsRoot": ZERO_HASH,
            "stateRoot": ZERO_HASH,
            "receiptsRoot": ZERO_HASH,
            "miner": ZERO_ADDRESS,
            "difficulty": self.session._chain.read_difficulty(),
            "totalDifficulty": self.session._chain.read_difficulty(),
            "extraData": "0x",
            "size": "0x0",
            "gasLimit": _quantity(30_000_000),
            "gasUsed": "0x0",
            "timestamp": self.session._chain.read_timestamp(),
            "transactions": [],
            "uncles": [],
            "baseFeePerGas": _quantity(self.session._chain.read_base_fee()),
        }

    def _result(self, result: Any) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": self._request_id, "result": result}

    def _error(self, code: int, message: str) -> dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": self._request_id,
            "error": {"code": code, "message": message},
        }


@contextmanager
def replace_tester_provider(session: Any):
    import web3
    import web3.providers.eth_tester

    original_root = web3.EthereumTesterProvider
    original_module = web3.providers.eth_tester.EthereumTesterProvider

    def factory(*_args: Any, **_kwargs: Any) -> RevmProvider:
        return RevmProvider(session)

    web3.EthereumTesterProvider = factory
    web3.providers.eth_tester.EthereumTesterProvider = factory
    try:
        yield
    finally:
        web3.EthereumTesterProvider = original_root
        web3.providers.eth_tester.EthereumTesterProvider = original_module


@dataclass(frozen=True)
class RevmWeb3Driver:
    target: Callable[[], None]
    fresh_chain = True

    def run_revm(self, session: Any) -> None:
        with replace_tester_provider(session):
            self.target()


def _int(value: Any) -> int:
    if isinstance(value, int):
        return value
    text = str(value)
    return int(text, 16) if text.startswith("0x") else int(text)


def _quantity(value: Any) -> str:
    return f"0x{_int(value):x}"


def _word(value: Any) -> str:
    return f"0x{_int(value):064x}"


def _bytes(value: Any) -> bytes:
    if isinstance(value, bytes):
        return value
    text = str(value)
    return bytes.fromhex(text[2:] if text.startswith("0x") else text)


def _hex_bytes(value: Any) -> str:
    return "0x" + bytes(value).hex()


def _block_hash() -> str:
    return "0x" + keccak(b"sevm-revm-block").hex()
