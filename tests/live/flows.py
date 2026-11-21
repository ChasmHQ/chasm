"""Live flows over the control API, run in order by run.py.

Each flow starts from the state the previous one left; `bank` deploys the
sample Bank contract first. A flow fails by raising SystemExit through check().
"""
import json
import os
import time

from chasm import (OWNER, anvil_key_0, check, command, command_raw, compiled, function_id,
                   query, rpc, screenshot, settle, trace)

WORKSPACE = os.environ.get("CHASM_WORKSPACE", "")


def _deposit(wei):
    command("contracts.function.select", id=function_id("deposit"))
    command("contracts.request.edit", value=str(wei))
    command("contracts.send")
    settle()
    view = query("contracts.view")
    check(f"deposit {wei}", "Transaction status: Success" in view["simple"], view["callStatus"])


def _balance():
    command("contracts.function.select", id=function_id("balanceOf"))
    command("contracts.request.edit", arguments=json.dumps([OWNER]))
    command("contracts.simulate")
    settle()
    view = query("contracts.view")
    check("balanceOf read", view["selectedName"].startswith("balanceOf"), view["callStatus"])
    return view["simple"].splitlines()[-1]


def bank():
    """Deploy Bank, deposit, trace, read, snapshot and revert; no key leaks."""
    for _ in range(120):
        if any(a["name"] == "Bank" for a in query("workspace.artifacts").get("artifacts", [])):
            break
        time.sleep(0.5)
    artifact = next(a for a in query("workspace.artifacts")["artifacts"] if a["name"] == "Bank")
    command("contracts.select", id=artifact["id"])
    command("wallet.dev_account", index=0)
    command("contracts.deploy.open")
    command("contracts.constructor.edit", index=0, value=OWNER)
    command("contracts.constructor.edit", index=1, value="[7,13,42]")
    form = query("contracts.deploy_form")
    check("deploy form ready", form["ready"], form["blockReason"])
    command("contracts.deploy")
    settle()
    state = query("app.state")
    active = [d["title"] for d in state["documents"] if d["active"]]
    check("deployed and instance selected", state["status"].startswith("Deployed") and active == ["Bank"], f"{state['status']} {active}")
    _deposit(2 * 10**18)
    command("contracts.trace")
    time.sleep(2)
    check("trace loaded", "deposit()" in query("contracts.view")["trace"])
    check("balance after deposit", _balance().endswith("2000000000000000000"))
    command("chain.snapshot")
    settle()
    check("snapshot taken", query("app.state")["snapshotTaken"])
    _deposit(10**18)
    check("balance after second deposit", _balance().endswith("3000000000000000000"))
    command("chain.revert")
    settle()
    state = query("app.state")
    check("revert kept Bank", len(state["deployments"]) == 1, state["notice"])
    check("balance back after revert", _balance().endswith("2000000000000000000"))
    key = anvil_key_0()
    for name in ["app.state", "contracts.view", "contracts.deploy_form", "macros.state", "debugger.state", "jobs.list", "app.catalog", "history.list", "toolchain.state"]:
        check(f"{name} has no private key", key not in json.dumps(query(name)).lower())


def revert():
    """withdraw(50000) shows the decoded revert, broadcasts nothing, and debugs."""
    block = query("app.state")["blockNumber"]
    command("contracts.function.select", id=function_id("withdraw"))
    check("amount field flagged", query("contracts.view")["fields"][0]["amount"])
    command("contracts.request.edit", arguments=json.dumps(["50000"]))
    command("contracts.send")
    settle()
    view = query("contracts.view")
    status = view["callStatus"]
    check("revert reason shown", status.startswith("Reverted before sending") and "Minimum Withdrawal is 1 Ether" in status, status)
    check("Debug this call offered", view["canDebugCall"])
    check("nothing broadcast", query("app.state")["blockNumber"] == block)
    command("debugger.trace", hash=None)
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    check("reverted call traced", "Reverted call" in t["title"], t["status"])
    command("debugger.trace.seek", step=t["total"] - 1)
    line = trace()["sourceLine"]
    check("stops on the failing require", line and "Minimum Withdrawal" in line["text"], str(line))


def steps():
    """Step into/over/out/instruction and their reverses on the reverted call."""
    def step(action):
        command("debugger.trace.step", action=action)
        t = trace()
        return t["step"], t["location"]
    command("debugger.trace.seek", step=0)
    for _ in range(40):
        _, location = step("step")
        if "Bank.sol:25" in location or "Bank.sol:24" in location:
            break
    over = []
    for _ in range(10):
        _, location = step("next")
        over.append(location)
        if "Bank.sol:28" in location:
            break
    check("step over never stops in generated code", all("no source" not in loc for loc in over), str(over))
    check("step over reaches the failing require", any("Bank.sol:28" in loc for loc in over), str(over))
    back, _ = step("prev-next")
    now = trace()["step"]
    forward, _ = step("op")
    again, _ = step("op-back")
    check("instruction step and back are exact", forward == now + 1 and again == now, f"{now} {forward} {again}")
    command("debugger.trace.console", line="b 28")
    command("debugger.trace.console", line="goto 1")
    command("debugger.trace.console", line="c")
    check("console continue stops at the breakpoint", "Bank.sol:28" in trace()["console"].splitlines()[-1], trace()["console"][-200:])


def history():
    """Calls are saved; Repeat, To macro and Debug work from History."""
    entries = query("history.list")["entries"]
    check("history saved calls", len(entries) >= 4, str(len(entries)))
    reverted = next(i for i, e in enumerate(entries) if e["status"] == "reverted")
    check("revert reason recorded", "Minimum Withdrawal" in entries[reverted]["summary"])
    deposit = max(i for i, e in enumerate(entries) if e["function"] == "deposit()" and e["status"] == "ok")
    check("send keeps tx hash and calldata", entries[deposit]["tx"].startswith("0x") and entries[deposit]["data"] == "0xd0e30db0")
    command("history.action", action="repeat", index=reverted)
    time.sleep(1)
    view = query("contracts.view")
    check("repeat refills the request", view["selectedName"].startswith("withdraw") and "50000" in view["arguments"], view["arguments"])
    command("history.action", action="to_macro", index=deposit)
    time.sleep(1)
    summaries = query("macros.state")["macros"]["stepSummaries"]
    check("to macro adds a raw_send step", any("0xd0e30db0" in s for s in summaries), str(summaries))
    command("history.action", action="debug", index=deposit)
    for _ in range(30):
        t = trace()
        if t["loaded"] and not t["busy"] and t["title"].startswith("Transaction"):
            break
        time.sleep(0.5)
    check("debug opens the deposit transaction", t["title"].startswith("Transaction"), t["title"] + " " + t["status"])
    command("debugger.trace.view", tab=4)
    names = [v["name"] for v in trace()["variables"]]
    check("variables name the storage write", any(n.startswith("balanceOf[") for n in names), str(names))


def timeline():
    """Snapshots before each change; cheats; revert to a point drops later state."""
    command("contracts.function.select", id=function_id("deposit"))
    command("contracts.request.edit", value="1000")
    command("contracts.send")
    settle()
    check("a send leaves a snapshot", any("deposit" in p["label"] for p in query("app.state")["timeline"]))
    who = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
    before = int(rpc("eth_getBalance", [who, "latest"]), 16)
    command("chain.cheat", action="deal", address=who, value="123")
    settle(); time.sleep(1)
    check("deal sets the balance", int(rpc("eth_getBalance", [who, "latest"]), 16) == 123 * 10**18)
    ts = int(rpc("eth_getBlockByNumber", ["latest", False])["timestamp"], 16)
    command("chain.cheat", action="time", value="86400")
    settle(); time.sleep(1)
    check("time advances one day", int(rpc("eth_getBlockByNumber", ["latest", False])["timestamp"], 16) - ts >= 86400)
    points = query("app.state")["timeline"]
    row = next(i for i, p in enumerate(points) if p["label"].startswith("before deal"))
    command("chain.timeline.revert", index=row)
    settle(); time.sleep(1)
    check("revert to before deal restores the balance", int(rpc("eth_getBalance", [who, "latest"]), 16) == before)
    check("later points are gone", not any(p["label"].startswith(("before deal", "before advance")) for p in query("app.state")["timeline"]))


