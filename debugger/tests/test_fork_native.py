from __future__ import annotations

import copy
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

import pytest
import requests
from chasm_debugger._revm import RevmChain
from test_fork import (
    BENEFICIARY,
    BLOCK_HASH,
    CALLER,
    DESTINATION,
    PARENT_HASH,
    PREFIX_HASH,
    ROOT,
    TARGET_HASH,
    fake_rpc_result,
    metadata_fixture,
)

from chasm_debugger.fork import ReplayPlan, load_capture, prepare_replay, save_capture
from chasm_debugger.fork.metadata import replay_metadata
from chasm_debugger.fork.validation import ForkError

BEACON = "0x000f3df6d732807ef1319fb7b8bb8522d0beac02"
STOP_ADDRESS = "0x" + "23" * 20
_NATIVE_FORK = hasattr(RevmChain, "transact_json")


def synthetic_reference() -> tuple[ReplayPlan, dict[str, Any]]:
    """Synthetic reference, not historical mainnet evidence: REVERT=21004, STOP=21000."""
    state = metadata_fixture()
    state["block"]["transactions"][1]["to"] = STOP_ADDRESS
    state["receipts"][TARGET_HASH]["to"] = STOP_ADDRESS

    class ReferenceRpc:
        def call(self, method, params):
            return copy.deepcopy(fake_rpc_result(state, method, params))

    metadata = replay_metadata(ReferenceRpc(), TARGET_HASH)
    accounts = {
        CALLER: {"balance": hex(10**18), "nonce": 0, "code": "0x"},
        DESTINATION: {"balance": "0x0", "nonce": 1, "code": "0x5f5ffd"},
        STOP_ADDRESS: {"balance": "0x0", "nonce": 1, "code": "0x00"},
        BENEFICIARY: {"balance": "0x0", "nonce": 0, "code": "0x"},
        # Minimal synthetic beacon contract: calldata root -> slot zero; not mainnet code.
        BEACON: {"balance": "0x0", "nonce": 1, "code": "0x5f355f5500"},
    }
    witness = {
        "version": 1,
        "anchor": metadata["anchor"],
        "accounts": accounts,
        "storage": {BEACON: {"0x0": "0x0"}},
        "block_hashes": {"199": PARENT_HASH},
        "headers": {},
    }
    return ReplayPlan(metadata, witness=witness), witness


