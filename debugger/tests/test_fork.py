from __future__ import annotations

import copy
import json
from datetime import datetime, timezone
from typing import Any

import pytest
import requests

from chasm_debugger.fork import (
    CANCUN_TIMESTAMP,
    MAINNET_GENESIS,
    PRAGUE_TIMESTAMP,
    BaselineResult,
    ForkError,
    ForkPlan,
    ReceiptMismatch,
    ReplayPlan,
    capture_bundle,
    load_capture,
    prepare_fork,
    prepare_replay,
    save_capture,
)
from chasm_debugger.fork.execution import compare_receipt, execution_receipt
from chasm_debugger.fork.rpc import MetadataRpc

CALLER = "0x" + "11" * 20
DESTINATION = "0x" + "22" * 20
BENEFICIARY = "0x" + "33" * 20
PARENT_HASH = "0x" + "aa" * 32
BLOCK_HASH = "0x" + "bb" * 32
ROOT = "0x" + "cc" * 32
PREFIX_HASH = "0x" + "01" * 32
TARGET_HASH = "0x" + "02" * 32


def metadata_fixture() -> dict[str, Any]:
    parent = {
        "number": "0xc7",
        "hash": PARENT_HASH,
        "parentHash": "0x" + "dd" * 32,
        "timestamp": hex(CANCUN_TIMESTAMP + 12),
    }
    block = {
        "number": "0xc8",
        "hash": BLOCK_HASH,
        "parentHash": PARENT_HASH,
        "timestamp": hex(CANCUN_TIMESTAMP + 24),
        "miner": BENEFICIARY,
        "gasLimit": hex(100000),
        "gasUsed": hex(42004),
        "baseFeePerGas": "0x1",
        "mixHash": "0x" + "44" * 32,
        "excessBlobGas": "0x0",
        "blobGasUsed": "0x0",
        "parentBeaconBlockRoot": ROOT,
    }
    transactions = []
    receipts = {}
    for index, tx_hash in enumerate((PREFIX_HASH, TARGET_HASH)):
        tx = {
            "hash": tx_hash,
            "blockHash": BLOCK_HASH,
            "blockNumber": "0xc8",
            "transactionIndex": hex(index),
            "type": "0x2",
            "chainId": "0x1",
            "from": CALLER,
            "to": DESTINATION,
            "input": "0x",
            "value": "0x0",
            "gas": hex(40000),
            "nonce": hex(index),
            "maxFeePerGas": "0x2",
            "maxPriorityFeePerGas": "0x1",
            "gasPrice": "0x2",
            "accessList": [],
        }
        transactions.append(tx)
        # Hand-derived reference: PUSH0 PUSH0 REVERT = 21004; STOP = 21000.
        used = 21004 if index == 0 else 21000
        receipts[tx_hash] = {
            "transactionHash": tx_hash,
            "blockHash": BLOCK_HASH,
            "blockNumber": "0xc8",
            "transactionIndex": hex(index),
            "type": "0x2",
            "from": CALLER,
            "to": DESTINATION,
            "status": hex(index),
            "gasUsed": hex(used),
            "cumulativeGasUsed": hex(21004 + (21000 if index else 0)),
            "logs": [],
            "contractAddress": None,
        }
    block["transactions"] = transactions
    return {
        "parent": parent,
        "block": block,
        "receipts": receipts,
        "genesis": {"number": "0x0", "hash": MAINNET_GENESIS},
        "chain_id": "0x1",
    }


def fake_rpc_result(state: dict[str, Any], method: str, params: list[Any]) -> Any:
    if method == "eth_chainId":
        return state["chain_id"]
    if method == "eth_getBlockByNumber":
        return {"0x0": state["genesis"], "0xc7": state["parent"], "0xc8": state["block"]}[
            params[0]
        ]
    if method == "eth_getBlockByHash":
        return {PARENT_HASH: state["parent"], BLOCK_HASH: state["block"]}[params[0]]
    if method == "eth_getTransactionByHash":
        return next(
            tx for tx in state["block"]["transactions"] if tx["hash"] == params[0]
        )
    if method == "eth_getTransactionReceipt":
        return state["receipts"][params[0]]
    raise AssertionError(f"unexpected metadata method {method}")


@pytest.fixture
def metadata_rpc(monkeypatch):
    state = metadata_fixture()
    calls = []

    def call(self, method, params):
        calls.append((method, params))
        return copy.deepcopy(fake_rpc_result(state, method, params))

    monkeypatch.setattr(MetadataRpc, "call", call)
    return state, calls