def toolchain():
    """Lock solc, back to Auto; install, use and remove a library."""
    state = query("toolchain.state")
    check("auto by default", state["solc"]["locked"] == "")
    installed = state["solc"]["installed"]
    version = next((v for v in installed if v.startswith("0.8.") and int(v.split(".")[2]) >= 13), None)
    if version:
        command("toolchain.action", action="lock_solc", version=version)
        check(f"locked to {version} compiles", "contract" in compiled())
        command("toolchain.action", action="lock_solc", version="")
        check("back to Auto compiles", "contract" in compiled())
    command("toolchain.action", action="install", library="solmate")
    for _ in range(180):
        status = query("toolchain.state")["status"]
        if status.startswith(("Installed", "Could not")):
            break
        time.sleep(1)
    check("solmate installed", status.startswith("Installed"), status)
    command("toolchain.action", action="use", library="solmate")
    time.sleep(1)
    lib = next(l for l in query("toolchain.state")["libraries"] if l["id"] == "solmate")
    check("solmate mapped in the project", bool(lib["used"]), str(lib))
    compiled()
    command("toolchain.action", action="remove", library="solmate")
    time.sleep(1)
    lib = next(l for l in query("toolchain.state")["libraries"] if l["id"] == "solmate")
    check("remove drops the remapping, keeps the download", lib["used"] is None and lib["installed"], str(lib))
    compiled()
    # Test RPC checks a node before it is saved as the fork; here, the local node.
    command("toolchain.action", action="test_rpc", fork_url="http://127.0.0.1:8545/?key=secret-path", fork_block="0")
    for _ in range(30):
        status = query("toolchain.state")["status"]
        if status.startswith(("RPC test", "Enter", "The fork")) or "did not answer" in status:
            break
        time.sleep(0.5)
    check("Test RPC reports each check", "ok  chain" in status and "fork block 0 is available" in status and "block 0 is served" in status, status)
    check("Test RPC never shows the URL path", "secret-path" not in status, status)
    check("Test RPC saves nothing", query("toolchain.state")["network"]["forkHost"] is None, str(query("toolchain.state")["network"]))


def palette():
    """Ctrl+K finds a deployed function and opens it."""
    command("ui.navigate", activity="explorer")
    command("ui.palette", query="bank dep", index=0)
    time.sleep(1)
    check("palette opens deposit()", query("contracts.view")["selectedName"].startswith("deposit"))


def macro():
    """A macro built only with form commands validates and runs."""
    bank = query("contracts.view")["address"]
    command("ui.navigate", activity="macros")
    command("macros.new")
    command("macros.step.add", kind="snapshot")
    command("macros.step.add", kind="call")
    command("macros.step.field", index=1, field="target", value=bank)
    command("macros.step.field", index=1, field="value", value="1000000000000000000")
    command("macros.step.encode", index=1, contract="Bank", function="deposit", arguments="", send=True)
    command("macros.step.add", kind="call")
    command("macros.step.field", index=2, field="target", value=bank)
    command("macros.step.encode", index=2, contract="Bank", function="balanceOf", arguments=json.dumps([OWNER]), send=False)
    command("macros.step.add", kind="assert")
    command("macros.step.field", index=3, field="left", value="${step.send_2.returnData}")
    command("macros.step.add", kind="revert")
    command("macros.validate")
    state = query("macros.state")["macros"]
    check("validated", state["validationCurrent"] and not state["validationFailed"], state["validation"] or state["error"])
    command("macros.run", dry_run=False)
    for _ in range(60):
        state = query("macros.state")["macros"]
        if not state["runBusy"] and state["results"]:
            break
        time.sleep(1)
    statuses = [step["status"] for step in state["steps"]]
    check("steps 1-4 succeeded", statuses[:4] == ["Succeeded"] * 4, str(statuses))
    command("macros.result.select", index=0)
    simple = query("macros.state")["macros"]["resultSimple"]
    print("  | " + " || ".join(simple.splitlines())[:400])
    check("the result says what reached the chain", "confirmed on the workspace chain" in simple and "revert" in simple.lower(), simple[:300])



def bundle():
    """Bundle mode: two deposits mined in one block; a failing step rolls the chain back."""
    bank = next(d for d in query("app.state")["documents"] if d["title"] == "Bank")
    command("ui.document.select", id=bank["id"])
    bank_address = query("contracts.view")["address"]
    command("wallet.dev_account", index=0)
    command("ui.navigate", activity="macros")

    def build(steps):
        command("macros.new")
        for index, (kind, value) in enumerate(steps):
            command("macros.step.add", kind="call")
            command("macros.step.field", index=index, field="target", value=bank_address)
            if kind == "deposit":
                command("macros.step.field", index=index, field="value", value=str(value))
                command("macros.step.encode", index=index, contract="Bank", function="deposit", arguments="", send=True)
            else:
                command("macros.step.encode", index=index, contract="Bank", function="withdraw", arguments=json.dumps([str(value)]), send=True)
        command("macros.bundle", enabled=True)
        check("bundle mode is on", query("macros.state")["macros"]["bundle"])
        command("macros.validate")
        state = query("macros.state")["macros"]
        check("validated", state["validationCurrent"] and not state["validationFailed"], state["validation"] or state["error"])

    def run():
        before = len(query("macros.state")["macros"]["results"])
        command("macros.run", dry_run=False)
        for _ in range(90):
            state = query("macros.state")["macros"]
            if not state["runBusy"] and len(state["results"]) > before:
                break
            time.sleep(1)
        command("macros.result.select", index=0)
        return query("macros.state")["macros"]

    head = int(rpc("eth_blockNumber"), 16)
    build([("deposit", 10**15), ("deposit", 2 * 10**15)])
    state = run()
    simple = state["resultSimple"]
    print("  | " + " || ".join(simple.splitlines())[:300])
    check("both deposits succeeded", [step["status"] for step in state["steps"]][:2] == ["Succeeded"] * 2, str([step["status"] for step in state["steps"]]))
    check("the result names one block", "mined together in one block" in simple, simple[:300])
    after = int(rpc("eth_blockNumber"), 16)
    block = rpc("eth_getBlockByNumber", [hex(after), False])
    check("exactly one new block holds both transactions", after == head + 1 and len(block["transactions"]) == 2, f"{head} -> {after}, {len(block['transactions'])} txs")
    check("automine is back on", rpc("anvil_getAutomine") is True)

    # A deposit then an over-withdrawal: the withdrawal reverts, so the deposit is undone too.
    balance = lambda: int(rpc("eth_getBalance", [bank_address, "latest"]), 16)
    before_balance, head = balance(), int(rpc("eth_blockNumber"), 16)
    build([("deposit", 10**15), ("withdraw", 10**24)])
    state = run()
    simple = state["resultSimple"]
    print("  | " + " || ".join(simple.splitlines())[:300])
    check("the failing step is reported", "Failed" in [step["status"] for step in state["steps"]], str([step["status"] for step in state["steps"]]))
    check("the result says it rolled back", "rolled back" in simple and "Nothing from this run stays applied" in simple, simple[:300])
    check("the chain is back where it started", int(rpc("eth_blockNumber"), 16) == head and balance() == before_balance, f"{head} vs {rpc('eth_blockNumber')}, {before_balance} vs {balance()}")
    check("automine is back on after a rollback", rpc("anvil_getAutomine") is True)
    command("ui.navigate", activity="explorer")


