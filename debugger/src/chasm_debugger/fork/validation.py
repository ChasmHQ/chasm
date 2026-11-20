from __future__ import annotations

import re
from typing import Any

CANCUN_TIMESTAMP = 1710338135
PRAGUE_TIMESTAMP = 1746612311
MAINNET_GENESIS = "0xd4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"
_QUANTITY = re.compile(r"0x(?:0|[1-9a-f][0-9a-f]*)\Z")
_DATA = re.compile(r"0x(?:[0-9a-f]{2})*\Z")


class ForkError(ValueError):
    pass


def obj(value: Any, name: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ForkError(f"{name} must be an object")
    return value


def required(value: dict[str, Any], name: str) -> Any:
    if name not in value:
        raise ForkError(f"missing required field {name}")
    return value[name]


def integer(value: Any, name: str, maximum: int = 2**64 - 1) -> int:
    if type(value) is not int or not 0 <= value <= maximum:
        raise ForkError(f"invalid {name}: expected unsigned integer")
    return value


def quantity(value: Any, name: str, maximum: int = 2**256 - 1) -> int:
    if not isinstance(value, str) or not _QUANTITY.fullmatch(value):
        raise ForkError(f"invalid canonical quantity {name}")
    result = int(value, 16)
    if result > maximum:
        raise ForkError(f"{name} exceeds supported range")
    return result


def data(value: Any, name: str, size: int | None = None) -> str:
    if not isinstance(value, str) or not _DATA.fullmatch(value):
        raise ForkError(f"invalid canonical hex data {name}")
    if size is not None and len(value) != 2 + size * 2:
        raise ForkError(f"invalid {name}: expected {size} bytes")
    return value


def address(value: Any, name: str = "address") -> str:
    return data(value, name, 20)


def hash32(value: Any, name: str = "hash") -> str:
    return data(value, name, 32)


def sequence(value: Any, name: str) -> list[Any]:
    if not isinstance(value, list):
        raise ForkError(f"{name} must be an array")
    return value


def cancun(timestamp: int) -> None:
    if not CANCUN_TIMESTAMP <= timestamp < PRAGUE_TIMESTAMP:
        raise ForkError(
            "only Ethereum mainnet Cancun timestamps before Prague are supported"
        )


def reject_newer(value: dict[str, Any]) -> None:
    for key in (
        "authorizationList",
        "authorization_list",
        "requestsHash",
        "executionRequests",
    ):
        if key in value:
            raise ForkError(f"unsupported post-Cancun field {key}")


def access_list(value: Any) -> list[dict[str, Any]]:
    result = []
    for item in sequence(value, "accessList"):
        entry = obj(item, "accessList entry")
        result.append(
            {
                "address": address(required(entry, "address")),
                "storage_keys": [
                    hash32(key, "storage key")
                    for key in sequence(required(entry, "storageKeys"), "storageKeys")
                ],
            }
        )
    return result


def native_request(value: Any) -> dict[str, Any]:
    tx = obj(value, "transaction request")
    fields = {
        "caller",
        "to",
        "data",
        "value",
        "gas_limit",
        "nonce",
        "chain_id",
        "tx_type",
        "gas_price",
        "max_priority_fee_per_gas",
        "access_list",
        "blob_hashes",
        "max_fee_per_blob_gas",
        "commit",
    }
    if set(tx) != fields:
        raise ForkError("transaction request has missing or unsupported fields")
    address(tx["caller"], "caller")
    if tx["to"] is not None:
        address(tx["to"], "to")
    data(tx["data"], "data")
    quantity(tx["value"], "value")
    integer(tx["gas_limit"], "gas_limit")
    integer(tx["nonce"], "nonce")
    if integer(tx["chain_id"], "chain_id") != 1:
        raise ForkError("only chain_id 1 is supported")
    kind = integer(tx["tx_type"], "tx_type", 3)
    price = quantity(tx["gas_price"], "gas_price")
    priority = tx["max_priority_fee_per_gas"]
    if kind >= 2:
        if quantity(priority, "max_priority_fee_per_gas") > price:
            raise ForkError("priority fee exceeds max fee")
    elif priority is not None:
        raise ForkError("legacy/access-list transaction cannot have a priority fee")
    entries = sequence(tx["access_list"], "access_list")
    for entry in entries:
        entry = obj(entry, "access_list entry")
        if set(entry) != {"address", "storage_keys"}:
            raise ForkError("invalid access_list entry fields")
        address(entry["address"])
        for key in sequence(entry["storage_keys"], "storage_keys"):
            hash32(key, "storage key")
    if kind == 0 and entries:
        raise ForkError("legacy transaction cannot have an access list")
    blobs = sequence(tx["blob_hashes"], "blob_hashes")
    for blob in blobs:
        if hash32(blob, "blob hash")[2:4] != "01":
            raise ForkError("unsupported blob version")
    if kind == 3:
        if tx["to"] is None or not 1 <= len(blobs) <= 6:
            raise ForkError("blob transaction needs a destination and 1..6 blob hashes")
        quantity(tx["max_fee_per_blob_gas"], "max_fee_per_blob_gas")
    elif blobs or tx["max_fee_per_blob_gas"] is not None:
        raise ForkError("non-blob transaction contains blob fields")
    if tx["commit"] is not True:
        raise ForkError("replay transactions must commit nonce, gas and state")
    return tx


def logs(value: Any) -> list[dict[str, Any]]:
    result = []
    for item in sequence(value, "logs"):
        item = obj(item, "log")
        topics = [
            hash32(topic, "log topic")
            for topic in sequence(required(item, "topics"), "topics")
        ]
        if len(topics) > 4:
            raise ForkError("log has more than four topics")
        result.append(
            {
                "address": address(required(item, "address")),
                "topics": topics,
                "data": data(required(item, "data"), "log data"),
            }
        )
    return result
