"""Windows close/reopen regression. Both nodes and all state are disposable.

Run after the debug build, with ports 3000 and 8545 free. Does not contact
Sepolia, sign a transaction, or inspect a dump or pairing token.
"""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error

import chasm

EXE = Path(__file__).resolve().parents[2] / "target/debug/chasm.exe"
ACCOUNT = "0x00000000000000000000000000000000000000aa"


def wait(label, check):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        try:
            value = check()
        except (FileNotFoundError, ConnectionError, urllib.error.URLError, ValueError, KeyError):
            value = None
        if value:
            return value
        time.sleep(0.5)
    raise AssertionError(f"Timed out: {label}")


def ready(workspace_id=None):
    state = chasm.query("app.state")
    return state if state.get("workspaceReady") and state["context"]["workspaceId"] != workspace_id else None


def switch(profile):
    old = ready()["context"]["workspaceId"]
    result = chasm.command_raw("network.switch", id=profile)
    assert result.get("effect") == "reopen_pending", result.get("status")
    wait(f"new workspace for {profile}", lambda: ready(old))
    network = chasm.query("toolchain.state")["network"]
    assert (network["profile"] or "local") == profile, f"Expected {profile}; got {network['profile'] or 'local'}"
    return network


def close(app):
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    f"if (!(Get-Process -Id {app.pid}).CloseMainWindow()) {{ exit 1 }}"], check=True)
    assert app.wait(timeout=90) == 0, "The window did not close normally"


def run(root):
    work = root / "project"
    work.mkdir()
    (work / "Counter.sol").write_text("// SPDX-License-Identifier: MIT\npragma solidity ^0.8.20; contract Counter { uint public value; }\n", encoding="utf-8")
    (work / "ChainIdProbe.sol").write_text(
        chr(10).join([
            "// SPDX-License-Identifier: MIT",
            "pragma solidity ^0.8.20;",
            "contract ChainIdProbe { function chainId() external view returns (uint256) { return block.chainid; } }",
            "",
        ]),
        encoding="utf-8",
    )
    token = root / "control"
    chasm.TOKEN_FILE = str(token)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    upstream = subprocess.Popen(["anvil", "--host", "127.0.0.1", "--port", str(port), "--chain-id", "11155111"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    app = None
    try:
        wait("upstream ready", lambda: chasm.rpc("eth_chainId", url=url) == "0xaa36a7")
        chasm.rpc("evm_mine", url=url)
        block = chasm.rpc("eth_getBlockByNumber", ["latest", False], url=url)
        def launch():
            return subprocess.Popen([EXE, work, "--control-token-file", token], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        app = launch()
        wait("initial workspace", ready)
        chasm.rpc("anvil_setBalance", [ACCOUNT, "0x2a"])
        chasm.command("network.save", profile={"id": "fixture", "fork_url": url, "fork_block": int(block["number"], 16)})
        listing = chasm.command("network.list")
        assert listing["active"] == "local" and [p["host"] for p in listing["profiles"]] == ["127.0.0.1"]
        # This upstream has no chain above its head. A refused preflight or
        # a failed boot must leave us on local, with the original balance.
        chasm.command("network.save", profile={"id": "unavailable", "fork_url": url, "fork_block": int(block["number"], 16) + 100})
        pending = chasm.command_raw("network.switch", id="unavailable")
        assert pending.get("effect") == "reopen_pending", pending.get("status")
        def refused():
            state = ready()
            if not state:
                return False
            message = (state.get("notice", "") + " " + chasm.query("toolchain.state").get("status", "")).lower()
            return "not switched" in message or "failed" in message or "did not pass" in message
        wait("unavailable block reported", refused)
        assert (chasm.query("toolchain.state")["network"]["profile"] or "local") == "local"
        assert int(chasm.rpc("eth_getBalance", [ACCOUNT, "latest"]), 16) == 42
        fork = switch("fixture")
        assert fork["identity"]["anchor_hash"] == block["hash"] and fork["sourceChainId"] == 11155111
        assert not (work / ".chasm/network-switch.json").exists()
        assert int(chasm.rpc("eth_getBalance", [ACCOUNT, "latest"]), 16) == 0
        # N4: Local mode reports 31337; a Source profile runs the forked
        # chain's id, and a dev key can still deploy there.
        chasm.command("wallet.dev_account", index=0)
        probe = next(a for a in chasm.query("workspace.artifacts")["artifacts"] if a["name"] == "ChainIdProbe")
        chasm.command("contracts.select", id=probe["id"])
        chasm.command("contracts.deploy")
        wait("local ChainIdProbe deployed", lambda: chasm.query("contracts.view")["address"].startswith("0x"))
        local_probe = chasm.query("contracts.view")["address"]
        assert int(chasm.rpc("eth_call", [{"to": local_probe, "data": "0x9a8a0592"}, "latest"]), 16) == 31337
        chasm.command("network.save", profile={"id": "source", "fork_url": url, "fork_block": int(block["number"], 16), "chain_id_mode": "source"})
        source = switch("source")
        assert source["identity"]["execution_chain_id"] == 11155111, source["identity"]
        assert source.get("keystoresBlocked") is True, source
        chasm.command("wallet.dev_account", index=0)
        # Artifact ids are workspace-scoped: read them again after a switch.
        probe = next(a for a in chasm.query("workspace.artifacts")["artifacts"] if a["name"] == "ChainIdProbe")
        chasm.command("contracts.select", id=probe["id"])
        chasm.command("contracts.deploy")
        wait("source ChainIdProbe deployed", lambda: chasm.query("contracts.view")["address"].startswith("0x"))
        source_probe = chasm.query("contracts.view")["address"]
        assert int(chasm.rpc("eth_call", [{"to": source_probe, "data": "0x9a8a0592"}, "latest"]), 16) == 11155111
        print("PASS: a Source profile runs the real chain id and a dev key can deploy on it")
        switch("local")
        assert int(chasm.rpc("eth_getBalance", [ACCOUNT, "latest"]), 16) == 42
        chasm.rpc("anvil_setBalance", [ACCOUNT, "0x2b"])
        close(app)
        app = launch()
        wait("workspace after normal close", ready)
        assert int(chasm.rpc("eth_getBalance", [ACCOUNT, "latest"]), 16) == 43
        close(app)
        # Keep the approval pending in a separate disposable process. The
        # test must not approve a clean fork on the user's behalf.
        app = launch()
        wait("workspace for fresh request", ready)
        fresh = chasm.command_raw("network.switch", id="local", mode="fresh")
        assert fresh.get("status") == "pending_approval", fresh.get("status")
        assert int(chasm.rpc("eth_getBalance", [ACCOUNT, "latest"]), 16) == 43
        print("PASS: failed target, pinned anchor, one-shot marker, isolated state, resume, normal close and fresh approval gate")
    finally:
        for process in (app, upstream):
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=15)


if __name__ == "__main__":
    if os.name != "nt":
        raise SystemExit("This close-window test requires Windows.")
    with tempfile.TemporaryDirectory(prefix="chasm-lifecycle-") as directory:
        run(Path(directory))
