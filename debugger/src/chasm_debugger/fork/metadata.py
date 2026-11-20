from __future__ import annotations

from typing import Any

from .rpc import MetadataRpc
from .validation import (
    MAINNET_GENESIS,
    ForkError,
    access_list,
    address,
    cancun,
    data,
    hash32,
    integer,
    logs,
    native_request,
    obj,
    quantity,
    reject_newer,
    required,
    sequence,
)


def mainnet(rpc: MetadataRpc) -> None:
    if quantity(rpc.call("eth_chainId", []), "chainId") != 1:
        raise ForkError("only Ethereum mainnet chain ID 1 is supported")
    genesis = obj(rpc.call("eth_getBlockByNumber", ["0x0", False]), "genesis")
    if hash32(required(genesis, "hash"), "genesis hash") != MAINNET_GENESIS:
        raise ForkError("mainnet genesis hash mismatch")
    if quantity(required(genesis, "number"), "genesis number") != 0:
        raise ForkError("invalid genesis block number")


def block_identity(block: dict[str, Any]) -> dict[str, Any]:
    return {
        "chain_id": 1,
        "number": quantity(required(block, "number"), "block number", 2**64 - 1),
        "hash": hash32(required(block, "hash"), "block hash"),
        "genesis_hash": MAINNET_GENESIS,
    }


def block_context(block: dict[str, Any]) -> dict[str, Any]:
    reject_newer(block)
    identity = block_identity(block)
    timestamp = quantity(required(block, "timestamp"), "timestamp", 2**64 - 1)
    cancun(timestamp)
    result = {
        "number": identity["number"],
        "timestamp": timestamp,
        "beneficiary": address(required(block, "miner"), "miner"),
        "gas_limit": quantity(required(block, "gasLimit"), "block gasLimit", 2**64 - 1),
        "base_fee": hex(quantity(required(block, "baseFeePerGas"), "baseFeePerGas")),
        "prevrandao": hash32(required(block, "mixHash"), "mixHash"),
        "excess_blob_gas": quantity(
            required(block, "excessBlobGas"), "excessBlobGas", 2**64 - 1
        ),
    }
    used = quantity(required(block, "gasUsed"), "block gasUsed", 2**64 - 1)
    if not result["gas_limit"] or used > result["gas_limit"]:
        raise ForkError("invalid block gas accounting")
    blob_used = quantity(required(block, "blobGasUsed"), "blobGasUsed", 2**64 - 1)
    if blob_used > 6 * 131072 or blob_used % 131072:
        raise ForkError("invalid Cancun blob gas accounting")
    hash32(required(block, "parentBeaconBlockRoot"), "parentBeaconBlockRoot")
    return result


def pinned_block(rpc: MetadataRpc, block: dict[str, Any]) -> None:
    identity = block_identity(block)
    canonical = obj(
        rpc.call("eth_getBlockByNumber", [hex(identity["number"]), False]),
        "canonical block",
    )
    if block_identity(canonical) != identity:
        raise ForkError("block is no longer canonical at its claimed height")


def parent_block(rpc: MetadataRpc, block: dict[str, Any]) -> dict[str, Any]:
    expected_hash = hash32(required(block, "parentHash"), "parentHash")
    parent = obj(rpc.call("eth_getBlockByHash", [expected_hash, False]), "parent block")
    parent_id = block_identity(parent)
    if (
        parent_id["hash"] != expected_hash
        or parent_id["number"] + 1 != block_identity(block)["number"]
    ):
        raise ForkError("target block ancestry mismatch")
    if quantity(required(parent, "timestamp"), "parent timestamp") >= quantity(
        required(block, "timestamp"), "timestamp"
    ):
        raise ForkError("parent timestamp must precede target")
    pinned_block(rpc, parent)
    return parent