def replay(metadata_rpc) -> ReplayPlan:
    return prepare_replay("http://user:secret@127.0.0.1:1/private-token", TARGET_HASH)


class FakeChain:
    def __init__(self, config: str) -> None:
        self.config = json.loads(config)
        self.requests: list[dict[str, Any]] = []
        self.roots: list[bytes] = []
        self.events: list[dict[str, Any]] = []
        self.resumes = 0
        self.gas_delta = 0
        self.host_call = False

    def apply_beacon_root(self, root: bytes) -> None:
        self.roots.append(root)

    def transact_json(self, request: str) -> None:
        tx = json.loads(request)
        assert self.roots == [bytes.fromhex(ROOT[2:])]
        assert tx["nonce"] == len(self.requests)
        assert tx["commit"] is True
        self.requests.append(tx)
        if self.host_call:
            self.events = [{"type": "host_call", "data": b""}]
        else:
            self.events = (
                [{"type": "paused", "reason": "revert"}] if tx["nonce"] == 0 else []
            )
            self.events.append(
                {
                    "type": "finished",
                    "success": tx["nonce"] == 1,
                    "gas_used": (21004 if tx["nonce"] == 0 else 21000) + self.gas_delta,
                    "created_address": None,
                    "logs": [],
                    "output": b"",
                }
            )

    def wait(self, timeout=30.0):
        return self.events.pop(0)

    def resume(self):
        self.resumes += 1

    def export_fork(self):
        return json.dumps(
            {
                "version": 1,
                "anchor": self.config["anchor"],
                "accounts": {},
                "storage": {},
                "headers": {},
                "block_hashes": {},
            }
        )


def test_replay_metadata_only_parent_anchor_and_full_target(metadata_rpc):
    plan = replay(metadata_rpc)
    state, calls = metadata_rpc
    assert isinstance(plan, ReplayPlan)
    assert plan.identity["transaction_hash"] == TARGET_HASH
    assert plan.identity["transaction_index"] == 1
    assert plan.target["max_priority_fee_per_gas"] == "0x1"
    assert plan.target["gas_price"] == "0x2"
    assert plan.target["nonce"] == 1
    assert plan.chain_config()["anchor"]["hash"] == PARENT_HASH
    assert plan.chain_config()["block"]["number"] == 200
    assert plan.report["state_validation"] is False
    assert not any("RawTransaction" in method for method, _ in calls)
    assert state["block"]["transactions"][1]["gas"] == hex(40000)
    assert "secret" not in repr(plan)


def test_baseline_reverted_prefix_commits_and_restart_repeats(metadata_rpc):
    plan = replay(metadata_rpc)
    chains = []

    def factory(config):
        chain = FakeChain(config)
        chains.append(chain)
        return chain

    baseline = plan.run_baseline(chain_factory=factory)
    assert isinstance(baseline, BaselineResult)
    assert baseline.validated
    assert baseline.report["validation"] == "matched_receipts"
    assert not baseline.report["state_validation"]
    assert [tx["nonce"] for tx in chains[0].requests] == [0, 1]
    assert chains[0].resumes == 1
    first = plan.new_chain(chain_factory=factory)
    second = plan.new_chain(chain_factory=factory)
    assert first is not second
    assert first.config == second.config
    assert first.requests == second.requests
    assert len(first.requests) == 1
    assert first.roots == second.roots == [bytes.fromhex(ROOT[2:])]


def test_baseline_fails_closed_or_reports_unvalidated(metadata_rpc):
    plan = replay(metadata_rpc)

    def factory(config):
        chain = FakeChain(config)
        chain.gas_delta = 1
        return chain

    with pytest.raises(ReceiptMismatch) as error:
        plan.run_baseline(chain_factory=factory)
    assert error.value.report["validated"] is False
    result = plan.run_baseline(chain_factory=factory, enforce=False)
    assert not result.validated
    assert len(result.report["transactions"]) == 2
    assert result.report["transactions"][0]["differences"]["gas_used"] == {
        "expected": 21004,
        "actual": 21005,
    }


def test_host_call_is_not_dispatched(metadata_rpc):
    plan = replay(metadata_rpc)

    def factory(config):
        chain = FakeChain(config)
        chain.host_call = True
        return chain

    with pytest.raises(ForkError, match="host calls are forbidden"):
        plan.run_baseline(chain_factory=factory)


