from __future__ import annotations

import copy
import json
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from .execution import compare_receipt, drain_transaction, export_witness
from .headers import validate_headers
from .metadata import fork_metadata, replay_metadata
from .rpc import MetadataRpc
from .validation import (
    MAINNET_GENESIS,
    ForkError,
    address,
    cancun,
    hash32,
    integer,
    logs,
    native_request,
    obj,
    quantity,
    required,
    sequence,
)

ChainFactory = Callable[[str], Any]


@dataclass(frozen=True)
class BaselineResult:
    report: dict[str, Any]
    witness: dict[str, Any]

    @property
    def validated(self) -> bool:
        return self.report.get("validated") is True


class ReceiptMismatch(ForkError):
    def __init__(self, report: dict[str, Any]) -> None:
        super().__init__("receipt comparison failed; historical replay is unvalidated")
        self.report = report


def native_chain(config: str) -> Any:
    from .._revm import RevmChain

    try:
        return RevmChain(config)
    except Exception:
        raise ForkError(
            "cannot construct strict fork chain (endpoint details redacted)"
        ) from None


def validate_metadata(metadata: Any) -> dict[str, Any]:
    metadata = obj(metadata, "plan")
    if set(metadata) != {
        "kind",
        "anchor",
        "parent_header",
        "target_header",
        "block",
        "beacon_root",
        "prefix",
        "target_record",
    }:
        raise ForkError("plan has missing or unsupported fields")
    kind = metadata["kind"]
    if kind not in {"replay", "fork"}:
        raise ForkError("unsupported fork plan kind")
    anchor = obj(metadata["anchor"], "anchor")
    if set(anchor) != {"chain_id", "number", "hash", "genesis_hash"}:
        raise ForkError("invalid anchor fields")
    if (
        integer(required(anchor, "chain_id"), "chain_id") != 1
        or required(anchor, "genesis_hash") != MAINNET_GENESIS
    ):
        raise ForkError("plan does not anchor Ethereum mainnet")
    integer(anchor["number"], "anchor number")
    hash32(anchor["hash"], "anchor hash")
    block = obj(metadata["block"], "block")
    if set(block) != {
        "number",
        "timestamp",
        "beneficiary",
        "gas_limit",
        "base_fee",
        "prevrandao",
        "excess_blob_gas",
    }:
        raise ForkError("invalid block context fields")
    if integer(block["number"], "block number") != anchor["number"] + 1:
        raise ForkError("execution block must follow anchor")
    cancun(integer(block["timestamp"], "timestamp"))
    address(block["beneficiary"], "beneficiary")
    if integer(block["gas_limit"], "gas_limit") == 0:
        raise ForkError("block gas limit is zero")
    quantity(block["base_fee"], "base_fee")
    hash32(block["prevrandao"], "prevrandao")
    integer(block["excess_blob_gas"], "excess_blob_gas")
    header = validate_headers(metadata)
    prefix = sequence(metadata["prefix"], "prefix")
    target = metadata["target_record"]
    if kind == "fork":
        if prefix or target is not None or metadata["beacon_root"] is not None:
            raise ForkError(
                "sandbox must not claim historical transactions or beacon root"
            )
        return metadata
    hash32(metadata["beacon_root"], "beacon root")
    if len(prefix) + 1 > header["transaction_count"]:
        raise ForkError("replay transaction count exceeds retained header")
    cumulative = 0
    hashes = set()
    block_hash = None
    blobs = 0
    for index, record in enumerate([*prefix, target]):
        record = obj(record, "transaction record")
        if set(record) != {"identity", "request", "receipt"}:
            raise ForkError("invalid transaction record fields")
        identity = obj(record["identity"], "transaction identity")
        if set(identity) != {
            "transaction_hash",
            "block_hash",
            "block_number",
            "transaction_index",
        }:
            raise ForkError("invalid transaction identity fields")
        tx_hash = hash32(identity["transaction_hash"], "transaction hash")
        if tx_hash in hashes:
            raise ForkError("duplicate transaction hash")
        hashes.add(tx_hash)
        this_hash = hash32(identity["block_hash"], "block hash")
        if block_hash is not None and this_hash != block_hash:
            raise ForkError("prefix block hash mismatch")
        block_hash = this_hash
        if this_hash != header["hash"]:
            raise ForkError("transaction block hash differs from retained header")
        if (
            integer(identity["block_number"], "block_number") != block["number"]
            or integer(identity["transaction_index"], "transaction_index") != index
        ):
            raise ForkError("prefix transaction order mismatch")
        request = native_request(record["request"])
        if request["gas_limit"] > block["gas_limit"] - cumulative:
            raise ForkError("transaction gas limit exceeds remaining block gas pool")
        blobs += len(request["blob_hashes"])
        if blobs > 6:
            raise ForkError("prefix exceeds Cancun blob gas pool")
        expected = obj(record["receipt"], "receipt")
        if set(expected) != {
            "status",
            "gas_used",
            "cumulative_gas_used",
            "logs",
            "contract_address",
            "identity",
        }:
            raise ForkError("invalid expected receipt fields")
        if expected["identity"] != identity:
            raise ForkError("receipt identity mismatch")
        status = integer(expected["status"], "receipt status", 1)
        used = integer(expected["gas_used"], "receipt gas_used")
        cumulative += used
        if (
            used > request["gas_limit"]
            or integer(expected["cumulative_gas_used"], "cumulative_gas_used")
            != cumulative
        ):
            raise ForkError("receipt gas accounting mismatch")
        result_logs = logs(expected["logs"])
        created = expected["contract_address"]
        if created is not None:
            address(created, "contract_address")
        if status == 0 and (created is not None or result_logs):
            raise ForkError(
                "reverted receipt contains committed logs or contract creation"
            )
        if request["to"] is not None and created is not None:
            raise ForkError("call receipt contains contract creation")
        if request["to"] is None and status == 1 and created is None:
            raise ForkError("successful creation has no contract address")
    if cumulative > header["gas_used"] or blobs * 131072 > header["blob_gas_used"]:
        raise ForkError("prefix gas exceeds retained target header")
    if len(prefix) + 1 == header["transaction_count"] and (
        cumulative != header["gas_used"] or blobs * 131072 != header["blob_gas_used"]
    ):
        raise ForkError("final prefix gas differs from retained target header")
    return metadata