def transaction(
    tx: Any, block: dict[str, Any], index: int
) -> tuple[dict[str, Any], dict[str, Any]]:
    tx = obj(tx, "full transaction")
    reject_newer(tx)
    identity = {
        "transaction_hash": hash32(required(tx, "hash"), "transaction hash"),
        "block_hash": hash32(required(tx, "blockHash"), "transaction blockHash"),
        "block_number": quantity(
            required(tx, "blockNumber"), "transaction blockNumber", 2**64 - 1
        ),
        "transaction_index": quantity(
            required(tx, "transactionIndex"), "transactionIndex", 2**64 - 1
        ),
    }
    if (
        identity["block_hash"] != block["hash"]
        or identity["block_number"] != quantity(block["number"], "block number")
        or identity["transaction_index"] != index
    ):
        raise ForkError("transaction block/index identity mismatch")
    kind = quantity(required(tx, "type"), "transaction type", 3)
    if quantity(required(tx, "chainId"), "transaction chainId") != 1:
        raise ForkError("transaction chainId is not mainnet")
    dynamic = kind in (2, 3)
    for field in ("maxFeePerGas", "maxPriorityFeePerGas"):
        if not dynamic and field in tx:
            raise ForkError(f"unexpected {field} on transaction type {kind}")
    for field in ("blobVersionedHashes", "maxFeePerBlobGas"):
        if kind != 3 and field in tx:
            raise ForkError(f"unexpected {field} on non-blob transaction")
    if kind == 0 and "accessList" in tx and tx["accessList"] != []:
        raise ForkError("legacy transaction contains accessList")
    request = {
        "caller": address(required(tx, "from"), "from"),
        "to": None if required(tx, "to") is None else address(tx["to"], "to"),
        "data": data(required(tx, "input"), "input"),
        "value": hex(quantity(required(tx, "value"), "value")),
        "gas_limit": quantity(required(tx, "gas"), "gas", 2**64 - 1),
        "nonce": quantity(required(tx, "nonce"), "nonce", 2**64 - 1),
        "chain_id": 1,
        "tx_type": kind,
        "gas_price": hex(
            quantity(required(tx, "maxFeePerGas" if dynamic else "gasPrice"), "gas price")
        ),
        "max_priority_fee_per_gas": hex(
            quantity(required(tx, "maxPriorityFeePerGas"), "priority fee")
        )
        if dynamic
        else None,
        "access_list": access_list(required(tx, "accessList")) if kind else [],
        "blob_hashes": [
            hash32(item, "blob hash")
            for item in sequence(
                required(tx, "blobVersionedHashes"), "blobVersionedHashes"
            )
        ]
        if kind == 3
        else [],
        "max_fee_per_blob_gas": hex(
            quantity(required(tx, "maxFeePerBlobGas"), "blob fee")
        )
        if kind == 3
        else None,
        "commit": True,
    }
    return identity, native_request(request)


def receipt(
    value: Any,
    identity: dict[str, Any],
    request: dict[str, Any],
    prior_gas: int,
    prior_logs: int = 0,
) -> dict[str, Any]:
    value = obj(value, "receipt")
    for field, key, parser in (
        ("transactionHash", "transaction_hash", hash32),
        ("blockHash", "block_hash", hash32),
        ("blockNumber", "block_number", quantity),
        ("transactionIndex", "transaction_index", quantity),
    ):
        if parser(required(value, field), field) != identity[key]:
            raise ForkError(f"receipt {field} identity mismatch")
    if quantity(required(value, "type"), "receipt type") != request["tx_type"]:
        raise ForkError("receipt transaction type mismatch")
    if (
        address(required(value, "from"), "receipt from") != request["caller"]
        or required(value, "to") != request["to"]
    ):
        raise ForkError("receipt sender/destination identity mismatch")
    status = quantity(required(value, "status"), "receipt status", 1)
    used = quantity(required(value, "gasUsed"), "receipt gasUsed", 2**64 - 1)
    cumulative = quantity(
        required(value, "cumulativeGasUsed"), "cumulativeGasUsed", 2**64 - 1
    )
    if used > request["gas_limit"] or cumulative != prior_gas + used:
        raise ForkError("receipt gas accounting mismatch")
    created = required(value, "contractAddress")
    if created is not None:
        address(created, "contractAddress")
    if (request["to"] is not None or status == 0) and created is not None:
        raise ForkError("unexpected contractAddress in receipt")
    if request["to"] is None and status == 1 and created is None:
        raise ForkError("successful creation receipt has no contractAddress")
    result_logs = logs(required(value, "logs"))
    if status == 0 and result_logs:
        raise ForkError("reverted receipt contains logs")
    for index, log in enumerate(value["logs"], start=prior_logs):
        for field, key, parser in (
            ("transactionHash", "transaction_hash", hash32),
            ("blockHash", "block_hash", hash32),
            ("blockNumber", "block_number", quantity),
            ("transactionIndex", "transaction_index", quantity),
        ):
            if parser(required(log, field), field) != identity[key]:
                raise ForkError("receipt log identity mismatch")
        if required(log, "removed") is not False:
            raise ForkError("receipt contains removed log")
        if quantity(required(log, "logIndex"), "logIndex") != index:
            raise ForkError("receipt logIndex order mismatch")
    return {
        "status": status,
        "gas_used": used,
        "cumulative_gas_used": cumulative,
        "logs": result_logs,
        "contract_address": created,
        "identity": identity,
    }


