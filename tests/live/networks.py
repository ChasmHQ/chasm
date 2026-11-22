"""Named profiles over the control API: switch with preflight and resume,
per-network data and imports, Source mode, and a refused bad RPC.

Needs the public RPC; the runner skips it with CHASM_SKIP_FORK=1.
"""
import json
import os
import time

from chasm import check, command, command_raw, query, rpc, settle

MAINNET = "https://ethereum-rpc.publicnode.com"
SEPOLIA = "https://ethereum-sepolia-rpc.publicnode.com"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
CHAIN_ID_SELECTOR = "0x9a8a0592"  # chainId()


def _wait_profile(profile, mode=None, seconds=300):
    """Wait until the workspace is ready on `profile` ("" or "local" = local),
    and, when `mode` is given, its chain id mode is that one."""
    for _ in range(seconds):
        try:
            state = query("app.state")
            network = query("toolchain.state")["network"]
            running = network["profile"] or "local"
            if state.get("workspaceReady") and not any(state["busy"].values()) and running == profile and (mode is None or network.get("chainIdMode") == mode):
                return network
        except Exception:
            pass
        time.sleep(1)
    raise SystemExit(f"the workspace did not reopen on {profile}")


def _read(function, arguments):
    command("contracts.function.select", id=function)
    command("contracts.request.edit", arguments=json.dumps(arguments))
    command("contracts.simulate")
    settle()
    return query("contracts.view")["simple"]


def _bank_balance(address, owner):
    """Read balanceOf(owner) on the restored Bank. Deployment and function ids
    are workspace-scoped, so both are resolved again after every switch."""
    deployment = next(d for d in query("app.state")["deployments"] if d["address"] == address)
    command("deployments.select", id=deployment["id"])
    balance_of = next(f["id"] for f in query("contracts.view")["functions"] if f["name"] == "balanceOf")
    return _read(balance_of, [owner])