@pytest.mark.parametrize("field", ["status", "gas_used", "logs", "contract_address"])
def test_receipt_comparison_reports_each_parity_dimension(field):
    expected = {
        "status": 1,
        "gas_used": 21000,
        "logs": [{"address": DESTINATION, "topics": [ROOT], "data": "0x"}],
        "contract_address": None,
        "identity": {"transaction_hash": TARGET_HASH},
    }
    actual = {
        key: copy.deepcopy(value) for key, value in expected.items() if key != "identity"
    }
    actual[field] = {
        "status": 0,
        "gas_used": 21001,
        "logs": [],
        "contract_address": DESTINATION,
    }[field]
    comparison = compare_receipt(expected, actual)
    assert not comparison["matched"]
    assert set(comparison["differences"]) == {field}
    assert comparison["identity"] == expected["identity"]


@pytest.mark.parametrize("field", ["success", "gas_used", "logs", "created_address"])
def test_native_receipt_missing_field_never_defaults(field):
    event = {"success": True, "gas_used": 21000, "logs": [], "created_address": None}
    del event[field]
    with pytest.raises(ForkError):
        execution_receipt(event)


def test_native_topic_quantity_is_normalized_to_fixed_data32():
    result = execution_receipt(
        {
            "success": True,
            "gas_used": 1,
            "created_address": None,
            "logs": [{"address": DESTINATION, "topics": ["0xabc"], "data": b"\x01"}],
        }
    )
    assert result["logs"] == [
        {"address": DESTINATION, "topics": ["0x" + "0" * 61 + "abc"], "data": "0x01"}
    ]


@pytest.mark.parametrize(
    "mutation,match",
    [
        (lambda state: state.update(chain_id="0x2"), "chain ID"),
        (lambda state: state["genesis"].update(hash=ROOT), "genesis"),
        (lambda state: state["parent"].update(hash=ROOT), "ancestry"),
        (lambda state: state["block"].update(timestamp=hex(PRAGUE_TIMESTAMP)), "Cancun"),
        (
            lambda state: state["block"].update(timestamp=hex(CANCUN_TIMESTAMP - 1)),
            "Cancun",
        ),
        (lambda state: state["block"].update(requestsHash=ROOT), "post-Cancun"),
        (lambda state: state["block"]["transactions"][0].update(type="0x4"), "range"),
        (
            lambda state: state["block"]["transactions"][0].update(authorizationList=[]),
            "post-Cancun",
        ),
        (lambda state: state["block"]["transactions"][0].pop("nonce"), "required"),
        (
            lambda state: state["block"]["transactions"][0].update(value="0x00"),
            "canonical",
        ),
        (
            lambda state: state["block"]["transactions"][0].update(
                transactionIndex="0x1"
            ),
            "index",
        ),
        (
            lambda state: state["receipts"][PREFIX_HASH].update(transactionHash=ROOT),
            "identity",
        ),
        (
            lambda state: state["receipts"][PREFIX_HASH].update(cumulativeGasUsed="0x0"),
            "gas accounting",
        ),
        (
            lambda state: state["block"]["transactions"][1].update(gas=hex(80000)),
            "gas pool",
        ),
    ],
)
def test_metadata_fails_closed(metadata_rpc, mutation, match):
    state, _ = metadata_rpc
    mutation(state)
    with pytest.raises(ForkError, match=match):
        replay(metadata_rpc)


def test_gas_pool_checked_against_actual_prefix_before_target(metadata_rpc):
    plan = replay(metadata_rpc)
    metadata = plan.metadata
    metadata["block"]["gas_limit"] = 61004
    metadata["target_header"]["context"]["gas_limit"] = 61004
    metadata["target_record"]["request"]["gas_limit"] = 40000
    tight = ReplayPlan(metadata, "http://127.0.0.1:1")
    chains = []

    def factory(config):
        chain = FakeChain(config)
        chain.gas_delta = 1
        chains.append(chain)
        return chain

    with pytest.raises(ForkError, match="gas pool"):
        tight.run_baseline(chain_factory=factory, enforce=False)
    assert len(chains[0].requests) == 1


def test_receipt_logs_order_and_fixed_topics(metadata_rpc):
    state, _ = metadata_rpc
    base = {
        "transactionHash": TARGET_HASH,
        "blockHash": BLOCK_HASH,
        "blockNumber": "0xc8",
        "transactionIndex": "0x1",
        "removed": False,
        "address": DESTINATION,
        "topics": [ROOT],
        "data": "0x",
    }
    state["receipts"][TARGET_HASH]["logs"] = [
        {**base, "logIndex": "0x0"},
        {**base, "logIndex": "0x1", "data": "0x01"},
    ]
    plan = replay(metadata_rpc)
    assert plan.metadata["target_record"]["receipt"]["logs"][1]["data"] == "0x01"
    state["receipts"][TARGET_HASH]["logs"][0]["topics"] = ["0x01"]
    with pytest.raises(ForkError, match="32 bytes"):
        replay(metadata_rpc)


