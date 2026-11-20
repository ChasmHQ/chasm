"""Synthetic fork-backed experiments, not historical mainnet parity evidence."""

from __future__ import annotations

import json
from types import SimpleNamespace

import pytest
from chasm_debugger._revm import RevmChain
from eth_hash.auto import keccak
from eth_utils import function_signature_to_4byte_selector
from harness import project as fixture_project

from chasm_debugger.commands import CommandProcessor
from chasm_debugger.evaluate import Evaluator, make_eval_hook
from chasm_debugger.fork import CANCUN_TIMESTAMP, MAINNET_GENESIS
from chasm_debugger.session import DebugSession, Finished, Paused, StepMode

CALLER = "0x" + "11" * 20
BANK = "0x" + "22" * 20
CALLEE = "0x" + "24" * 20
IMPLEMENTATION = "0x" + "25" * 20
BENEFICIARY = "0x" + "33" * 20
ANCHOR = "0x" + "aa" * 32
BALANCE_SLOT = int.from_bytes(
    keccak(bytes.fromhex(CALLER[2:]).rjust(32, b"\x00") + (2).to_bytes(32, "big")), "big"
)
HISTORY_ITEM = int.from_bytes(keccak((4).to_bytes(32, "big")), "big")


@pytest.fixture(params=[False, True], ids=["direct", "delegate-proxy"])
def fork_bank(request):
    project = fixture_project()
    artifact = project.artifact("Bank")
    proxy = request.param
    anchor = {
        "chain_id": 1,
        "number": 200,
        "hash": ANCHOR,
        "genesis_hash": MAINNET_GENESIS,
    }
    witness = {
        "version": 1,
        "anchor": anchor,
        "accounts": {
            CALLER: {"balance": hex(10**18), "nonce": 0, "code": "0x"},
            BANK: {
                "balance": hex(10**18),
                "nonce": 1,
                "code": "0x" + artifact.deployed_bytecode.hex(),
            },
            BENEFICIARY: {"balance": "0x0", "nonce": 0, "code": "0x"},
            CALLEE: {
                "balance": "0x0",
                "nonce": 1,
                "code": "0x" + project.artifact("Callee").deployed_bytecode.hex(),
            },
        },
        "storage": {
            BANK: {
                "0x0": hex(int(CALLER, 16) | (25 << 160)),
                "0x1": hex(100),
                hex(BALANCE_SLOT): hex(100),
                "0x4": "0x0",
                hex(HISTORY_ITEM): "0x0",
            },
            CALLEE: {"0x0": "0x0"},
        },
        "block_hashes": {"200": ANCHOR},
        "headers": {},
    }
    if proxy:
        witness["accounts"][BANK]["code"] = (
            "0x363d3d373d3d3d363d73"
            + IMPLEMENTATION[2:]
            + "5af43d82803e903d91602b57fd5bf3"
        )
        witness["accounts"][IMPLEMENTATION] = {
            "balance": "0x0",
            "nonce": 1,
            "code": "0x" + artifact.deployed_bytecode.hex(),
        }
    config = {
        "rpc_url": None,
        "anchor": anchor,
        "block": {
            "number": 201,
            "timestamp": CANCUN_TIMESTAMP + 24,
            "beneficiary": BENEFICIARY,
            "gas_limit": 30_000_000,
            "base_fee": "0x1",
            "prevrandao": "0x" + "44" * 32,
            "excess_blob_gas": 0,
        },
        "strict": True,
        "witness": witness,
    }
    target = {
        "caller": CALLER,
        "to": BANK,
        "data": "0x" + function_signature_to_4byte_selector("deposit()").hex(),
        "value": hex(20),
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
    plan = SimpleNamespace(
        target=target,
        identity=anchor,
        report={"kind": "fork", "anchor": anchor, "sandbox": True},
        baseline_report=None,
        new_chain=lambda: RevmChain(json.dumps(config)),
    )
    session = DebugSession.from_fork_plan(project, plan)
    evaluator = Evaluator(project)
    session.set_eval_hook(make_eval_hook(evaluator))
    commands = CommandProcessor(session, evaluator)
    session.start()
    assert isinstance(session.wait(10), Paused)
    session.break_at_function("Bank._credit")
    assert isinstance(session.resume(StepMode.RUN, timeout=10), Paused)
    assert session.last_snapshot.address.hex() == BANK[2:]
    assert (
        session.last_snapshot.code_address.hex()
        == (IMPLEMENTATION if proxy else BANK)[2:]
    )
    try:
        yield session, commands
    finally:
        session.detach()


def command_ok(commands, text):
    result = commands.execute(text)
    assert result.ok, result.error
    return result


def test_fork_mapping_packed_field_discard_and_keep(fork_bank):
    session, commands = fork_bank
    assert session.inspect("evaluate", "feeBps").value == 25
    assert session.inspect("evaluate", "balances[msg.sender]").value == 100
    assert session.inspect("evaluate", "_fee(10000)").value == 25
    session.inspect("evaluate", "feeBps = 1000", keep=False)
    assert session.inspect("evaluate", "feeBps").value == 25
    assert not session.diverged
    command_ok(commands, "set var feeBps = 1000")
    assert session.inspect("evaluate", "feeBps").value == 1000
    assert session.inspect("evaluate", "owner").value.lower() == CALLER
    command_ok(commands, "set var balances[msg.sender] = 200")
    assert session.inspect("evaluate", "balances[msg.sender]").value == 200
    assert session.diverged
    assert isinstance(session.resume(StepMode.RUN, timeout=10), Finished)
    assert int(session._chain.read_storage_at(BANK, BALANCE_SLOT), 16) == 218
    assert int(session._chain.read_storage_at(BANK, 1), 16) == 118


def test_fork_prompt_yul_and_cheatcode_stay_local(fork_bank):
    session, commands = fork_bank
    original_time = int(session._chain.read_timestamp(), 16)
    command_ok(commands, "vm.warp(12345)")
    assert int(session._chain.read_timestamp(), 16) == 12345
    assert original_time != 12345
    command_ok(commands, "sstore(1, 777)")
    assert int(session._chain.read_storage_at(BANK, 1), 16) == 777
    assert session.diverged
    assert session.inspect("evaluate", "totalDeposits").value == 777


def test_fork_discarded_state_leaves_continuation_unchanged(fork_bank):
    session, _commands = fork_bank
    session.inspect("evaluate", "balances[msg.sender] = 99999", keep=False)
    assert not session.diverged
    assert isinstance(session.resume(StepMode.RUN, timeout=10), Finished)
    assert int(session._chain.read_storage_at(BANK, BALANCE_SLOT), 16) == 120
    assert int(session._chain.read_storage_at(BANK, 1), 16) == 120


def test_fork_nested_solidity_call_discard_then_keep(fork_bank):
    session, _commands = fork_bank
    expression = f"forward(address({CALLEE}), 9)"
    before = session._chain.snapshot()
    discarded = session.inspect("evaluate", expression, keep=False)
    assert discarded.value == 18
    assert int(session._chain.read_storage_at(CALLEE, 0), 16) == 0
    after = session._chain.snapshot()
    assert after["pc"] == before["pc"]
    assert after["gas_remaining"] == before["gas_remaining"]
    assert not session.diverged
    kept = session.inspect("evaluate", expression, keep=True)
    assert kept.value == 18
    assert int(session._chain.read_storage_at(CALLEE, 0), 16) == 9
    assert session.diverged