def fork():
    """Fork mainnet, check a real contract, debug a real transaction, back to local.

    Needs network access to the public RPC; skipped when CHASM_SKIP_FORK=1.
    """
    if os.environ.get("CHASM_SKIP_FORK") == "1":
        print("  SKIP fork (CHASM_SKIP_FORK=1)")
        return
    url = "https://ethereum-rpc.publicnode.com"
    usdc = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
    check("USDC absent locally", rpc("eth_getCode", [usdc, "latest"]) == "0x")

    def reopened(host):
        time.sleep(3)
        for _ in range(240):
            time.sleep(1)
            try:
                state = query("toolchain.state")
                if state["network"]["forkHost"] == host and not state["compile"].startswith("Compiling"):
                    return state
            except Exception:
                pass
        raise SystemExit("project did not reopen")

    # The first transaction of a block: replaying one call on the parent state
    # is exact only when nothing else in the block ran before it. The replay
    # node keeps mainnet's chain id, so EIP-712 and chain-id checks pass.
    head = int(rpc("eth_blockNumber", url=url), 16)
    tx = None
    for number in range(head - 3, head - 120, -1):
        first = rpc("eth_getBlockByNumber", [hex(number), True], url=url)["transactions"]
        # A user transaction (paid priority fee); MEV bots at index 0 often
        # depend on the exact block and cannot be replayed on their own.
        if first and first[0].get("to") and len(first[0]["input"]) > 10 and int(first[0]["gas"], 16) < 2_000_000 and int(first[0].get("maxPriorityFeePerGas", "0x0"), 16) > 0:
            tx = first[0]
            break
    check("found a contract call to replay", tx is not None)
    receipt = rpc("eth_getTransactionReceipt", [tx["hash"]], url=url)
    command("toolchain.action", action="network", fork_url=url)
    reopened("ethereum-rpc.publicnode.com")
    check("chain id stays 31337", rpc("eth_chainId") == "0x7a69")
    network = query("toolchain.state")["network"]
    check("fork names the real chain", network["forkChain"] == "Ethereum (1)", str(network))
    check("source and execution chain ids are both stated", network.get("sourceChainId") == 1 and network.get("executionChainId") == 31337, str(network))
    check("USDC code readable on the fork", len(rpc("eth_getCode", [usdc, "latest"])) > 1000)
    head_before = rpc("eth_blockNumber")
    command("debugger.trace", hash=tx["hash"])
    for _ in range(400):
        t = trace()
        if not t["busy"] and (t["loaded"] or t["status"]) and t["status"] != "Recording the execution…":
            break
        time.sleep(1)
    check("replayed without reopening the project", t["loaded"] and rpc("eth_blockNumber") == head_before, t["status"])
    check("replay outcome matches mainnet", ("REVERTED" in t["summary"]) == (receipt["status"] == "0x0"), t["summary"])
    check("replay gas matches the receipt", f"gas used {int(receipt['gasUsed'], 16)}" in t["summary"], f"{t['summary']} vs {int(receipt['gasUsed'], 16)}")
    verified = rpc("eth_getCode", [tx["to"], "latest"])
    status = t["status"]
    print(f"  note: {status[:160]}")
    check("Sourcify was asked about the contracts", "Sourcify" in status or len(verified) <= 2, status)
    if "Source from Sourcify for 0" not in status and "Sourcify" in status:
        # A verified frame is named by its contract; unverified ones by a short address.
        command("debugger.trace.view", tab=3)
        names = [row["text"] for row in trace()["calls"]]
        command("debugger.trace.view", tab=0)
        check("verified source is shown", t["file"] != "" or any(not name.startswith("0x") for name in names), f"{t['file']} {names[:4]}")
    # Open USDC by address: it is a proxy, so the ABI is its implementation's.
    command("contracts.import", address=usdc)
    for _ in range(120):
        view = query("contracts.view")
        if view.get("abiOrigin"):
            break
        time.sleep(0.5)
    check("USDC imported as a bound tab", view.get("address", "").lower() == usdc.lower() and "Sourcify" in view.get("abiOrigin", ""), str(view.get("abiOrigin")) + " " + query("app.state")["notice"])
    check("the proxy and its implementation are named", "proxy" in view.get("proxy", "") and "runs the code of 0x" in view.get("proxy", ""), view.get("proxy"))
    names = [f["name"] for f in view["functions"]]
    check("the implementation's ABI is used", "transferWithAuthorization" in names and "name" in names, str(names[:12]))
    index = names.index("name")
    command("contracts.function.select", id=view["functions"][index]["id"])
    out = command("contracts.simulate")
    for _ in range(60):
        job = next((j for j in query("jobs.list")["jobs"] if j["id"] == out["jobId"]), None)
        if job and job["status"] != "running":
            break
        time.sleep(0.5)
    check("the proxy read job finished successfully", job and job["status"] == "ok", str(job and (job["status"], job.get("summary", "")[:200])))
    result = query("contracts.view")
    check("a read through the proxy decodes", result.get("documentId") == out["documentId"] and "USD Coin" in result["simple"], result["simple"][-200:])
    command("toolchain.action", action="network", fork_url="")
    reopened(None)
    check("back to a local chain", rpc("eth_getCode", [usdc, "latest"]) == "0x")


def inline_deploy():
    """Deploy from the contract tab, call and debug in five clicks."""
    clicks = 0
    artifact = next(a for a in query("workspace.artifacts")["artifacts"] if a["name"] == "Bank")
    command("contracts.select", id=artifact["id"]); clicks += 1
    state = query("app.state")
    active = next(d for d in state["documents"] if d["active"])
    check("an unbound Bank tab is open", active["kind"] == 0 and query("contracts.view")["address"] == "", str(active))
    form = query("contracts.deploy_form")
    check("constructor form is in the tab", form["open"] and len(form["fields"]) == 2, str(form)[:300])
    command("contracts.constructor.edit", index=0, value=OWNER)
    command("contracts.constructor.edit", index=1, value="[1]")
    check("form ready", query("contracts.deploy_form")["ready"], query("contracts.deploy_form")["blockReason"])
    before = len(state["deployments"])
    command("contracts.deploy"); clicks += 1
    settle()
    state = query("app.state")
    check("deployed from the tab", len(state["deployments"]) == before + 1, state["notice"] or state["status"])
    check("no separate deploy tab", all(d["kind"] != 1 for d in state["documents"]), str(state["documents"]))
    view = query("contracts.view")
    check("tab is bound to the new address", view["address"] == state["deployments"][-1]["address"], view["address"])
    command("contracts.function.select", id=function_id("deposit")); clicks += 1
    command("contracts.request.edit", value=str(10**18))
    command("contracts.send"); clicks += 1
    settle()
    check("deposit sent", "Transaction status: Success" in query("contracts.view")["simple"])
    command("debugger.trace", hash=None); clicks += 1
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    check("deposit traced", t["loaded"] and t["total"] > 0, t["status"])
    check(f"five clicks or fewer ({clicks})", clicks <= 5)



