"""Client for Chasm's local control API, shared by the live flows.

A debug build started with `--control-token-file FILE` writes the endpoint
and a Full-tier token to FILE. Every flow reads it from CHASM_TOKEN_FILE.
"""
import base64
import itertools
import json
import os
import sys
import time
import urllib.error
import urllib.request

TOKEN_FILE = os.environ.get("CHASM_TOKEN_FILE", "")
_counter = itertools.count()


def _credentials():
    if not TOKEN_FILE:
        raise SystemExit("Set CHASM_TOKEN_FILE (run.py does this).")
    endpoint, token = open(TOKEN_FILE, encoding="utf-8").read().split()
    return endpoint, token


def call(path, body=None, timeout=60):
    endpoint, token = _credentials()
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(endpoint + path, data=data, method="POST" if body is not None else "GET")
    request.add_header("Authorization", "Bearer " + token)
    if body is not None:
        request.add_header("Content-Type", "application/json")
    # The Full tier allows 600 requests a minute; long flows wait it out.
    for _ in range(30):
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.loads(response.read())
        except urllib.error.HTTPError as error:
            out = json.loads(error.read() or b"{}")
            if out.get("error", {}).get("code") != "rate_limited":
                return out
        time.sleep(3)
    return out


def query(name):
    return call("/api/v1/query", {"query": name})


def command(action_id, /, **params):
    """Run a command; fails the flow unless it completed."""
    out = command_raw(action_id, **params)
    if out.get("status") != "completed":
        raise SystemExit(f"{action_id} failed: {json.dumps(out)[:400]}")
    return out


def command_raw(action_id, /, **params):
    body = {
        "requestId": f"live-{time.time_ns()}-{next(_counter)}",
        "context": query("app.state")["context"],
        "command": {"actionId": action_id, "params": params},
    }
    return call("/api/v1/command", body)


def settle(seconds=60):
    """Wait until no deploy/call/chain/wallet work is running."""
    for _ in range(seconds * 2):
        if not any(query("app.state")["busy"].values()):
            return
        time.sleep(0.5)
    raise SystemExit("timed out waiting for busy flags")


def compiled(seconds=240):
    """Wait for a compile to finish; returns the compile label."""
    time.sleep(1)
    for _ in range(seconds):
        label = query("toolchain.state")["compile"]
        if not label.startswith("Compiling"):
            return label
        time.sleep(1)
    raise SystemExit("timed out waiting for the compile")


def trace():
    return query("debugger.state")["debugger"]["trace"]


def screenshot(path):
    shot = query("ui.screenshot").get("screenshot")
    if shot:
        open(path, "wb").write(base64.b64decode(shot["pngBase64"]))


def rpc(method, params=None, url="http://127.0.0.1:8545"):
    request = urllib.request.Request(url, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}).encode(), headers={"Content-Type": "application/json", "User-Agent": "chasm-live-test"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read())["result"]


def function_id(name):
    return next(f["id"] for f in query("contracts.view")["functions"] if f["name"] == name)


# Anvil development account 0: a public, well-known test address.
OWNER = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"


def anvil_key_0():
    """Account 0's key, derived from Anvil's public test mnemonic with cast so
    no key is written in the repo. Used only to assert it never leaks."""
    import subprocess
    out = subprocess.run(["cast", "wallet", "private-key", "test test test test test test test test test test test junk", "0"], capture_output=True, text=True, check=True)
    return out.stdout.strip().removeprefix("0x").lower()


def check(label, ok, detail=""):
    print(("  PASS " if ok else "  FAIL ") + label + (f": {detail}" if detail and not ok else ""))
    if not ok:
        raise SystemExit(1)