def replay_metadata(rpc: MetadataRpc, tx_hash: str) -> dict[str, Any]:
    mainnet(rpc)
    target_tx = obj(rpc.call("eth_getTransactionByHash", [tx_hash]), "target transaction")
    if hash32(required(target_tx, "hash"), "transaction hash") != tx_hash:
        raise ForkError("requested transaction hash mismatch")
    target_hash = hash32(required(target_tx, "blockHash"), "target blockHash")
    block = obj(rpc.call("eth_getBlockByHash", [target_hash, True]), "target block")
    if block_identity(block)["hash"] != target_hash:
        raise ForkError("requested block hash mismatch")
    context = block_context(block)
    pinned_block(rpc, block)
    parent = parent_block(rpc, block)
    index = quantity(
        required(target_tx, "transactionIndex"), "target transactionIndex", 2**64 - 1
    )
    transactions = sequence(required(block, "transactions"), "block transactions")
    if index >= len(transactions):
        raise ForkError("target index is outside block transactions")
    records = []
    cumulative = 0
    seen: set[str] = set()
    blob_count = 0
    log_count = 0
    for offset, tx in enumerate(transactions[: index + 1]):
        identity, request = transaction(tx, block, offset)
        if identity["transaction_hash"] in seen:
            raise ForkError("duplicate transaction in ordered prefix")
        seen.add(identity["transaction_hash"])
        if request["gas_limit"] > context["gas_limit"] - cumulative:
            raise ForkError("transaction gas limit exceeds remaining block gas pool")
        blob_count += len(request["blob_hashes"])
        if blob_count > 6:
            raise ForkError("prefix exceeds Cancun block blob gas pool")
        expected = receipt(
            rpc.call("eth_getTransactionReceipt", [identity["transaction_hash"]]),
            identity,
            request,
            cumulative,
            log_count,
        )
        cumulative = expected["cumulative_gas_used"]
        log_count += len(expected["logs"])
        records.append({"identity": identity, "request": request, "receipt": expected})
    if transaction(target_tx, block, index) != (
        records[-1]["identity"],
        records[-1]["request"],
    ):
        raise ForkError("target transaction differs from full block transaction")
    if cumulative > quantity(block["gasUsed"], "block gasUsed"):
        raise ForkError("prefix cumulative gas exceeds block gasUsed")
    if index == len(transactions) - 1 and cumulative != quantity(
        block["gasUsed"], "block gasUsed"
    ):
        raise ForkError("final receipt cumulative gas differs from block gasUsed")
    pinned_block(rpc, block)
    return {
        "kind": "replay",
        "anchor": block_identity(parent),
        "parent_header": parent_header(parent),
        "target_header": target_header(block),
        "block": context,
        "beacon_root": hash32(block["parentBeaconBlockRoot"], "parentBeaconBlockRoot"),
        "prefix": records[:-1],
        "target_record": records[-1],
    }


def fork_metadata(rpc: MetadataRpc, block_number: int) -> dict[str, Any]:
    mainnet(rpc)
    block = obj(
        rpc.call(
            "eth_getBlockByNumber", [hex(integer(block_number, "block_number")), False]
        ),
        "fork block",
    )
    if block_identity(block)["number"] != block_number:
        raise ForkError("fork block number mismatch")
    context = block_context(block)
    parent = parent_block(rpc, block)
    pinned_block(rpc, block)
    next_context = synthetic_context(
        context,
        quantity(block["gasUsed"], "gasUsed"),
        quantity(block["blobGasUsed"], "blobGasUsed"),
    )
    return {
        "kind": "fork",
        "anchor": block_identity(block),
        "parent_header": parent_header(parent),
        "target_header": target_header(block),
        "block": next_context,
        "beacon_root": None,
        "prefix": [],
        "target_record": None,
    }


def synthetic_context(
    context: dict[str, Any], used: int, blob_used: int
) -> dict[str, Any]:
    timestamp = context["timestamp"] + 12
    cancun(timestamp)
    gas_target = context["gas_limit"] // 2
    if gas_target == 0:
        raise ForkError("block gas target is zero")
    base_fee = int(context["base_fee"], 16)
    delta = base_fee * abs(used - gas_target) // gas_target // 8
    next_fee = base_fee + max(delta, 1) if used > gas_target else base_fee - delta
    next_context = {
        **context,
        "number": context["number"] + 1,
        "timestamp": timestamp,
        "base_fee": hex(next_fee),
        "excess_blob_gas": max(
            0,
            context["excess_blob_gas"] + blob_used - 393216,
        ),
    }
    return next_context


def parent_header(block: dict[str, Any]) -> dict[str, Any]:
    return {
        "number": block_identity(block)["number"],
        "hash": hash32(required(block, "hash"), "header hash"),
        "parent_hash": hash32(required(block, "parentHash"), "header parentHash"),
        "timestamp": quantity(
            required(block, "timestamp"), "header timestamp", 2**64 - 1
        ),
    }


def target_header(block: dict[str, Any]) -> dict[str, Any]:
    return {
        **parent_header(block),
        "context": block_context(block),
        "gas_used": quantity(block["gasUsed"], "header gasUsed", 2**64 - 1),
        "blob_gas_used": quantity(block["blobGasUsed"], "header blobGasUsed", 2**64 - 1),
        "beacon_root": hash32(
            block["parentBeaconBlockRoot"], "header parentBeaconBlockRoot"
        ),
        "transaction_count": len(
            sequence(required(block, "transactions"), "transactions")
        ),
    }