def call_jobs():
    """Send on an unbound tab is refused with its reason; a real Send returns a job to poll."""
    artifact = next(a for a in query("workspace.artifacts")["artifacts"] if a["name"] == "Bank")
    command("contracts.select", id=artifact["id"])
    command("contracts.function.select", id=function_id("deposit"))
    view = query("contracts.view")
    check("an unbound tab cannot send, and says why", view["address"] == "" and not view["canSend"] and "no deployment" in view["sendBlockReason"], f"{view['canSend']} {view['sendBlockReason']}")
    jobs_before = len(query("jobs.list")["jobs"])
    out = command_raw("contracts.send")
    check("Send on it is refused, not reported completed", out.get("status") == "failed" and out["error"]["code"] == "not_ready" and "no deployment" in out["error"]["message"], str(out)[:300])
    check("no job was started", len(query("jobs.list")["jobs"]) == jobs_before)

    bound = query("app.state")["deployments"][-1]
    command("deployments.select", id=bound["id"])
    command("contracts.function.select", id=function_id("deposit"))
    command("contracts.request.edit", value=str(10**15))
    view = query("contracts.view")
    check("the bound tab can send", view["canSend"] and view["sendBlockReason"] == "" and view["address"] == bound["address"], view["sendBlockReason"])
    out = command("contracts.send")
    check("Send returns its job and document", out.get("effect") == "job_started" and out.get("jobId") and out.get("documentId") == view["documentId"], str(out)[:300])
    # Move to another tab while it runs: the job still reports to its own document.
    command("contracts.select", id=artifact["id"])
    for _ in range(120):
        job = next((j for j in query("jobs.list")["jobs"] if j["id"] == out["jobId"]), None)
        if job and job["status"] != "running":
            break
        time.sleep(0.5)
    check("the job finishes with a summary and hash", job and job["status"] == "ok" and job.get("transactionHash", "").startswith("0x") and job.get("documentId") == out["documentId"], str(job))
    command("ui.document.select", id=out["documentId"])
    check("the result landed in the sending document", "Transaction status: Success" in query("contracts.view")["simple"])



def impersonate():
    """Send as an address without its key: refused without gas, then a deposit as it lands."""
    actor = "0x000000000000000000000000000000000000bEEF"
    bank = next(d for d in query("app.state")["documents"] if d["title"] == "Bank")
    command("ui.document.select", id=bank["id"])
    command("wallet.impersonate", address=actor)
    wallet = query("app.state")["wallet"]
    check("the wallet acts as the address", wallet["address"] == actor.lower() and wallet["impersonated"], str(wallet))

    def deposit():
        command("contracts.function.select", id=function_id("deposit"))
        command("contracts.request.edit", value=str(10**17))
        out = command("contracts.send")
        for _ in range(120):
            job = next((j for j in query("jobs.list")["jobs"] if j["id"] == out["jobId"]), None)
            if job and job["status"] != "running":
                return job
            time.sleep(0.5)
        return job

    job = deposit()
    check("without ETH the node refuses and says why", job["status"] == "failed" and "cannot pay the value plus gas" in job.get("summary", "") and not job.get("transactionHash"), str(job))
    command("chain.cheat", action="deal", address=actor, value="1")
    settle()
    job = deposit()
    check("after a deal the deposit is sent as the address", job["status"] == "ok" and job.get("transactionHash", "").startswith("0x"), str(job))
    tx = rpc("eth_getTransactionByHash", [job["transactionHash"]])
    check("the transaction is from the impersonated address", tx["from"].lower() == actor.lower(), tx["from"])
    command("contracts.function.select", id=function_id("balanceOf"))
    command("contracts.request.edit", arguments=json.dumps([actor]))
    out = command("contracts.simulate")
    for _ in range(60):
        if not query("contracts.view")["busy"]:
            break
        time.sleep(0.5)
    check("its balance in the bank", query("contracts.view")["simple"].splitlines()[-1].endswith(str(10**17)), query("contracts.view")["simple"][-120:])
    entry = query("history.list")["entries"][0]
    check("history records who sent it", entry["from"].lower() == actor.lower(), str(entry)[:200])
    # A contract address works too (the Bank itself acts as a depositor).
    bank_address = query("contracts.view")["address"]
    command("wallet.impersonate", address=bank_address)
    command("chain.cheat", action="deal", address=bank_address, value="1")
    settle()
    job = deposit()
    check("a contract address can act too", job["status"] == "ok", str(job))
    command("wallet.dev_account", index=0)
    check("back to a dev key", not query("app.state")["wallet"]["impersonated"])


def template():
    """New ERC20 from a template; install OpenZeppelin when missing; deploy it."""
    command("contracts.template", kind="erc20", name="LiveToken")
    time.sleep(1)
    compiled()
    if "missing libraries" in query("app.state")["notice"].lower():
        command("toolchain.action", action="install", library="openzeppelin-contracts")
        for _ in range(300):
            status = query("toolchain.state")["status"]
            if status.startswith(("Installed", "Could not")):
                break
            time.sleep(1)
        check("openzeppelin installed", status.startswith("Installed"), status)
        command("toolchain.action", action="use", library="openzeppelin-contracts")
    label = compiled()
    artifacts = query("workspace.artifacts")["artifacts"]
    token = next((a for a in artifacts if a["name"] == "LiveToken"), None)
    check("template compiles", token is not None, label)
    command("contracts.select", id=token["id"])
    command("contracts.constructor.edit", index=0, value=OWNER)
    command("contracts.deploy")
    settle()
    view = query("contracts.view")
    check("template deployed in place", view["address"] != "", query("app.state")["notice"])
    command("contracts.function.select", id=function_id("totalSupply"))
    command("contracts.simulate")
    time.sleep(1)
    check("initial supply minted", query("contracts.view")["simple"].strip().endswith("1000000000000000000000000"), query("contracts.view")["simple"][-200:])
    duplicate = command_raw("contracts.template", kind="erc20", name="LiveToken")
    time.sleep(0.5)
    check("an existing file is not overwritten", "already exists" in query("app.state")["notice"], str(duplicate)[:200])



def panes():
    """Trace panes float, dock in a zone and come back with the sevm preset."""
    def placement(key):
        for pane in query("app.state")["layout"]["panes"]:
            if pane["pane"] == key:
                return pane["placement"]
        return None
    command("debugger.trace", hash=None)
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    check("trace loaded in the integrated view", t["loaded"] and placement("trace_ops") is None, str(placement("trace_ops")))
    command("ui.panel", pane="trace_console", action="dock", zone=3)
    time.sleep(0.5)
    check("console docks at the bottom", placement("trace_console") == {"mode": "docked", "zone": "bottom"}, str(placement("trace_console")))
    command("ui.panel", pane="trace_state", action="float")
    time.sleep(1)
    check("state floats in its own window", (placement("trace_state") or {}).get("mode") == "floating", str(placement("trace_state")))
    command("debugger.trace.step", action="op")
    check("stepping still works with panes moved", trace()["step"] >= 1)
    command("ui.panel", pane="trace_state", action="dock", zone=2)
    time.sleep(0.5)
    check("state docks on the right", placement("trace_state") == {"mode": "docked", "zone": "right"}, str(placement("trace_state")))
    command("ui.panel", pane="trace_state", action="hide")
    command("ui.panel", pane="trace_console", action="hide")
    time.sleep(0.5)
    check("hidden panes return to the integrated view", (placement("trace_state") or {}).get("mode") == "hidden", str(placement("trace_state")))

def gas():
    """Gas profile of the last trace: per call frame and per source line."""
    command("debugger.trace", hash=None)
    for _ in range(60):
        if trace()["loaded"] and not trace()["busy"]:
            break
        time.sleep(0.5)
    command("debugger.trace.view", tab=5)
    profile = trace()["gas"]
    check("gas per call frame, named from the ABI", profile["calls"] and "." in profile["calls"][0]["name"] and "0x" not in profile["calls"][0]["name"], str(profile["calls"])[:300])
    check("gas per source line", any(".sol:" in row["name"] for row in profile["lines"]), str(profile["lines"])[:300])
    command("debugger.trace.console", line="gas lines")
    check("console gas table", "total execution gas" in trace()["console"], trace()["console"][-300:])
    command("debugger.trace.view", tab=0)


