"""Phase 7 stress flow: large forms, long traces, many tabs, deployments,
History entries and macro steps. Run with `python tests/live/run.py stress`.

Each check also records how long the action took; budgets follow NEXT_PLAN P3.
Screenshots of each size go to $CHASM_STRESS_SHOTS when it is set.
"""
import json
import os
import shutil
import time

from chasm import (OWNER, check, command, compiled, function_id, query, screenshot,
                   settle, trace)

HERE = os.path.dirname(os.path.abspath(__file__))
WORKSPACE = os.environ.get("CHASM_WORKSPACE", "")
SHOTS = os.environ.get("CHASM_STRESS_SHOTS", "")
TIMINGS = []


def timed(label, action, budget_ms=None):
    started = time.perf_counter()
    result = action()
    elapsed = (time.perf_counter() - started) * 1000
    TIMINGS.append((label, elapsed, budget_ms))
    if budget_ms is not None:
        check(f"{label} under {budget_ms} ms ({elapsed:.0f} ms)", elapsed <= budget_ms)
    return result


def wait_trace(seconds=120):
    for _ in range(seconds * 4):
        t = trace()
        if t["loaded"] and not t["busy"]:
            return t
        if t["status"] and not t["busy"] and not t["loaded"]:
            return t
        time.sleep(0.25)
    raise SystemExit("trace did not load")


def sent():
    """The last Send succeeded: the result is new and not a revert."""
    view = query("contracts.view")
    return "Transaction status: Success" in view["simple"] and not view["callStatus"].startswith("Reverted")


def stress_artifact():
    shutil.copy(os.path.join(HERE, "fixtures", "Stress.sol"), os.path.join(WORKSPACE, "Stress.sol"))
    command("workspace.compile")
    compiled()
    artifacts = query("workspace.artifacts")["artifacts"]
    return next(a for a in artifacts if a["name"] == "Stress"), next((a for a in artifacts if a["name"] == "IWide"), None)


