"""Opt-in Base smoke test with a public, read-only upstream RPC."""
import os
import time

from chasm import check, command, command_raw, query, rpc, trace
from networks import _wait_profile

BASE = "https://base-rpc.publicnode.com"
WETH = "0x4200000000000000000000000000000000000006"


def base_fork():
    """Fork Base with automatic OP Stack mode and read WETH on the local node."""
    if os.environ.get("CHASM_SKIP_FORK") == "1":
        print("  SKIP base_fork (CHASM_SKIP_FORK=1)")
        return
    url = os.environ.get("CHASM_BASE_RPC", BASE)
    check("the Base RPC reports chain ID 8453", rpc("eth_chainId", url=url) == "0x2105")
    if (query("toolchain.state")["network"]["profile"] or "local") != "local":
        command_raw("network.switch", id="local")
    _wait_profile("local")
    command("network.save", profile={"id": "base", "name": "Base", "fork_url": url})
    command_raw("network.switch", id="base")
    try:
        network = _wait_profile("base", seconds=120)
        check("Base uses OP Stack mode automatically", network["chainKind"] == "op-stack" and network["optimism"] is True)
        check("the Base fork still uses chain ID 31337 in Local mode", network["sourceChainId"] == 8453 and rpc("eth_chainId") == "0x7a69")
        check("WETH bytecode is readable", len(rpc("eth_getCode", [WETH, "latest"])) > 1000)
        result = rpc("eth_call", [{"to": WETH, "data": "0x313ce567"}, "latest"])
        check("WETH decimals are readable", int(result, 16) == 18)
        block = hex(network["identity"]["fork_block"])
        header = rpc("eth_getBlockByNumber", [block, True])
        check("the fork serves OP deposit transactions", any(tx.get("type") == "0x7e" for tx in header["transactions"]))
        command("debugger.trace", call={"to": WETH, "data": "0x313ce567"})
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            state = trace()
            if not state["busy"]:
                break
            time.sleep(0.5)
        check("the unsigned WETH read traces on the fork", not state["busy"] and state["loaded"], state["status"])
        check("tracing a read does not advance the fork", rpc("eth_blockNumber") == block)
    finally:
        if (query("toolchain.state")["network"]["profile"] or "local") == "base":
            command_raw("network.switch", id="local")
            _wait_profile("local")
        command("network.remove", id="base")