def locals():
    """Solidity locals of the function at the cursor, from the AST and the stack."""
    import shutil
    here = os.path.dirname(os.path.abspath(__file__))
    shutil.copy(os.path.join(here, "fixtures", "Stress.sol"), os.path.join(WORKSPACE, "Stress.sol"))
    command("workspace.compile")
    compiled()
    artifact = next(a for a in query("workspace.artifacts")["artifacts"] if a["name"] == "Stress")
    command("contracts.select", id=artifact["id"])
    command("contracts.deploy")
    settle()
    command("contracts.function.select", id=function_id("sum"))
    command("contracts.request.edit", arguments=json.dumps([[5, 6, 7]]))
    command("contracts.send")
    settle()
    command("debugger.trace", hash=None)
    for _ in range(60):
        if trace()["loaded"] and not trace()["busy"]:
            break
        time.sleep(0.5)
    # The SSTORE of `total = result` runs after the loop: result is 18.
    command("debugger.trace.console", line="goto 1")
    command("debugger.trace.console", line="find SSTORE")
    out = command_raw("debugger.trace.console", line="locals")
    console = trace()["console"]
    for line in console.splitlines()[-4:]:
        print("  | " + line[:160])
    check("locals name the function's variables", "result" in console and "values" in console, console[-400:])
    check("result holds the sum at the store", "result (uint256) = 18" in console, console[-400:])
    check("calldata array decoded", "values (uint256[] calldata) = [5, 6, 7]" in console, console[-400:])
    # Inside the loop body `i` is in scope.
    line = next(n for n, text in enumerate(open(os.path.join(WORKSPACE, "Stress.sol"), encoding="utf-8"), 1) if "result += values[i]" in text)
    command("debugger.trace.console", line="goto 1")
    command("debugger.trace.console", line=f"b {line}")
    for _ in range(3):
        command("debugger.trace.console", line="c")
    command("debugger.trace.console", line=f"d {line}")
    command("debugger.trace.console", line="locals")
    console = trace()["console"]
    for line in console.splitlines()[-4:]:
        print("  | " + line[:160])
    shown = console.split("(trace) locals")[-1]
    # Three continues stop on line 24 at i = 0, 1, 2; result already holds 5 + 6.
    check("the loop counter is a local inside the loop", "i (uint256) =" in shown and "result (uint256) =" in shown, shown[-300:])
    command("debugger.trace.view", tab=4)
    names = [row["name"] for row in trace()["variables"]]
    check("Variables tab lists locals", any(name.startswith("result") for name in names), str(names)[:300])
    command("debugger.trace.view", tab=0)


def captures():
    """Offline engine: save the replay's capture in the project and reopen it."""
    def debugger():
        return query("debugger.state")["debugger"]

    def paused(seconds=180):
        for _ in range(seconds * 2):
            state = debugger()
            if state["canCommand"] or state["phase"] in ("Failed", "Finished"):
                return state
            time.sleep(0.5)
        raise SystemExit(f"debugger did not pause: {debugger()['status']}")

    command("ui.navigate", activity="debugger")
    # The built-in example is a synthetic capture: an offline replay with a baseline.
    command("debugger.launch", mode=2, capture_id="synthetic")
    command("debugger.start")
    state = paused()
    check("offline replay paused", state["canCommand"], state["status"] + " " + state["notice"])
    command("debugger.capture.save")
    for _ in range(120):
        status = debugger()["captureStatus"]
        if status and not status.startswith("Exporting"):
            break
        time.sleep(0.5)
    check("capture saved in the project", status.startswith("Saved") and ".chasm" in status, status)
    saved = debugger()["captures"]
    check("capture listed", len(saved) >= 1, str(saved))
    command("debugger.cancel")
    for _ in range(60):
        if debugger()["canStart"]:
            break
        time.sleep(0.5)
    command("debugger.capture.open", name=saved[0])
    for _ in range(60):
        status = debugger()["captureStatus"]
        if status.endswith("Start offline capture.") or status.startswith("Could not"):
            break
        time.sleep(0.5)
    check("saved capture uploaded", status.endswith("Start offline capture."), status)
    command("debugger.start")
    state = paused()
    check("saved capture replays offline", state["canCommand"], state["status"] + " " + state["notice"])
    command("debugger.cancel")
    for _ in range(60):
        if debugger()["canStart"]:
            break
        time.sleep(0.5)
    command("debugger.launch", mode=0)
    command("debugger.capture.save")
    time.sleep(0.5)
    check("synthetic runs say why there is nothing to save", "no replay baseline" in debugger()["captureStatus"], debugger()["captureStatus"])



def foundry_test():
    """A Foundry test pauses at entry, stops at a launch breakpoint, and shows a compile error."""
    import shutil
    project = os.path.join(WORKSPACE, "ftest")
    os.makedirs(os.path.join(project, "test"), exist_ok=True)
    with open(os.path.join(project, "foundry.toml"), "w") as f:
        f.write('[profile.default]\nsolc_version = "0.8.28"\nevm_version = "cancun"\nlibs = []\n')
    test = os.path.join(project, "test", "Steps.t.sol")
    source = ("pragma solidity ^0.8.28;\ncontract Steps {\n uint256 public amount;\n function testOne() public {\n"
              "  amount = 1;\n  amount = 2;\n  require(amount == 2);\n }\n}\n")
    with open(test, "w") as f:
        f.write(source)

    def debugger():
        return query("debugger.state")["debugger"]

    def settled(seconds=180):
        """Paused, finished or failed: anything but preparing."""
        time.sleep(1)
        for _ in range(seconds * 2):
            state = debugger()
            if state["canCommand"] or state["phase"].lower() in ("failed", "finished"):
                return state
            time.sleep(0.5)
        return debugger()

    def stop():
        command("debugger.cancel")
        for _ in range(60):
            if debugger()["canStart"]:
                return
            time.sleep(0.5)

    try:
        command("ui.navigate", activity="debugger")
        command("debugger.launch", mode=1, foundry_target="ftest/test/Steps.t.sol", foundry_match="testOne", trusted=True, breakpoints="", run_to_breakpoint=False)
        command("debugger.start")
        state = settled()
        check("the test pauses at its first line", state["canCommand"] and "Steps.t.sol" in state["location"] and ":5" in state["location"], f"{state['location']} {state['status']} {state['notice']}")
        stop()

        command("debugger.launch", mode=1, breakpoints="Steps.t.sol:6, Missing.sol:1", run_to_breakpoint=True)
        check("the launch summary lists the breakpoints", "breakpoints Steps.t.sol:6" in debugger()["launchSummary"], debugger()["launchSummary"])
        command("debugger.start")
        state = settled()
        check("a launch breakpoint stops the run", state["canCommand"] and ":6" in state["location"], f"{state['location']} {state['status']}")
        check("the output pane names each breakpoint", "Steps.t.sol:6" in state["output"] and "'Missing.sol:1' not set" in state["output"], state["output"])
        stop()

        with open(test, "w") as f:
            f.write(source.replace("amount = 2;", "amount = missing;"))
        command("debugger.start")
        state = settled()
        check("a compile error is shown", "compile failed" in state["output"] and "Undeclared identifier" in state["output"], state["output"][:400] + " / " + state["notice"])
        stop()
    finally:
        command("debugger.launch", mode=0, breakpoints="", run_to_breakpoint=False)
        shutil.rmtree(project, ignore_errors=True)

def rpc_call_data(signature, args):
    """ABI-encode a call with cast; no keys involved."""
    import subprocess
    return subprocess.run(["cast", "calldata", signature, *map(str, args)], capture_output=True, text=True, check=True).stdout.strip()


