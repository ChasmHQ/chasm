from __future__ import annotations

from typing import Any

from .metadata import block_context, synthetic_context
from .validation import ForkError, hash32, integer, obj


def validate_headers(metadata: dict[str, Any]) -> dict[str, Any]:
    anchor = metadata["anchor"]
    parent = obj(metadata["parent_header"], "parent header")
    target = obj(metadata["target_header"], "target header")
    common = {"number", "hash", "parent_hash", "timestamp"}
    if set(parent) != common or set(target) != common | {
        "context",
        "gas_used",
        "blob_gas_used",
        "beacon_root",
        "transaction_count",
    }:
        raise ForkError("invalid retained header fields")
    for header in (parent, target):
        integer(header["number"], "header number")
        integer(header["timestamp"], "header timestamp")
        hash32(header["hash"], "header hash")
        hash32(header["parent_hash"], "header parent hash")
    if (
        target["parent_hash"] != parent["hash"]
        or target["number"] != parent["number"] + 1
        or target["timestamp"] <= parent["timestamp"]
    ):
        raise ForkError("retained target/parent header ancestry mismatch")
    root = hash32(target["beacon_root"], "retained beacon root")
    context = obj(target["context"], "retained block context")
    fields = {
        "number",
        "timestamp",
        "beneficiary",
        "gas_limit",
        "base_fee",
        "prevrandao",
        "excess_blob_gas",
    }
    if set(context) != fields:
        raise ForkError("invalid retained context fields")
    number = integer(context["number"], "context number")
    timestamp = integer(context["timestamp"], "context timestamp")
    if number != target["number"] or timestamp != target["timestamp"]:
        raise ForkError("retained target header/context identity mismatch")
    used = integer(target["gas_used"], "header gas_used")
    blob_used = integer(target["blob_gas_used"], "header blob_gas_used")
    integer(target["transaction_count"], "header transaction_count")
    validated_context = block_context(
        {
            "number": hex(number),
            "hash": target["hash"],
            "timestamp": hex(timestamp),
            "miner": context["beneficiary"],
            "gasLimit": hex(integer(context["gas_limit"], "context gas_limit")),
            "baseFeePerGas": context["base_fee"],
            "mixHash": context["prevrandao"],
            "excessBlobGas": hex(
                integer(context["excess_blob_gas"], "context excess_blob_gas")
            ),
            "gasUsed": hex(used),
            "blobGasUsed": hex(blob_used),
            "parentBeaconBlockRoot": root,
        }
    )
    if context != validated_context:
        raise ForkError("retained target context differs from normalized header")
    selected = parent if metadata["kind"] == "replay" else target
    if anchor["hash"] != selected["hash"] or anchor["number"] != selected["number"]:
        raise ForkError("anchor differs from retained header")
    expected = (
        context
        if metadata["kind"] == "replay"
        else synthetic_context(context, used, blob_used)
    )
    if metadata["block"] != expected:
        raise ForkError("execution block context differs from retained target header")
    if metadata["kind"] == "replay" and metadata["beacon_root"] != root:
        raise ForkError("beacon transition root differs from retained target header")
    return target