@pytest.fixture
def recording_rpc():
    state = metadata_fixture()
    state["block"]["transactions"][1]["to"] = STOP_ADDRESS
    state["receipts"][TARGET_HASH]["to"] = STOP_ADDRESS
    _, witness = synthetic_reference()
    recorded: list[dict[str, Any]] = []
    failures = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                recorded.append(payload)
                method = payload["method"]
                params = payload["params"]
                if method in {
                    "eth_getBalance",
                    "eth_getTransactionCount",
                    "eth_getCode",
                    "eth_getStorageAt",
                }:
                    pin = params[-1]
                    assert pin == {"blockHash": PARENT_HASH, "requireCanonical": True}
                    account = witness["accounts"].get(
                        params[0], {"balance": "0x0", "nonce": 0, "code": "0x"}
                    )
                    if method == "eth_getBalance":
                        result = account["balance"]
                    elif method == "eth_getTransactionCount":
                        result = hex(account["nonce"])
                    elif method == "eth_getCode":
                        result = account["code"]
                    else:
                        result = "0x" + "00" * 32
                else:
                    result = fake_rpc_result(state, method, params)
                reply = {"jsonrpc": "2.0", "id": payload["id"], "result": result}
            except Exception as error:
                failures.append(str(error))
                reply = {
                    "jsonrpc": "2.0",
                    "id": payload.get("id"),
                    "error": {"code": -32000, "message": "fixture error"},
                }
            raw = json.dumps(reply).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", recorded, failures
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@pytest.mark.skipif(not _NATIVE_FORK, reason="native fork bridge is being implemented")
def test_native_local_rpc_prefix_capture_offline(recording_rpc, tmp_path, monkeypatch):
    url, recorded, failures = recording_rpc
    plan = prepare_replay(url, TARGET_HASH)
    baseline = plan.run_baseline()
    assert not failures
    assert baseline.validated
    assert [entry["actual"]["gas_used"] for entry in baseline.report["transactions"]] == [
        21004,
        21000,
    ]
    assert [entry["actual"]["status"] for entry in baseline.report["transactions"]] == [
        0,
        1,
    ]
    assert any(request["method"] == "eth_getStorageAt" for request in recorded)
    assert all(request["method"] != "eth_getRawTransactionByHash" for request in recorded)
    path = tmp_path / "synthetic-reference.json"
    save_capture(path, plan, baseline)
    loaded = load_capture(path)
    before = len(recorded)

    def no_request(*_args, **_kwargs):
        raise AssertionError("offline replay attempted Python HTTP")

    monkeypatch.setattr(requests.Session, "request", no_request)
    repeated = loaded.run_baseline()
    assert repeated.report == baseline.report
    assert len(recorded) == before
    chain = loaded.new_chain()
    assert int(chain.read_nonce(CALLER)) == 1
    assert int(chain.read_storage_at(BEACON, 0), 16) == int(ROOT, 16)
    assert int(chain.read_balance(CALLER), 16) == 10**18 - 21004 * 2
    reset = loaded.new_chain()
    assert int(reset.read_nonce(CALLER)) == 1
    assert int(reset.read_storage_at(BEACON, 0), 16) == int(ROOT, 16)
    assert len(recorded) == before
    assert loaded.identity["block_hash"] == BLOCK_HASH
    assert loaded.metadata["prefix"][0]["identity"]["transaction_hash"] == PREFIX_HASH


@pytest.mark.skipif(not _NATIVE_FORK, reason="native fork bridge is being implemented")
def test_native_hand_derived_offline_reference_and_witness_miss():
    plan, witness = synthetic_reference()
    result = plan.run_baseline()
    assert result.validated
    broken = copy.deepcopy(witness)
    del broken["accounts"][STOP_ADDRESS]
    with pytest.raises(ForkError):
        plan.run_baseline(witness=broken, offline=True)


@pytest.mark.skipif(not _NATIVE_FORK, reason="native fork bridge is being implemented")
@pytest.mark.parametrize("target_oog", [False, True])
def test_native_out_of_gas_commits_nonce_and_full_gas_fee(target_oog):
    plan, witness = synthetic_reference()
    metadata = plan.metadata
    record = metadata["target_record"] if target_oog else metadata["prefix"][0]
    destination = record["request"]["to"]
    witness["accounts"][destination]["code"] = "0x60015f5500"
    witness["storage"][destination] = {"0x0": "0x0"}
    record["receipt"].update(status=0, gas_used=40000, logs=[], contract_address=None)
    metadata["prefix"][0]["receipt"]["cumulative_gas_used"] = metadata["prefix"][0][
        "receipt"
    ]["gas_used"]
    total = (
        metadata["prefix"][0]["receipt"]["gas_used"]
        + metadata["target_record"]["receipt"]["gas_used"]
    )
    metadata["target_record"]["receipt"]["cumulative_gas_used"] = total
    metadata["target_header"]["gas_used"] = total
    oog_plan = ReplayPlan(metadata, witness=witness)
    result = oog_plan.run_baseline()
    assert result.validated
    assert result.report["transactions"][int(target_oog)]["actual"]["gas_used"] == 40000
    chain = oog_plan.new_chain()
    if not target_oog:
        assert int(chain.read_nonce(CALLER)) == 1
        assert int(chain.read_balance(CALLER), 16) == 10**18 - 40000 * 2
    from chasm_debugger.fork.execution import drain_transaction

    drain_transaction(chain, oog_plan.target)
    assert int(chain.read_nonce(CALLER)) == 2
    assert int(chain.read_balance(CALLER), 16) == 10**18 - total * 2