def revert_explained():
    """A revert two frames down names its frame and custom error; oversized code names the limit."""
    import shutil
    here = os.path.dirname(os.path.abspath(__file__))
    shutil.copy(os.path.join(here, "fixtures", "Keeper.sol"), os.path.join(WORKSPACE, "Keeper.sol"))
    command("workspace.compile")
    compiled()
    artifacts = {a["name"]: a for a in query("workspace.artifacts")["artifacts"]}
    check("keeper fixture compiled", {"MiniToken", "Keeper", "Oversized"} <= set(artifacts), str(sorted(artifacts)))

    command("contracts.select", id=artifacts["MiniToken"]["id"])
    command("contracts.deploy")
    settle()
    token = query("contracts.view")["address"]
    check("token deployed", token != "", query("app.state")["notice"])
    command("contracts.select", id=query("workspace.artifacts")["artifacts"][[a["name"] for a in query("workspace.artifacts")["artifacts"]].index("Keeper")]["id"])
    command("contracts.constructor.edit", index=0, value=token)
    command("contracts.deploy")
    settle()
    keeper = query("contracts.view")["address"]
    check("keeper deployed", keeper != "", query("app.state")["notice"])

    # The keeper holds no tokens: payout reverts in MiniToken.transfer.
    command("contracts.function.select", id=function_id("payout"))
    command("contracts.request.edit", arguments=json.dumps([OWNER, "5"]))
    command("contracts.send")
    settle()
    view = query("contracts.view")
    check("send stopped before signing", view["callStatus"].startswith("Reverted before sending") and view["canDebugCall"], view["callStatus"])
    command("debugger.trace", hash=None)
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    status = t["status"]
    print("  | " + status[:220])
    check("trace names the token frame", "Reverted in MiniToken.transfer" in status and "call depth 2" in status, status)
    check("and decodes the custom error", "InsufficientBalance(sender: 0x" in status and "needed: 5" in status, status)
    check("cursor sits on the failing line", (t["sourceLine"] or {}).get("text", "").strip().startswith("if (balance < amount) revert"), str(t["sourceLine"]))

    # The same failure through the API, as an unsent call.
    data = rpc_call_data("payout(address,uint256)", [OWNER, 5])
    command("debugger.trace", call={"to": keeper, "data": data})
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    check("an unsent call traces through the API", "InsufficientBalance(" in t["status"], t["status"])
    bad = command_raw("debugger.trace", call={"data": "nothex"})
    check("a malformed call is refused", bad.get("error", {}).get("code") == "invalid_params", str(bad)[:200])

    # Oversized runtime: the deploy is stopped with the size rule, and can be traced.
    command("contracts.select", id=artifacts["Oversized"]["id"])
    command("contracts.deploy")
    settle()
    notice = query("app.state")["notice"]
    check("oversized deploy names EIP-170", "24576-byte limit (EIP-170)" in notice, notice)
    check("the creation is kept for Debug this call", query("contracts.view")["canDebugCall"], query("contracts.view")["callStatus"])
    command("debugger.trace", hash=None)
    for _ in range(60):
        t = trace()
        if not t["busy"] and t["status"] and t["status"] != "Recording the execution…":
            break
        time.sleep(0.5)
    print("  | " + t["status"][:200])
    check("tracing the creation names the size limit", t["loaded"] and "EIP-170" in t["status"], t["status"])


def external_build():
    """Code built outside Chasm (via-IR, optimizer, no metadata hash, an immutable) steps by source."""
    import shutil
    import subprocess
    here = os.path.dirname(os.path.abspath(__file__))
    work = os.path.join(WORKSPACE, "..", "external")
    shutil.rmtree(work, ignore_errors=True)
    shutil.copytree(os.path.join(here, "fixtures", "external"), work, ignore=shutil.ignore_patterns("out", "cache"))
    os.makedirs(os.path.join(work, "src"), exist_ok=True)
    shutil.move(os.path.join(work, "Vault.sol"), os.path.join(work, "src", "Vault.sol"))
    subprocess.run(["forge", "build", "--offline", "--root", work], capture_output=True, check=True)
    artifact = json.load(open(os.path.join(work, "out", "Vault.sol", "Vault.json"), encoding="utf-8"))
    creation = artifact["bytecode"]["object"] + "000000000000000000000000" + OWNER[2:].lower()
    # Deploy outside Chasm with the public Anvil key, derived at runtime.
    sent = subprocess.run(["cast", "send", "--rpc-url", "http://127.0.0.1:8545", "--private-key", anvil_key_0(), "--json", "--create", creation],
                          capture_output=True, text=True, check=True)
    address = json.loads(sent.stdout)["contractAddress"]
    check("vault deployed outside Chasm", bool(address), sent.stdout[:200])

    # The same source in the workspace, compiled with the same settings.
    shutil.copy(os.path.join(work, "src", "Vault.sol"), os.path.join(WORKSPACE, "Vault.sol"))
    command("toolchain.action", action="settings", optimizer=True, optimizer_runs=200, via_ir=True, bytecode_hash="none")
    label = compiled()
    summary = query("toolchain.state")["settings"]["summary"]
    check("settings saved", summary == "optimizer 200 runs, via-IR, no metadata hash", summary)
    check("workspace compiles with them", "contract" in label, label)

    data = rpc_call_data("deposit(uint256)", [7])
    command("debugger.trace", call={"to": address, "data": data})
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    check("external code maps to Vault.sol", t["file"] == "Vault.sol", f"{t['file']!r} {t['location']}")
    command("debugger.trace.console", line="args")
    check("arguments decode", "amount = 7" in trace()["console"], trace()["console"][-200:])
    # Back to defaults so later flows compile as before.
    command("toolchain.action", action="settings")
    compiled()
    os.remove(os.path.join(WORKSPACE, "Vault.sol"))
    command("workspace.compile")
    compiled()


def build_source():
    """Auto picks foundry.toml when present; a duplicate library names both copies."""
    import shutil
    here = os.path.dirname(os.path.abspath(__file__))
    fixture = os.path.join(here, "fixtures", "foundry_project")
    state = query("toolchain.state")["build"]
    check("Auto uses chasm.toml without foundry.toml", state["chosen"] == "auto" and state["active"] == "chasm", str(state))
    config_path = os.path.join(WORKSPACE, ".chasm", "chasm.toml")
    saved_config = open(config_path, encoding="utf-8").read() if os.path.exists(config_path) else None
    # Earlier flows leave contracts that import Chasm-managed libraries; park
    # them so the Foundry build sees only this project's sources.
    parked = os.path.join(os.path.dirname(WORKSPACE), "parked")
    os.makedirs(parked, exist_ok=True)
    for name in os.listdir(WORKSPACE):
        if name.endswith(".sol") and name != "Bank.sol":
            shutil.move(os.path.join(WORKSPACE, name), os.path.join(parked, name))
    # Turn the workspace into a Foundry project with its own library.
    shutil.copy(os.path.join(fixture, "foundry.toml"), os.path.join(WORKSPACE, "foundry.toml"))
    shutil.copytree(os.path.join(fixture, "lib"), os.path.join(WORKSPACE, "lib"), dirs_exist_ok=True)
    os.makedirs(os.path.join(WORKSPACE, "src"), exist_ok=True)
    shutil.copy(os.path.join(fixture, "src", "Pool.sol"), os.path.join(WORKSPACE, "src", "Pool.sol"))
    try:
        command("workspace.compile")
        label = compiled()
        state = query("toolchain.state")["build"]
        check("Auto switches to foundry.toml", state["active"] == "foundry" and state["origin"].startswith("foundry.toml"), str(state))
        check("the Foundry remapping compiles Pool", "Pool" in [a["name"] for a in query("workspace.artifacts")["artifacts"]], label + " " + query("app.state")["notice"][:300])
        check("Foundry settings apply", state["settings"].startswith("optimizer 200 runs"), state["settings"])
        # Chasm mode, with a second copy of the library under a more specific
        # prefix: like Chasm's OpenZeppelin next to OpenZeppelin Upgradeable.
        copy = os.path.join(os.path.dirname(WORKSPACE), "shared-copy")
        shutil.copytree(os.path.join(fixture, "lib", "shared"), copy, dirs_exist_ok=True)
        base = WORKSPACE.replace(os.sep, "/")
        mapping = f'remappings = ["@shared/base/={copy.replace(os.sep, "/")}/base/", "@shared/upgradeable/={base}/lib/shared/upgradeable/"]'
        open(config_path, "w", encoding="utf-8").write(mapping + "\n\n[build]\nsource = \"chasm\"\n")
        command("workspace.compile")
        compiled()
        notice = query("app.state")["notice"]
        print("  | " + " / ".join(notice.splitlines()[:3])[:300])
        check("a duplicate library names both copies", notice.startswith("Two copies of Init.sol") and "(from @shared/base/=" in notice and "(from a relative import)" in notice, notice[:400])
    finally:
        if saved_config is None:
            os.remove(config_path)
        else:
            open(config_path, "w", encoding="utf-8").write(saved_config)
        os.remove(os.path.join(WORKSPACE, "foundry.toml"))
        shutil.rmtree(os.path.join(WORKSPACE, "lib"), ignore_errors=True)
        shutil.rmtree(os.path.join(WORKSPACE, "src"), ignore_errors=True)
        for name in os.listdir(parked):
            shutil.move(os.path.join(parked, name), os.path.join(WORKSPACE, name))
        command("workspace.compile")
        compiled()