class ForkPlan:
    def __init__(
        self,
        metadata: dict[str, Any],
        rpc_url: str | None = None,
        *,
        witness: dict[str, Any] | None = None,
        baseline_report: dict[str, Any] | None = None,
    ) -> None:
        self._metadata = copy.deepcopy(validate_metadata(metadata))
        self._rpc_url = rpc_url
        self.saved_witness = copy.deepcopy(witness)
        self.baseline_report = copy.deepcopy(baseline_report)

    def __repr__(self) -> str:
        return f"{type(self).__name__}(identity={self.identity!r})"

    @property
    def metadata(self) -> dict[str, Any]:
        return copy.deepcopy(self._metadata)

    @property
    def target(self) -> dict[str, Any] | None:
        record = self._metadata["target_record"]
        return copy.deepcopy(record["request"]) if record else None

    @property
    def identity(self) -> dict[str, Any]:
        record = self._metadata["target_record"]
        return copy.deepcopy(record["identity"] if record else self._metadata["anchor"])

    @property
    def report(self) -> dict[str, Any]:
        sandbox = self._metadata["kind"] == "fork"
        return {
            "kind": self._metadata["kind"],
            "identity": self.identity,
            "anchor": copy.deepcopy(self._metadata["anchor"]),
            "block": copy.deepcopy(self._metadata["block"]),
            "prefix_count": len(self._metadata["prefix"]),
            "sandbox": sandbox,
            "state_validation": False,
            "validated": False,
            "scope": "receipt comparison only; provider metadata is not a consensus proof",
            "context": "synthetic next block; inherited beneficiary/prevrandao; no beacon transition"
            if sandbox
            else "historical parent state plus EIP-4788 and ordered transaction prefix",
        }

    def chain_config(
        self, witness: dict[str, Any] | None = None, offline: bool = False
    ) -> dict[str, Any]:
        witness = witness if witness is not None else self.saved_witness
        offline = offline or self._rpc_url is None
        if offline and not witness:
            raise ForkError("offline fork requires a saved remote-state witness")
        if witness is not None:
            obj(witness, "witness")
        return {
            "rpc_url": None if offline else self._rpc_url,
            "anchor": copy.deepcopy(self._metadata["anchor"]),
            "block": copy.deepcopy(self._metadata["block"]),
            "strict": True,
            "witness": copy.deepcopy(witness),
        }

    def prepare_chain(self, chain: Any, *, enforce: bool = True) -> dict[str, Any]:
        root = self._metadata["beacon_root"]
        if root is not None:
            try:
                chain.apply_beacon_root(bytes.fromhex(root[2:]))
            except Exception:
                raise ForkError(
                    "beacon system transition failed (endpoint details redacted)"
                ) from None
        comparisons = []
        cumulative = 0
        for record in self._metadata["prefix"]:
            request = record["request"]
            self.check_gas_pool(request, cumulative)
            actual = drain_transaction(chain, request)
            cumulative += actual["gas_used"]
            comparison = compare_receipt(record["receipt"], actual)
            comparisons.append(comparison)
            if enforce and not comparison["matched"]:
                raise ReceiptMismatch(
                    {
                        "validated": False,
                        "transactions": comparisons,
                        "state_validation": False,
                    }
                )
        if self.target is not None:
            self.check_gas_pool(self.target, cumulative)
        return {
            "validated": all(item["matched"] for item in comparisons),
            "transactions": comparisons,
            "gas_used": cumulative,
            "state_validation": False,
        }

    def check_gas_pool(self, request: dict[str, Any], consumed: int) -> None:
        if request["gas_limit"] > self._metadata["block"]["gas_limit"] - consumed:
            raise ForkError("transaction gas limit exceeds remaining block gas pool")

    def new_chain(
        self,
        *,
        witness: dict[str, Any] | None = None,
        offline: bool = False,
        enforce: bool = True,
        chain_factory: ChainFactory | None = None,
    ) -> Any:
        config = json.dumps(
            self.chain_config(witness, offline), separators=(",", ":"), sort_keys=True
        )
        chain = (chain_factory or native_chain)(config)
        self.prepare_chain(chain, enforce=enforce)
        return chain

    def run_baseline(
        self,
        *,
        target: dict[str, Any] | None = None,
        witness: dict[str, Any] | None = None,
        offline: bool = False,
        enforce: bool = True,
        chain_factory: ChainFactory | None = None,
    ) -> BaselineResult:
        if target is not None and self._metadata["kind"] == "replay":
            raise ForkError("historical baseline cannot override the target transaction")
        config = json.dumps(
            self.chain_config(witness, offline), separators=(",", ":"), sort_keys=True
        )
        chain = (chain_factory or native_chain)(config)
        prefix = self.prepare_chain(chain, enforce=enforce)
        request = self.target if target is None else native_request(target)
        comparisons = prefix["transactions"]
        execution = None
        if request is not None:
            self.check_gas_pool(request, prefix["gas_used"])
            execution = drain_transaction(chain, request)
            record = self._metadata["target_record"]
            if record:
                comparison = compare_receipt(record["receipt"], execution)
                comparisons.append(comparison)
        validated = self._metadata["kind"] == "replay" and all(
            item["matched"] for item in comparisons
        )
        report = {
            **self.report,
            "validated": validated,
            "validation": "matched_receipts" if validated else "unvalidated",
            "transactions": comparisons,
            "execution": execution,
        }
        self.baseline_report = copy.deepcopy(report)
        if enforce and self._metadata["kind"] == "replay" and not validated:
            raise ReceiptMismatch(report)
        return BaselineResult(report=report, witness=export_witness(chain))


class ReplayPlan(ForkPlan):
    pass


def prepare_replay(rpc_url: str, tx_hash: str) -> ReplayPlan:
    hash32(tx_hash, "transaction hash")
    with MetadataRpc(rpc_url) as rpc:
        metadata = replay_metadata(rpc, tx_hash)
    return ReplayPlan(metadata, rpc_url)


def prepare_fork(rpc_url: str, block_number: int) -> ForkPlan:
    integer(block_number, "block_number")
    with MetadataRpc(rpc_url) as rpc:
        metadata = fork_metadata(rpc, block_number)
    return ForkPlan(metadata, rpc_url)