def networks():
    """Profiles switch with preflight and resume: data, imports and chain id follow."""
    if os.environ.get("CHASM_SKIP_FORK") == "1":
        print("  SKIP networks (CHASM_SKIP_FORK=1)")
        return

    # The previous flow may have stopped on a fork; start from the local chain.
    if (query("toolchain.state")["network"]["profile"] or "local") != "local":
        command_raw("network.switch", id="local")
        _wait_profile("local")
    command("network.save", profile={"id": "mainnet", "name": "Ethereum mainnet", "fork_url": MAINNET})
    listing = command("network.list")
    check("the mainnet profile is saved and not active", listing["active"] == "local" and any(p["id"] == "mainnet" and not p["active"] for p in listing["profiles"]), json.dumps(listing)[:200])

    # A local Bank from the earlier flows, with one deposit on the local chain.
    _wait_profile("local")
    command("wallet.dev_account", index=1)
    owner = query("app.state")["wallet"]["address"]
    bank = next((d for d in query("app.state")["deployments"] if d["name"] == "Bank"), None)
    check("a local Bank is available for the network test", bank is not None, json.dumps(query("app.state")["deployments"])[:200])
    bank_address = bank["address"]
    command("deployments.select", id=bank["id"])
    deposit = next(f["id"] for f in query("contracts.view")["functions"] if f["name"] == "deposit")
    command("contracts.function.select", id=deposit)
    command("contracts.request.edit", value=str(5 * 10**17))
    command("contracts.send")
    settle()
    check("the deposit is on the local chain", "500000000000000000" in _bank_balance(bank_address, owner), query("contracts.view")["simple"][-120:])
    entries = query("history.list")["entries"]
    check("history records this network", any(e.get("network", "").startswith("local:") for e in entries), json.dumps(entries[:1])[:200])

    command_raw("network.switch", id="mainnet")
    network = _wait_profile("mainnet")
    check("mainnet runs after the preflight", (network["forkChain"] or "") == "Ethereum (1)", json.dumps(network)[:200])
    check("USDC code is readable on the fork", len(rpc("eth_getCode", [USDC, "latest"])) > 1000)
    check("the local Bank is absent on mainnet", all(d["address"] != bank_address for d in query("app.state")["deployments"]), json.dumps(query("app.state")["deployments"])[:200])
    entries = query("history.list")["entries"]
    check("history is filtered to the running network", all(e.get("network", "").startswith("mainnet:") for e in entries), json.dumps(entries[:1])[:200])

    command("contracts.import", address=USDC)
    view = {}
    for _ in range(120):
        view = query("contracts.view")
        if view.get("abiOrigin"):
            break
        time.sleep(0.5)
    check("USDC imports on mainnet", "Sourcify" in view.get("abiOrigin", ""), json.dumps(view.get("abiOrigin"))[:200])
    imported_name = next((name for name in _artifact_names() if "@" in name and "ChainIdProbe" not in name), None)
    check("the import is listed under the fork", imported_name is not None, str(_artifact_names()[:6]))

    command_raw("network.switch", id="local")
    _wait_profile("local")
    print("  | after resume:", json.dumps({"notice": query("app.state").get("notice", "")[:200], "network": query("toolchain.state")["network"].get("profile"), "profiles": [p.get("savedState") for p in command("network.list")["profiles"]]})[:400])
    check("the local Bank is back after resume", any(d["address"] == bank_address for d in query("app.state")["deployments"]), json.dumps(query("app.state")["deployments"])[:200])
    check("the signer choice is restored", query("app.state")["wallet"]["address"] == owner, query("app.state")["wallet"]["address"])
    check("the chain state came back with the checkpoint", "500000000000000000" in _bank_balance(bank_address, owner), query("contracts.view")["simple"][-120:])
    check("a mainnet import is not listed on the local chain", imported_name not in _artifact_names(), str(_artifact_names()[:6]))

    command_raw("network.switch", id="mainnet")
    _wait_profile("mainnet")
    check("the mainnet import comes back with resume", imported_name in _artifact_names(), str(_artifact_names()[:6]))

    # Source mode: the node runs mainnet's real id, and the probe reports it.
    # Saving the active profile tests it and reopens, so it is accepted first.
    saved = command_raw("network.save", profile={"id": "mainnet", "name": "Ethereum mainnet", "fork_url": MAINNET, "chain_id_mode": "source"})
    check("the active profile edit is staged behind a preflight", saved.get("effect") == "reopen_pending", json.dumps(saved)[:200])
    network = _wait_profile("mainnet", mode="source")
    check("the Source profile reports chain id 1", network["executionChainId"] == 1 and network["keystoresBlocked"] is True, json.dumps(network)[:200])
    probe = next(a for a in query("workspace.artifacts")["artifacts"] if a["name"] == "ChainIdProbe")
    command("wallet.dev_account", index=0)
    command("contracts.select", id=probe["id"])
    deployed = command_raw("contracts.deploy")
    settle()
    if deployed.get("status") != "completed":
        print("  | deploy:", json.dumps(deployed)[:300], "| notice:", query("app.state").get("notice", "")[:300])
    probe_address = ""
    for _ in range(120):
        probe_address = query("contracts.view")["address"]
        if len(probe_address) == 42 and probe_address.startswith("0x"):
            break
        time.sleep(0.5)
    view = query("contracts.view")
    if len(probe_address) != 42:
        import re
        match = re.search(r"Transaction hash: (0x[0-9a-fA-F]{64})", view.get("callStatus", ""))
        if match:
            receipt = rpc("eth_getTransactionReceipt", [match.group(1)])
            header = rpc("eth_getBlockByNumber", [receipt.get("blockNumber"), False]) if receipt else None
            print("  | receipt blockHash:", (receipt or {}).get("blockHash"), "| node header hash:", (header or {}).get("hash"), "| chain:", rpc("eth_chainId"))
    check("ChainIdProbe deploys on a Source profile with a dev key", len(probe_address) == 42, json.dumps(view)[:200])
    chain_id = int(rpc("eth_call", [{"to": probe_address, "data": CHAIN_ID_SELECTOR}, "latest"]), 16)
    check("the ChainIdProbe reports mainnet's own id", chain_id == 1, str(chain_id))

    command_raw("network.switch", id="local")
    _wait_profile("local")
    command("network.save", profile={"id": "mainnet", "name": "Ethereum mainnet", "fork_url": MAINNET})

    # A bad URL is refused by the preflight; the running network is untouched.
    command("network.save", profile={"id": "broken", "name": "Broken", "fork_url": "https://chasm-probe.invalid"})
    command_raw("network.switch", id="broken")
    status_text = ""
    for _ in range(90):
        time.sleep(1)
        status_text = query("toolchain.state").get("status", "")
        if "Not switched" in status_text:
            break
    check("the bad RPC is refused before anything changes", "Not switched" in status_text and "keeps running" in status_text, status_text[:220])
    check("the local chain keeps running", (query("toolchain.state")["network"]["profile"] or "local") == "local")
    command("network.remove", id="broken")

    # Sepolia is first-class: a Local-mode fork reports 31337 here, 11155111
    # upstream. A public RPC sometimes refuses under load; the preflight then
    # keeps the current network running, which is the designed behaviour, so
    # this optional check skips instead of failing the suite.
    command("network.save", profile={"id": "sepolia", "name": "Sepolia", "fork_url": SEPOLIA})
    command_raw("network.switch", id="sepolia")
    network = None
    for _ in range(180):
        try:
            state = query("app.state")
            candidate = query("toolchain.state")["network"]
            if state.get("workspaceReady") and (candidate["profile"] or "local") == "sepolia":
                network = candidate
                break
            if "Not switched" in query("toolchain.state").get("status", ""):
                break
        except Exception:
            pass
        time.sleep(1)
    if network is None:
        print(f"  SKIP sepolia check (the RPC refused this run: {query('toolchain.state').get('status', '')[:120]})")
        command_raw("network.switch", id="local")
        _wait_profile("local")
        command("network.remove", id="sepolia")
    else:
        check("Sepolia forks with the local execution id", network["sourceChainId"] == 11155111 and rpc("eth_chainId") == "0x7a69", json.dumps(network)[:200])
    command("toolchain.action", action="test_rpc", fork_url=SEPOLIA)
    status_text = ""
    for _ in range(90):
        time.sleep(1)
        status_text = query("toolchain.state").get("status", "")
        if "chain kind" in status_text or "did not pass" in status_text:
            break
    check("Sepolia is reported as Ethereum", "chain kind: Ethereum" in status_text, status_text[:300])
    command_raw("network.switch", id="local")
    _wait_profile("local")
    command("network.remove", id="sepolia")


def _artifact_names():
    return [a["name"] for a in query("workspace.artifacts")["artifacts"]]
