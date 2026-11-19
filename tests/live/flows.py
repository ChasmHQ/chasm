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
        check("verified source is shown", t["file"] != "" or any(not f.endswith("unknown code") for f in t["frames"]), f"{t['file']} {t['frames']}")
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

FLOWS = [bank, revert, steps, history, timeline, toolchain, palette, macro, inline_deploy, template, panes, gas, locals, captures, fork]
