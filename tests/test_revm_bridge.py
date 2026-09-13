"""The REVM spike crosses the Python boundary without owning the GIL."""

from __future__ import annotations

import json
import subprocess
import sys
import threading
import time

from eth_utils import function_signature_to_4byte_selector

from sevm._revm import RevmChain, RevmSession, revm_version
from sevm.cheatcodes import CheatState, VM_ADDRESS, apply_cheat


def test_live_mutation_crosses_the_python_bridge():
    session = RevmSession(bytes.fromhex("60015f5500"), stop_pc=3)
    paused = session.wait()
    assert paused["type"] == "paused"
    assert paused["reason"] == "breakpoint"
    assert paused["step"] == 3
    assert paused["stack"] == ["0x0", "0x1"]
    assert paused["mnemonic"] == "SSTORE"
    assert paused["code_address"] == paused["address"]
    assert paused["caller"] == paused["origin"]
    assert paused["calldata"] == b""
    assert paused["frames"] == [
        {
            "depth": 0,
            "kind": "call",
            "address": paused["address"],
            "code_address": paused["address"],
            "caller": paused["caller"],
            "value": "0x0",
            "calldata": b"",
            "is_static": False,
            "pc": 3,
            "opcode": 0x55,
            "gas_limit": paused["gas_limit"],
            "gas_remaining": paused["gas_remaining"],
            "code": bytes.fromhex("60015f5500"),
        }
    ]

    assert session.set_stack(1, 9) == "0x9"
    assert session.write_memory(3, b"\xaa\xbb") == 2
    assert session.write_storage(7, 8) == "0x8"
    assert session.snapshot()["memory"][3:5] == b"\xaa\xbb"
    session.step()

    stepped = session.wait()
    assert stepped["type"] == "paused"
    assert stepped["reason"] == "step"
    assert stepped["step"] == 4
    assert stepped["pc"] == 4
    session.resume()

    finished = session.wait()
    assert finished["type"] == "finished"
    assert finished["success"]
    storage = {(address, key): value for address, key, value in finished["storage"]}
    target = "0x1000000000000000000000000000000000000001"
    assert storage[(target, "0x0")] == "0x9"
    assert storage[(target, "0x7")] == "0x8"


def test_wait_releases_the_gil():
    session = RevmSession(bytes.fromhex("5b00"), stop_pc=0)
    assert session.wait()["type"] == "paused"
    result = []
    waiter = threading.Thread(target=lambda: result.append(session.wait()))
    waiter.start()
    time.sleep(0.05)
    session.resume()
    waiter.join(timeout=2)

    assert not waiter.is_alive()
    assert result[0]["type"] == "finished"
    assert revm_version() == "43.0.2"


def test_installed_headless_launcher_uses_json_rpc():
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}},
    ]
    result = subprocess.run(
        [sys.executable, "-m", "sevm.revm_server"],
        input="".join(json.dumps(request) + "\n" for request in requests),
        capture_output=True,
        text=True,
        check=True,
    )
    responses = [json.loads(line) for line in result.stdout.splitlines()]
    assert result.stderr == ""
    assert responses[0]["result"]["protocol"] == "sevm-debugger/1"
    assert responses[1]["result"] is None


def test_persistent_chain_deploys_and_reuses_state():
    runtime = bytes.fromhex("5b" * 16 + "5f546001015f5500")
    init_code = bytes.fromhex("6018600a5f3960185ff3") + runtime
    chain = RevmChain()
    chain.create(init_code)
    deployment = chain.wait()
    address = deployment["created_address"]
    assert deployment["success"] and address
    assert chain.set_breakpoints([(address, 22)]) == 1

    for before, expected in (("0x0", "0x1"), ("0x1", "0x2")):
        chain.call(address)
        paused = chain.wait()
        assert paused["type"] == "paused"
        assert paused["pc"] == 22
        assert chain.read_storage(0) == before
        if before == "0x0":
            assert chain.write_memory(3, b"\xaa\xbb") == 2
            assert chain.snapshot()["memory"][3:5] == b"\xaa\xbb"
            assert chain.write_storage(7, 8) == "0x8"
            assert chain.evaluate(bytes.fromhex("602a5f5260205ff3"))[-1] == 42
            assert chain.write_balance(address, 99) == "0x63"
            assert chain.read_balance(address) == "0x63"
            assert chain.read_code_at(address) == runtime
            assert chain.write_nonce(address, 12) == 12
            assert chain.read_nonce(address) == 12
            assert chain.write_storage_at(address, 9, 10) == "0xa"
            assert chain.read_storage_at(address, 9) == "0xa"
            assert chain.write_transient(address, 11, 12) == "0xc"
            assert chain.read_transient(address, 11) == "0xc"
            chain.warm_storage(address, 13)
            assert chain.write_block_number(100) == "0x64"
            assert chain.read_block_number() == "0x64"
            assert chain.write_timestamp(200) == "0xc8"
            assert chain.read_timestamp() == "0xc8"
            assert chain.write_base_fee(300) == 300
            assert chain.read_base_fee() == 300
            assert chain.write_base_fee(0) == 0
            assert chain.write_chain_id(400) == 400
            assert chain.read_chain_id() == 400
            assert chain.write_chain_id(1) == 1
            assert chain.write_coinbase(address) == address
            assert chain.read_coinbase() == address
            randao = bytes.fromhex("44" * 32)
            assert chain.write_prevrandao(randao) == randao
            assert chain.read_prevrandao() == randao
            assert chain.write_difficulty(500) == "0x1f4"
            assert chain.read_difficulty() == "0x1f4"
        chain.resume()
        finished = chain.wait()
        storage = {(addr, key): value for addr, key, value in finished["storage"]}
        assert storage[(address, "0x0")] == expected


def test_foundry_host_call_crosses_the_python_bridge():
    selector = function_signature_to_4byte_selector("assertTrue(bool)")
    selector_word = selector + bytes(28)
    code = (
        b"\x7f"
        + selector_word
        + bytes.fromhex("5f5260016004525f5f60245f5f73")
        + VM_ADDRESS
        + bytes.fromhex("61c350f15000")
    )
    session = RevmSession(code, stop_pc=0)
    assert session.wait()["type"] == "paused"
    session.resume()

    host_call = session.wait()
    assert host_call["type"] == "host_call"
    assert bytes.fromhex(host_call["address"][2:]) == VM_ADDRESS
    output = apply_cheat(CheatState(), None, host_call["data"], host_call["caller"])
    session.respond_host(output)
    assert session.wait()["success"]