def call_tree():
    """The Calls tab lists frames; an attached ABI decodes code Chasm did not build; pc/opcode/entry breaks."""
    import shutil
    import subprocess
    here = os.path.dirname(os.path.abspath(__file__))
    if not os.path.exists(os.path.join(WORKSPACE, "Keeper.sol")):
        shutil.copy(os.path.join(here, "fixtures", "Keeper.sol"), os.path.join(WORKSPACE, "Keeper.sol"))
        command("workspace.compile")
        compiled()
    artifacts = {a["name"]: a for a in query("workspace.artifacts")["artifacts"]}
    # A MiniToken deployed outside Chasm: its code matches, but no tab is bound to it.
    token_code = subprocess.run(["forge", "inspect", "--root", WORKSPACE, "--contracts", WORKSPACE, "Keeper.sol:MiniToken", "bytecode"],
                                capture_output=True, text=True)
    command("contracts.select", id=artifacts["MiniToken"]["id"])
    command("contracts.deploy")
    settle()
    token = query("contracts.view")["address"]
    command("contracts.select", id=artifacts["Keeper"]["id"])
    command("contracts.constructor.edit", index=0, value=token)
    command("contracts.deploy")
    settle()
    keeper = query("contracts.view")["address"]
    data = rpc_call_data("payout(address,uint256)", [OWNER, 5])
    command("debugger.trace", call={"to": keeper, "data": data})
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    command("debugger.trace.view", tab=3)
    calls = trace()["calls"]
    texts = [row["text"] for row in calls]
    print("  | " + " || ".join(texts)[:240])
    check("two frames: the keeper and the token", len(calls) == 2 and texts[0].startswith("Keeper.payout") and "MiniToken.transfer" in texts[1], str(texts))
    check("each frame says how it ended", "→ reverted" in texts[0] and "→ reverted" in texts[1], str(texts))
    check("the cursor is in the token frame", calls[1]["here"], str(calls))
    command("debugger.trace.console", line="frame 0")
    command("debugger.trace.console", line="args")
    console = trace()["console"]
    check("args decode the outer frame", "payout(address,uint256)" in console and "amount = 5" in console, console[-300:])
    command("debugger.trace.console", line="frame 1")
    command("debugger.trace.console", line="args")
    console = trace()["console"]
    check("args decode the inner frame", "transfer(address,uint256)" in console.split("(trace) args")[-1], console[-300:])
    # Breakpoints that need no source.
    command("debugger.trace.console", line="goto 1")
    command("debugger.trace.console", line="b entry")
    command("debugger.trace.console", line="c")
    first_token_step = int(texts[1].split("steps ")[1].split("–")[0])
    check("b entry stops on the token frame's first step", trace()["step"] + 1 == first_token_step, f"{trace()['step'] + 1} vs {first_token_step}")
    command("debugger.trace.console", line="d entry")
    command("debugger.trace.console", line="goto 1")
    command("debugger.trace.console", line="b op SLOAD")
    command("debugger.trace.console", line="c")
    check("b op stops on an SLOAD", trace()["location"].rstrip().endswith("SLOAD"), trace()["location"])
    command("debugger.trace.console", line="d op SLOAD")
    # Code Chasm did not build: a MiniToken variant with a different body, so
    # its runtime does not match. Attaching the MiniToken ABI names its frame.
    variant = os.path.join(os.path.dirname(WORKSPACE), "variant")
    os.makedirs(os.path.join(variant, "src"), exist_ok=True)
    text = open(os.path.join(WORKSPACE, "Keeper.sol"), encoding="utf-8").read().split("contract Keeper")[0].replace("balanceOf[to] += amount;", "balanceOf[to] += amount + 0;")
    open(os.path.join(variant, "src", "MiniToken.sol"), "w", encoding="utf-8").write(text)
    subprocess.run(["forge", "build", "--offline", "--root", variant], capture_output=True, check=True)
    creation = json.load(open(os.path.join(variant, "out", "MiniToken.sol", "MiniToken.json"), encoding="utf-8"))["bytecode"]["object"]
    sent = subprocess.run(["cast", "send", "--rpc-url", "http://127.0.0.1:8545", "--private-key", anvil_key_0(), "--json", "--create", creation], capture_output=True, text=True, check=True)
    # cast --json may be preceded by a Foundry banner line; take the JSON line.
    outside = json.loads(next(line for line in reversed(sent.stdout.splitlines()) if line.strip().startswith("{")))["contractAddress"]
    call = rpc_call_data("transfer(address,uint256)", [OWNER, 1])
    command("debugger.trace", call={"to": outside, "data": call})
    for _ in range(60):
        if trace()["loaded"] and not trace()["busy"]:
            break
        time.sleep(0.5)
    command("debugger.trace.view", tab=3)
    before = trace()["calls"][0]["text"]
    # MiniToken alone declares this selector in a fresh workspace; with an
    # ERC20 template present it is ambiguous and the candidates are listed.
    check("an unmatched contract is named only when one ABI fits", before.startswith("MiniToken.transfer at ") or ("0xa9059cbb (one of " in before and "MiniToken.transfer" in before), before)
    command("contracts.select", id=artifacts["MiniToken"]["id"])
    command("contracts.attach", address=outside)
    command("debugger.trace", call={"to": outside, "data": call})
    for _ in range(60):
        if trace()["loaded"] and not trace()["busy"]:
            break
        time.sleep(0.5)
    after = trace()["calls"][0]["text"]
    print("  | " + after[:160])
    check("an attached ABI names it", after.startswith("MiniToken.transfer at "), after)
    command("debugger.trace.console", line="args")
    check("and decodes its arguments", "amount = 1" in trace()["console"].split("(trace) args")[-1], trace()["console"][-200:])
    command("debugger.trace.view", tab=0)


