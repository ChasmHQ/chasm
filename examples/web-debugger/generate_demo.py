"""Generate synthetic browser fixtures using the installed compiler and local REVM."""

from __future__ import annotations

import copy
import json
from pathlib import Path

from chasm_debugger._revm import RevmChain
from eth_hash.auto import keccak
from eth_utils import function_signature_to_4byte_selector

from chasm_debugger.compile import compile_standard
from chasm_debugger.fork import MAINNET_GENESIS, ReplayPlan, save_capture
from chasm_debugger.fork.execution import drain_transaction

ROOT = Path(__file__).resolve().parent
CALLER = "0x" + "11" * 20
VAULT = "0x" + "22" * 20
BENEFICIARY = "0x" + "33" * 20
BEACON = "0x000f3df6d732807ef1319fb7b8bb8522d0beac02"
ANCHOR = "0x" + "aa" * 32
BLOCK = "0x" + "bb" * 32
TX_HASH = "0x" + "02" * 32
BEACON_ROOT = "0x" + "cc" * 32


def write_new(name: str, value: dict) -> None:
    with (ROOT / name).open("x", encoding="utf-8", newline="\n") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")


def main() -> None:
    content = (ROOT / "WorkshopVault.sol").read_text(encoding="utf-8")
    output = compile_standard(
        {"WorkshopVault.sol": content}, solc_version="0.8.28", evm_version="cancun"
    )
    settings = {"optimizer": {"enabled": False}, "evmVersion": "cancun"}
    build = {
        "solcVersion": "0.8.28",
        "input": {
            "language": "Solidity",
            "sources": {"WorkshopVault.sol": {"content": content}},
            "settings": settings,
        },
        "output": output,
    }
    code = output["contracts"]["WorkshopVault.sol"]["WorkshopVault"]["evm"][
        "deployedBytecode"
    ]["object"]
    anchor = {
        "chain_id": 1,
        "number": 199,
        "hash": ANCHOR,
        "genesis_hash": MAINNET_GENESIS,
    }
    block = {
        "number": 200,
        "timestamp": 1710338159,
        "beneficiary": BENEFICIARY,
        "gas_limit": 30_000_000,
        "base_fee": "0x1",
        "prevrandao": "0x" + "44" * 32,
        "excess_blob_gas": 0,
    }
    mapping_slot = int.from_bytes(
        keccak(bytes.fromhex(CALLER[2:]).rjust(32, b"\x00") + (1).to_bytes(32, "big")),
        "big",
    )
    witness = {
        "version": 1,
        "anchor": anchor,
        "accounts": {
            CALLER: {"balance": hex(10**18), "nonce": 0, "code": "0x"},
            VAULT: {"balance": hex(10**18), "nonce": 1, "code": "0x" + code},
            BENEFICIARY: {"balance": "0x0", "nonce": 0, "code": "0x"},
            BEACON: {"balance": "0x0", "nonce": 1, "code": "0x5f355f5500"},
        },
        "storage": {
            VAULT: {
                "0x0": hex(int(CALLER, 16) | (25 << 160)),
                hex(mapping_slot): hex(10000),
                "0x2": hex(10000),
            },
            BEACON: {"0x0": "0x0"},
        },
        "block_hashes": {"199": ANCHOR},
        "headers": {},
    }
    request = {
        "caller": CALLER,
        "to": VAULT,
        "data": "0x"
        + function_signature_to_4byte_selector("withdraw(uint256)").hex()
        + (1000).to_bytes(32, "big").hex(),
        "value": "0x0",
        "gas_limit": 300000,
        "nonce": 0,
        "chain_id": 1,
        "tx_type": 0,
        "gas_price": "0x2",
        "max_priority_fee_per_gas": None,
        "access_list": [],
        "blob_hashes": [],
        "max_fee_per_blob_gas": None,
        "commit": True,
    }
    config = {
        "rpc_url": None,
        "anchor": anchor,
        "block": block,
        "strict": True,
        "witness": witness,
    }
    chain = RevmChain(json.dumps(config))
    chain.apply_beacon_root(bytes.fromhex(BEACON_ROOT[2:]))
    observed = drain_transaction(chain, request)
    identity = {
        "transaction_hash": TX_HASH,
        "block_hash": BLOCK,
        "block_number": 200,
        "transaction_index": 0,
    }
    expected = {
        **observed,
        "identity": identity,
        "cumulative_gas_used": observed["gas_used"],
    }
    header = {
        "number": 200,
        "hash": BLOCK,
        "parent_hash": ANCHOR,
        "timestamp": block["timestamp"],
        "context": block,
        "gas_used": observed["gas_used"],
        "blob_gas_used": 0,
        "beacon_root": BEACON_ROOT,
        "transaction_count": 1,
    }
    metadata = {
        "kind": "replay",
        "anchor": anchor,
        "block": block,
        "beacon_root": BEACON_ROOT,
        "prefix": [],
        "target_record": {"identity": identity, "request": request, "receipt": expected},
        "parent_header": {
            "number": 199,
            "hash": ANCHOR,
            "parent_hash": "0x" + "dd" * 32,
            "timestamp": 1710338147,
        },
        "target_header": header,
    }
    plan = ReplayPlan(copy.deepcopy(metadata), witness=witness)
    baseline = plan.run_baseline()
    write_new("build-info.json", build)
    save_capture(ROOT / "source-replay.json", plan, baseline)
    print(
        "Synthetic fixture generated from the same local engine, not independent mainnet evidence"
    )
    print(json.dumps(observed, indent=2))


if __name__ == "__main__":
    main()