def test_fork_is_pinned_synthetic_next_block(metadata_rpc):
    plan = prepare_fork("http://127.0.0.1:1", 200)
    assert isinstance(plan, ForkPlan)
    assert plan.target is None
    assert plan.report["sandbox"]
    assert "no beacon transition" in plan.report["context"]
    config = plan.chain_config()
    assert config["anchor"]["hash"] == BLOCK_HASH
    assert config["block"]["number"] == 201
    assert config["block"]["timestamp"] == CANCUN_TIMESTAMP + 36
    assert config["block"]["base_fee"] == "0x1"
    chain = plan.new_chain(chain_factory=FakeChain)
    assert not chain.requests and not chain.roots
    baseline = plan.run_baseline(chain_factory=FakeChain)
    assert not baseline.validated and baseline.report["sandbox"]


def test_prague_boundary_is_mainnet_activation():
    assert (
        datetime.fromtimestamp(PRAGUE_TIMESTAMP, timezone.utc).isoformat()
        == "2025-05-07T10:05:11+00:00"
    )


def test_capture_offline_deterministic_no_credentials_or_requests(
    metadata_rpc, tmp_path, monkeypatch
):
    plan = replay(metadata_rpc)
    baseline = plan.run_baseline(chain_factory=FakeChain)
    path = tmp_path / "capture.json"
    save_capture(path, plan, baseline)
    original = path.read_bytes()
    assert b"secret" not in original and b"127.0.0.1" not in original
    assert b"rpc_url" not in original
    with pytest.raises(FileExistsError):
        save_capture(path, plan, baseline)
    save_capture(path, plan, baseline, overwrite=True)
    assert path.read_bytes() == original

    def forbid(*args, **kwargs):
        raise AssertionError("offline path attempted a request")

    monkeypatch.setattr(requests.Session, "request", forbid)
    loaded = load_capture(path)
    assert isinstance(loaded, ReplayPlan)
    assert loaded.chain_config()["rpc_url"] is None
    assert loaded.chain_config()["witness"] == baseline.witness
    repeated = loaded.run_baseline(chain_factory=FakeChain)
    assert repeated.report == baseline.report
    assert loaded.new_chain(chain_factory=FakeChain).config["rpc_url"] is None
    override = {**baseline.witness, "extra_remote_state": {"captured": True}}
    assert capture_bundle(plan, baseline, witness=override)["witness"] == override


def test_capture_accepts_legacy_sevm_format(metadata_rpc, tmp_path):
    plan = replay(metadata_rpc)
    baseline = plan.run_baseline(chain_factory=FakeChain)
    bundle = capture_bundle(plan, baseline)
    assert bundle["format"] == "chasm-fork-capture"
    bundle["format"] = "sevm-fork-capture"
    path = tmp_path / "legacy-capture.json"
    path.write_text(json.dumps(bundle), encoding="utf-8")
    loaded = load_capture(path)
    assert loaded.run_baseline(chain_factory=FakeChain).report == baseline.report


@pytest.mark.parametrize(
    "change",
    [
        lambda bundle: bundle.update(version=2),
        lambda bundle: bundle.update(witness={}),
        lambda bundle: bundle["plan"]["anchor"].update(chain_id=2),
        lambda bundle: bundle["plan"]["target_record"]["request"].pop("nonce"),
        lambda bundle: bundle["plan"]["prefix"][0]["identity"].update(
            transaction_index=1
        ),
        lambda bundle: bundle["witness"].update(rpc_url="http://secret@localhost"),
        lambda bundle: bundle["plan"]["target_header"].update(parent_hash=ROOT),
        lambda bundle: bundle["plan"]["parent_header"].update(hash=ROOT),
        lambda bundle: bundle["plan"]["target_header"].update(
            timestamp=CANCUN_TIMESTAMP + 36
        ),
        lambda bundle: bundle["plan"]["target_header"]["context"].update(base_fee="0x2"),
        lambda bundle: bundle["plan"]["block"].update(prevrandao=ROOT),
        lambda bundle: bundle["plan"]["target_header"].update(beacon_root=BLOCK_HASH),
        lambda bundle: bundle["plan"].update(beacon_root=BLOCK_HASH),
        lambda bundle: bundle["plan"]["target_header"].update(transaction_count=1),
        lambda bundle: bundle["plan"]["target_header"].update(gas_used=42005),
    ],
)
def test_capture_rejects_incomplete_or_tampered_inputs(metadata_rpc, tmp_path, change):
    plan = replay(metadata_rpc)
    baseline = plan.run_baseline(chain_factory=FakeChain)
    bundle = capture_bundle(plan, baseline)
    change(bundle)
    path = tmp_path / "capture.json"
    path.write_text(json.dumps(bundle), encoding="utf-8")
    with pytest.raises(ForkError):
        load_capture(path)