def storage_names():
    """State variables by name at any step, named writes and the transaction's state diff."""
    bank = next(d for d in query("app.state")["documents"] if d["title"] == "Bank")
    command("ui.document.select", id=bank["id"])
    _deposit(10**18)
    command("debugger.trace", hash=None)
    for _ in range(60):
        t = trace()
        if t["loaded"] and not t["busy"]:
            break
        time.sleep(0.5)
    owner_key = OWNER.lower()

    def console(line):
        command("debugger.trace.console", line=line)
        return trace()["console"].split(f"(trace) {line}")[-1]

    console("goto 1")
    before = console(f"p balanceOf[{OWNER}]")
    check("p mapping[key] at the first step reads the value before", "balanceOf[" in before and "(uint256) =" in before, before)
    check("p owner reads a variable the call never touched", OWNER[2:10].lower() in console("p owner").lower(), console("p owner"))
    lucky = console("p luckyNumbers[1]")
    check("p array[i] reads an element", "= 13" in lucky, lucky)
    check("p on a struct-less path names the problem", "No state variable" in console("p nothing"))
    console("goto 999999")
    after = console(f"p balanceOf[{OWNER}]")
    value = lambda text: int(text.split("=")[1].split()[0])
    check("the deposit shows at the last step", value(after) == value(before) + 10**18, f"{before} / {after}")
    writes = console("writes")
    check("writes are named", f"balanceOf[{OWNER}]".lower() in writes.lower(), writes)
    diff = console("diff")
    check("diff names the slot with before and after", f"balanceOf[{OWNER}]".lower() in diff.lower() and str(value(before)) in diff and str(value(after)) in diff, diff)
    check("diff shows the bank's balance change", "balance 0x" in diff and "wei" in diff, diff)
    print("  | " + " || ".join(diff.strip().splitlines())[:300])


def foundry_import():
    """A Foundry build in another folder maps in the debugger without recompiling or matching settings."""
    import shutil
    import subprocess
    here = os.path.dirname(os.path.abspath(__file__))
    work = os.path.join(WORKSPACE, "..", "imported")
    shutil.rmtree(work, ignore_errors=True)
    shutil.copytree(os.path.join(here, "fixtures", "external"), work, ignore=shutil.ignore_patterns("out", "cache"))
    os.makedirs(os.path.join(work, "src"), exist_ok=True)
    shutil.move(os.path.join(work, "Vault.sol"), os.path.join(work, "src", "Vault.sol"))
    subprocess.run(["forge", "build", "--offline", "--root", work], capture_output=True, check=True)
    artifact = json.load(open(os.path.join(work, "out", "Vault.sol", "Vault.json"), encoding="utf-8"))
    creation = artifact["bytecode"]["object"] + "000000000000000000000000" + OWNER[2:].lower()
    sent = subprocess.run(["cast", "send", "--rpc-url", "http://127.0.0.1:8545", "--private-key", anvil_key_0(), "--json", "--create", creation],
                          capture_output=True, text=True, check=True)
    address = json.loads(sent.stdout)["contractAddress"]
    data = rpc_call_data("deposit(uint256)", [9])

    def traced():
        command("debugger.trace", call={"to": address, "data": data})
        for _ in range(60):
            t = trace()
            if t["loaded"] and not t["busy"]:
                return t
            time.sleep(0.5)
        return trace()

    check("without the import the code has no source", traced()["file"] != "Vault.sol", trace()["file"])
    command("toolchain.action", action="add_import", folder="/nonexistent/project")
    check("a missing folder is refused", "does not exist" in query("toolchain.state")["status"], query("toolchain.state")["status"])
    command("toolchain.action", action="add_import", folder=work)
    state = query("toolchain.state")
    check("the build is added", "1 contracts" in state["status"] and len(state["imports"]) == 1, f"{state['status']} {state['imports']}")
    t = traced()
    check("imported via-IR code maps to Vault.sol", t["file"] == "Vault.sol", f"{t['file']!r} {t['location']}")
    command("debugger.trace.console", line="args")
    check("arguments decode from the imported ABI", "amount = 9" in trace()["console"], trace()["console"][-200:])
    command("debugger.trace.console", line="goto 999999")
    command("debugger.trace.console", line="p total")
    console = trace()["console"].split("(trace) p total")[-1]
    check("p reads a variable with the imported storage layout", "total (uint256) = 9" in console, console)
    command("toolchain.action", action="remove_import", folder=state["imports"][0])
    check("remove drops it", query("toolchain.state")["imports"] == [], str(query("toolchain.state")["imports"]))


def scenario():
    """Several transactions in one session, a conditional breakpoint, a watch, and a trace comparison."""
    bank = next(d for d in query("app.state")["documents"] if d["title"] == "Bank")
    command("ui.document.select", id=bank["id"])
    hashes = []
    for wei in (10**18, 2 * 10**18):
        _deposit(wei)
        hashes.append(query("history.list")["entries"][0]["tx"])
    check("two deposits recorded", all(h.startswith("0x") for h in hashes), str(hashes))

    def loaded():
        for _ in range(120):
            t = trace()
            if t["loaded"] and not t["busy"]:
                return t
            time.sleep(0.5)
        return trace()

    def console(line):
        command("debugger.trace.console", line=line)
        return trace()["console"].split(f"(trace) {line}")[-1]

    command("debugger.trace", hashes=hashes)
    t = loaded()
    check("the scenario opens as one session", "2 transactions" in t["title"], f"{t['title']} {t['status']}")
    txs = console("txs")
    check("txs lists both deposits", txs.count("deposit") >= 2 or txs.count("ok") == 2, txs)
    where = console("tx 2")
    check("tx 2 jumps into the second transaction", "tx 2/2" in where, where)
    console("goto 1")
    value = lambda text: int(text.split("=")[1].split()[0])
    first = value(console(f"p balanceOf[{OWNER}]"))
    console("goto 999999")
    last = value(console(f"p balanceOf[{OWNER}]"))
    check("storage carries over: the end has both deposits", last - first == 3 * 10**18, f"{first} -> {last}")
    calls = console("calls")
    check("one TX frame per transaction", calls.count("tx 1 ") == 1 and calls.count("tx 2 ") == 1, calls)

    # Watch the owner's balance: c stops at each write, in either transaction.
    console("goto 1")
    check("watch a mapping entry", "Watching" in console(f"watch balanceOf[{OWNER}]"), trace()["console"][-200:])
    stop1 = console("c")
    stop2 = console("c")
    check("the watch stops in each transaction", "tx 1/2" in stop1 and "tx 2/2" in stop2 and "SSTORE" in stop2, f"{stop1} / {stop2}")
    console(f"unwatch balanceOf[{OWNER}]")
    console("goto 1")
    console(f"b op SSTORE if depth == 1 && $1 > {first + 2 * 10**18}")
    stop = console("c")
    check("a conditional opcode break skips the first deposit", "tx 2/2" in stop, stop)
    console("d op SSTORE")

    # Compare the second deposit with the first one's saved trace. Each
    # transaction debugged on its own is saved; scenario parts are not.
    command("debugger.trace", hash=hashes[0])
    loaded()
    command("debugger.trace", hash=hashes[1])
    for _ in range(60):
        if trace()["title"].endswith(hashes[1][-6:]) and not trace()["busy"]:
            break
        time.sleep(0.5)
    listing = console("compare")
    check("compare lists saved traces", hashes[0] in listing, listing[:300])
    compared = console(f"compare {hashes[0]}.json")
    check("compare reports the path and the effect difference", ("Same path" in compared or "First divergence" in compared) and "Storage effects differ" in compared and f"balanceOf[{OWNER}]" in compared, compared)
    print("  | " + " || ".join(compared.strip().splitlines())[:300])

from networks import networks

FLOWS = [bank, revert, steps, history, timeline, toolchain, palette, macro, bundle, inline_deploy, call_jobs, impersonate, template, panes, gas, storage_names, scenario, locals, captures, foundry_test, revert_explained, call_tree, external_build, foundry_import, build_source, fork, networks]