def stress():
    """Wide forms, long traces, many tabs, deployments, History and macro steps."""
    stress, wide = stress_artifact()
    command("wallet.dev_account", index=0)
    check("IWide interface listed for the 20-field form", wide is not None)

    # 20 parameters: every field is rendered and keeps its value.
    command("contracts.select", id=wide["id"])
    command("contracts.attach", address="0x000000000000000000000000000000000000dEaD")
    command("contracts.function.select", id=function_id("wide"))
    view = query("contracts.view")
    check("20 request fields", len(view["fields"]) == 20, str(len(view["fields"])))

    # Deploy Stress in place, then 30 more deployments.
    command("contracts.select", id=stress["id"])
    timed("deploy", lambda: (command("contracts.deploy"), settle()))
    address = query("contracts.view")["address"]
    check("Stress deployed", address != "", query("app.state")["notice"])

    # A 50-item array argument.
    command("contracts.function.select", id=function_id("sum"))
    values = list(range(1, 51))
    command("contracts.request.edit", arguments=json.dumps([values]))
    command("contracts.send")
    settle()
    check("50-item array sent", sent(), query("contracts.view")["callStatus"])

    # Long traces: 4000 storage writes and 20000 loop iterations.
    for name, n, label in [("churn", 400, "400 storage writes"), ("spin", 20000, "20000 iterations")]:
        command("contracts.function.select", id=function_id(name))
        command("contracts.request.edit", arguments=json.dumps([str(n)]))
        command("contracts.send")
        settle()
        check(f"{label} sent", sent(), query("contracts.view")["callStatus"])
        command("debugger.trace", hash=None)
        started = time.perf_counter()
        t = wait_trace()
        load_ms = (time.perf_counter() - started) * 1000
        TIMINGS.append((f"trace {label} ({t['total']} steps)", load_ms, None))
        check(f"{label} traced", t["loaded"] and t["total"] > 1000, t["status"])
        if t["total"] >= 200000:
            check("a cut trace says so", "cut at the step limit" in t["summary"], t["summary"])
        timed(f"seek to the end of {label}", lambda: command("debugger.trace.seek", step=t["total"] - 1), 500)
        timed(f"step back in {label}", lambda: command("debugger.trace.step", action="op-back"), 500)
        command("debugger.trace.view", tab=4)
        timed(f"step with Variables (locals) shown in {label}", lambda: command("debugger.trace.step", action="op-back"), 1000)
        command("debugger.trace.view", tab=0)
        if name == "churn":
            command("debugger.trace.view", tab=4)
            variables = trace()["variables"]
            check("named writes listed", sum(1 for v in variables if v["name"].startswith("slots[")) >= 400, str(len(variables)))
            command("debugger.trace.view", tab=0)

    # 30 deployments.
    command("ui.navigate", activity="contracts")
    command("contracts.select", id=stress["id"])
    before = len(query("app.state")["deployments"])
    for _ in range(30):
        command("contracts.select", id=stress["id"])
        command("contracts.deploy")
        settle()
    state = query("app.state")
    check("30 more deployments", len(state["deployments"]) == before + 30, str(len(state["deployments"])))

    # 15 open tabs: one per deployment.
    for deployment in state["deployments"][-15:]:
        command("deployments.select", id=deployment["id"])
    tabs = len(query("app.state")["documents"])
    check("15 or more tabs open", tabs >= 15, str(tabs))

    # 500 History entries (the most it keeps).
    history = os.path.join(WORKSPACE, ".chasm", "history.jsonl")
    with open(history, "a", encoding="utf-8") as file:
        for index in range(520):
            file.write(json.dumps({"time": 1700000000 + index, "kind": "send" if index % 2 else "simulate", "contract": "Stress",
                                   "address": address, "function": "spin(uint256)", "arguments": f"[\"{index}\"]", "value": "0",
                                   "from": OWNER, "status": "ok" if index % 7 else "reverted", "summary": f"entry {index}"}) + "\n")
    entries = timed("history.list with 500 entries", lambda: query("history.list")["entries"], 1000)
    check("History keeps 100 per query of the newest 500", len(entries) == 100, str(len(entries)))
    timed("open History with 500 rows", lambda: command("history.action", action="show", index=0), 500)

    # Palette with 30 deployments and 500 History rows.
    timed("palette search", lambda: command("ui.palette", query="stress spin"), 500)
    command("ui.palette", query="stress spin", index=0)

    # 128-step macro (the schema limit).
    command("ui.navigate", activity="macros")
    command("macros.new")
    for index in range(128):
        command("macros.step.add", kind="snapshot")
    command("macros.validate")
    macros = query("macros.state")["macros"]
    check("128-step macro validates", macros["validationCurrent"] and not macros["validationFailed"], macros["validation"] or macros["error"])
    started = time.perf_counter()
    command("macros.run", dry_run=False)
    for _ in range(480):
        macros = query("macros.state")["macros"]
        if not macros["runBusy"] and macros["results"]:
            break
        time.sleep(0.5)
    TIMINGS.append(("run the 128-step macro", (time.perf_counter() - started) * 1000, None))
    statuses = [step["status"] for step in macros["steps"]]
    check("128-step macro ran", len(statuses) == 128 and all(status == "Succeeded" for status in statuses), str(statuses[:6]) + f" … {len(statuses)}")

    timed("open Contracts after the 128-step result", lambda: command("ui.navigate", activity="contracts"), 1000)
    # Every size and zoom renders; screenshots for review.
    for zoom in [100, 125, 150]:
        timed(f"zoom {zoom} %", lambda: command("ui.layout", zoom_percent=zoom), 1000)
        for activity in ["contracts", "explorer", "macros", "debugger"]:
            timed(f"open {activity} at {zoom} %", lambda: command("ui.navigate", activity=activity), 1000)
            time.sleep(0.3)
            if SHOTS:
                screenshot(os.path.join(SHOTS, f"{activity}-{zoom}.png"))
    command("ui.layout", zoom_percent=100)

    print("\n  timings")
    for label, elapsed, budget in TIMINGS:
        print(f"  {elapsed:8.0f} ms  {label}{'' if budget is None else f'  (budget {budget})'}")