def test_offline_missing_witness_is_explicit(metadata_rpc):
    plan = replay(metadata_rpc)
    with pytest.raises(ForkError, match="witness"):
        plan.chain_config(offline=True)


def test_rpc_disables_redirects_and_environment_proxy(monkeypatch):
    observed = {}

    class Session:
        trust_env = True

        def post(self, url, **kwargs):
            observed.update(kwargs)
            observed["trust_env"] = self.trust_env
            return type("Response", (), {"status_code": 302})()

        def close(self):
            observed["closed"] = True

    monkeypatch.setattr(requests, "Session", Session)
    with MetadataRpc("http://user:secret@127.0.0.1:1/api-key") as rpc:
        with pytest.raises(ForkError) as error:
            rpc.call("eth_chainId", [])
        assert "secret" not in str(error.value)
        assert "api-key" not in str(error.value)
    assert observed["allow_redirects"] is False
    assert observed["trust_env"] is False
    assert observed["closed"]


def test_rpc_transport_exception_is_redacted(monkeypatch):
    def fail(*args, **kwargs):
        raise requests.ConnectionError("http://user:secret@localhost/private-token")

    monkeypatch.setattr(requests.Session, "post", fail)
    with (
        MetadataRpc("http://user:secret@127.0.0.1:1") as rpc,
        pytest.raises(ForkError) as error,
    ):
        rpc.call("eth_chainId", [])
    assert "secret" not in str(error.value)
    assert error.value.__suppress_context__


@pytest.mark.parametrize("kind", [0, 1, 2, 3])
def test_supported_transaction_types_preserve_full_envelope(metadata_rpc, kind):
    state, _ = metadata_rpc
    for tx in state["block"]["transactions"]:
        tx["type"] = hex(kind)
        state["receipts"][tx["hash"]]["type"] = hex(kind)
        if kind < 2:
            tx.pop("maxFeePerGas")
            tx.pop("maxPriorityFeePerGas")
        if kind == 0:
            tx.pop("accessList")
        else:
            tx["accessList"] = [{"address": DESTINATION, "storageKeys": [ROOT]}]
        if kind == 3:
            tx["blobVersionedHashes"] = ["0x01" + "22" * 31]
            tx["maxFeePerBlobGas"] = "0x3"
    if kind == 3:
        state["block"]["blobGasUsed"] = hex(2 * 131072)
    plan = replay(metadata_rpc)
    target = plan.target
    assert target["tx_type"] == kind
    assert target["gas_price"] == "0x2"
    assert target["nonce"] == 1 and target["chain_id"] == 1
    assert target["access_list"] == (
        [{"address": DESTINATION, "storage_keys": [ROOT]}] if kind else []
    )
    assert target["max_priority_fee_per_gas"] == ("0x1" if kind >= 2 else None)
    assert target["max_fee_per_blob_gas"] == ("0x3" if kind == 3 else None)
    assert len(target["blob_hashes"]) == int(kind == 3)


def test_receipt_log_index_order_is_validated(metadata_rpc):
    state, _ = metadata_rpc
    state["receipts"][TARGET_HASH]["logs"] = [
        {
            "transactionHash": TARGET_HASH,
            "blockHash": BLOCK_HASH,
            "blockNumber": "0xc8",
            "transactionIndex": "0x1",
            "removed": False,
            "address": DESTINATION,
            "topics": [ROOT],
            "data": "0x",
            "logIndex": "0x1",
        }
    ]
    with pytest.raises(ForkError, match="logIndex"):
        replay(metadata_rpc)


def test_rpc_restricts_methods_and_validates_endpoint():
    with pytest.raises(ForkError):
        MetadataRpc("file:///private")
    with (
        MetadataRpc("http://127.0.0.1:1") as rpc,
        pytest.raises(ForkError, match="not permitted"),
    ):
        rpc.call("eth_sendRawTransaction", ["0x"])
