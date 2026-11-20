from __future__ import annotations

import json
from types import SimpleNamespace

import pytest

from chasm_debugger import cli
from chasm_debugger.commands.processor import CommandProcessor
from chasm_debugger.commands.result import CommandResult
from chasm_debugger.compile import Project
from chasm_debugger.mcp_server import DebugDriver, build_server
from chasm_debugger.session import DebugSession, Finished, Paused, SessionError, StepMode

CALLER = "0x" + "11" * 20
TARGET = "0x" + "22" * 20
HASH = "0x" + "33" * 32


def empty_project():
    return Project({}, {}, {}, "0.8.28")


class FakeChain:
    def __init__(self):
        self.points = []
        self.requests = []

    def set_breakpoints(self, points):
        self.points = points

    def transact_json(self, request):
        self.requests.append(json.loads(request))

    def wait(self, timeout):
        return {"type": "finished", "success": True, "gas_used": 21000}


class FakePlan:
    def __init__(self):
        self.target = {"caller": CALLER, "to": TARGET, "data": "0x", "gas_limit": 100000}
        self.identity = {
            "chain_id": 1,
            "block_number": 17000000,
            "transaction_hash": HASH,
        }
        self.report = {"mode": "replay", "strict": True}
        self.baseline_report = {"validated": True}
        self.chains = []
        self.baselines = 0

    def new_chain(self):
        chain = FakeChain()
        self.chains.append(chain)
        return chain

    def run_baseline(self):
        self.baselines += 1
        return SimpleNamespace(report=self.baseline_report, witness={"version": 1})


def test_mainnet_parser_keeps_options_after_transaction_hash():
    args = cli.build_parser().parse_args(
        [
            "replay",
            HASH,
            "--rpc-env",
            "ETH_RPC_URL",
            "--contracts",
            "build-info",
            "--console",
            "-x",
            "si",
            "-x",
            "b *0x10",
            "--capture",
            "capture.json",
        ]
    )
    assert args.tx_hash == HASH
    assert args.rpc_env == "ETH_RPC_URL"
    assert args.exec == ["si", "b *0x10"]
    assert args.capture == "capture.json"
    offline = cli.build_parser().parse_args(["replay", "--offline", "capture.json"])
    assert offline.tx_hash is None
    fork = cli.build_parser().parse_args(
        [
            "fork",
            "--block",
            "0x100",
            "--rpc-env",
            "RPC",
            "--to",
            TARGET,
            "--from",
            CALLER,
            "--nonce",
            "1",
            "--gas-price",
            "2",
            "--console",
        ]
    )
    assert fork.block == 256
    assert fork.caller == CALLER


def test_fork_missing_driver_fails_before_rpc(monkeypatch, capsys):
    monkeypatch.setattr(
        cli, "_prepare_mainnet", lambda *a, **k: pytest.fail("RPC attempted")
    )
    assert (
        cli.main(["fork", "--block", "17000000", "--rpc-env", "ETH_RPC_URL", "--console"])
        == 1
    )
    assert "--to and --from" in capsys.readouterr().out


def test_fork_driver_requires_explicit_nonce_and_fee():
    with pytest.raises(ValueError, match="--nonce and --gas-price"):
        cli._fork_transaction(to=TARGET, caller=CALLER)
    request = cli._fork_transaction(to=TARGET, caller=CALLER, nonce=0, gas_price=10)
    assert request["nonce"] == 0
    assert request["gas_price"] == "0xa"
    from chasm_debugger.fork.validation import native_request

    assert native_request(request) == request
    assert request["chain_id"] == 1
    assert request["tx_type"] == 0
    assert request["commit"] is True


def test_empty_project_does_not_compile(monkeypatch):
    monkeypatch.setattr(
        cli, "compile_foundry_project", lambda *a, **k: pytest.fail("compile attempted")
    )
    project = cli._mainnet_project(None)
    assert not project.sources and not project.artifacts


def test_replay_prepares_baseline_and_capture(monkeypatch):
    import chasm_debugger.fork as fork

    plan = FakePlan()
    saved = []
    monkeypatch.setenv("FRONTEND_TEST_RPC", "https://secret.invalid/key")
    monkeypatch.setattr(fork, "prepare_replay", lambda rpc, tx: plan)
    monkeypatch.setattr(fork, "save_capture", lambda *args: saved.append(args))
    actual, baseline = cli._prepare_mainnet(
        "replay", rpc_env="FRONTEND_TEST_RPC", tx_hash=HASH, capture="bundle.json"
    )
    assert actual is plan
    assert baseline.report["validated"]
    assert plan.baselines == 1
    assert saved[0] == ("bundle.json", plan, baseline)


def test_offline_replay_does_not_read_environment(monkeypatch):
    import chasm_debugger.fork as fork

    plan = FakePlan()
    monkeypatch.setattr(fork, "load_capture", lambda path: plan)
    monkeypatch.setattr(
        cli, "_rpc_from_env", lambda *a: pytest.fail("RPC environment read")
    )
    assert cli._prepare_mainnet("replay", offline="bundle.json")[0] is plan
    with pytest.raises(ValueError, match="cannot be combined"):
        cli._prepare_mainnet("replay", offline="bundle.json", rpc_env="RPC")


def test_rpc_errors_do_not_print_endpoint(monkeypatch, capsys):
    import chasm_debugger.fork as fork

    secret = "https://user:password@rpc.invalid/private-token"
    monkeypatch.setenv("FRONTEND_TEST_RPC", secret)

    def fail(*args):
        raise RuntimeError(f"request failed: {secret}")

    monkeypatch.setattr(fork, "prepare_replay", fail)
    assert cli.main(["replay", HASH, "--rpc-env", "FRONTEND_TEST_RPC", "--console"]) == 1
    output = capsys.readouterr().out
    assert secret not in output
    assert "password" not in output
    assert "redacted" in output


def test_session_restart_rebuilds_anchor_and_preserves_baseline():
    plan = FakePlan()
    session = DebugSession.from_fork_plan(empty_project(), plan)
    session.start()
    assert isinstance(session.wait(2), Finished)
    assert session._chain.requests == [plan.target]
    assert ("0x" + "00" * 20, 0) in session._chain.points
    session.mark_diverged()
    assert session.diverged
    report = session.baseline_report
    assert isinstance(session.restart(timeout=2), Finished)
    assert len(plan.chains) == 2
    assert not session.diverged
    assert session.baseline_report == report
    assert session.experiment_result["success"]
    with pytest.raises(SessionError, match="script arguments"):
        session.restart(argv=["changed"], timeout=2)


def test_inspection_and_discarded_evaluate_are_not_divergent(monkeypatch):
    session = DebugSession.from_fork_plan(empty_project(), FakePlan())
    monkeypatch.setattr(session, "_inspect", lambda *a, **k: 1)
    session.inspect("read_storage", 0)
    session.inspect("evaluate", "1", keep=False)
    assert not session.diverged
    session.inspect("evaluate", "1", keep=True)
    assert session.diverged
    session.diverged = False
    session.inspect("write_memory", 0, b"x")
    assert session.diverged


def test_mutated_command_marks_experiment(monkeypatch):
    session = DebugSession.from_fork_plan(empty_project(), FakePlan())
    processor = CommandProcessor(session, None)
    monkeypatch.setattr(processor, "_dispatch", lambda line: CommandResult(mutated=True))
    monkeypatch.setattr(session, "refresh_snapshot", lambda: None)
    assert processor.execute("synthetic").mutated
    assert session.diverged


def test_mcp_replay_uses_navigation_session_and_separate_baseline(monkeypatch):
    plan = FakePlan()
    monkeypatch.setattr(
        cli, "_prepare_mainnet", lambda *a, **k: (plan, plan.run_baseline())
    )
    driver = DebugDriver(timeout=2)
    report = driver.start_replay(tx_hash=HASH, rpc_env="RPC")
    assert driver._processor.session is driver._session
    assert report["kind"] == "replay"
    assert report["baseline"] == {"validated": True}
    assert not report["diverged"]
    driver._session.mark_diverged()
    assert driver.status()["diverged"]
    assert driver.fork_info()["baseline"]["validated"]
    assert "simultaneously" in driver.fork_info()["attachment"]
    assert not driver.restart()["diverged"]
    driver.stop()


def test_mcp_fork_requires_driver_before_preparation(monkeypatch):
    monkeypatch.setattr(
        cli, "_prepare_mainnet", lambda *a, **k: pytest.fail("RPC attempted")
    )
    with pytest.raises(Exception, match="--nonce and --gas-price"):
        DebugDriver().start_fork(17000000, "RPC", TARGET, CALLER)


def test_server_registers_mainnet_tools():
    import asyncio

    server = build_server()
    names = {tool.name for tool in asyncio.run(server.list_tools())}
    assert {"chasm_start_replay", "chasm_start_fork", "chasm_fork_info"} <= names


def test_raw_pc_and_opcode_breakpoints_with_empty_project(monkeypatch):
    chain = FakeChain()
    session = DebugSession(empty_project(), chain_factory=lambda: chain)
    session.break_at_pc(15)
    session._sync_breakpoints()
    assert ("0x" + "00" * 20, 15) in chain.points
    session.break_at_opcode("SSTORE")
    session._frames = [
        SimpleNamespace(
            disassembly=SimpleNamespace(
                instructions=[SimpleNamespace(pc=4, mnemonic="SSTORE")]
            )
        )
    ]
    session._sync_breakpoints()
    assert ("0x" + "00" * 20, 4) in chain.points


def test_real_raw_entry_steps_mutations_and_restart():
    from chasm_debugger._revm import RevmChain

    if not hasattr(RevmChain, "transact_json"):
        pytest.skip("native fork bridge has not been rebuilt")
    chains = []

    def new_chain():
        chain = RevmChain()
        chain.write_balance(CALLER, 10**20)
        chain.write_code_at(TARGET, bytes.fromhex("600160005560025000"))
        chains.append(chain)
        return chain

    plan = FakePlan()
    plan.new_chain = new_chain
    plan.target = cli._fork_transaction(
        to=TARGET, caller=CALLER, gas=100000, gas_price=0, nonce=0
    )
    session = DebugSession.from_fork_plan(empty_project(), plan)
    try:
        session.start()
        first = session.wait(5)
        assert isinstance(first, Paused)
        assert first.snapshot.pc == 0
        assert not first.snapshot.has_source
        assert session.resume(StepMode.STEPI, timeout=5).snapshot.pc == 2
        session.inspect("set_gas", 70000)
        assert session.diverged
        assert session.baseline_report["validated"]
        assert session.restart(timeout=5).snapshot.pc == 0
        assert not session.diverged
        assert len(chains) == 2
        session.break_at_pc(5)
        assert session.resume(StepMode.RUN, timeout=5).snapshot.pc == 5
    finally:
        session.detach()


def _synthetic_capture(tmp_path):
    from chasm_debugger._revm import RevmChain
    from test_fork_native import synthetic_reference

    from chasm_debugger.fork import save_capture

    if not hasattr(RevmChain, "transact_json"):
        pytest.skip("native fork bridge has not been rebuilt")
    plan, _ = synthetic_reference()
    plan.saved_witness["storage"].setdefault(plan.target["to"], {})["0x0"] = "0x0"
    baseline = plan.run_baseline()
    path = tmp_path / "synthetic-replay.json"
    save_capture(path, plan, baseline)
    return path


def test_real_offline_mcp_baseline_target_restart_and_mutations(tmp_path, monkeypatch):
    import requests
    from test_fork_native import BEACON, ROOT, STOP_ADDRESS

    path = _synthetic_capture(tmp_path)

    def no_request(*args, **kwargs):
        pytest.fail("offline frontend attempted HTTP")

    monkeypatch.setattr(requests.Session, "request", no_request)
    driver = DebugDriver(timeout=5)
    try:
        report = driver.start_replay(offline=str(path))
        assert report["stopped"] and report["pc"] == 0
        assert report["address"] == STOP_ADDRESS
        assert report["fork"]["anchor"]["number"] == 199
        assert report["fork"]["source"]["mode"] == "raw"
        assert report["fork"]["source"]["parity"] is None
        assert report["baseline"]["validated"]
        assert len(report["baseline"]["transactions"]) == 2
        assert not report["diverged"]
        session = driver._session
        assert int(session._chain.read_nonce(CALLER)) == 2
        assert int(session._chain.read_storage_at(BEACON, 0), 16) == int(ROOT, 16)
        assert driver.disassemble()["rows"]
        assert not driver.status()["diverged"]
        driver.write_storage(0, 7)
        assert driver.status()["diverged"]
        assert driver.status()["baseline"]["validated"]
        assert driver.restart()["pc"] == 0
        assert not driver.status()["diverged"]
        assert int(session._chain.read_nonce(CALLER)) == 2
        assert int(session._chain.read_storage_at(BEACON, 0), 16) == int(ROOT, 16)
        assert driver.continue_execution()["finished"]
        assert driver.fork_info()["experiment_result"]["success"]
        json.dumps(driver.status())
    finally:
        driver.stop()


def test_real_offline_cli_uses_standard_console_and_startup_commands(
    tmp_path, monkeypatch, capsys
):
    from rich.console import Console

    path = _synthetic_capture(tmp_path)
    monkeypatch.setattr(Console, "input", lambda *a, **k: "q")
    assert (
        cli.main(
            ["replay", "--offline", str(path), "--console", "-x", "info gas", "-x", "si"]
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "Baseline replay report" in output
    assert "Solidity/EVM debugger" in output
    assert "(no source)" in output or "pc" in output


def test_real_opcode_breakpoint_discovers_unseen_raw_callee():
    from chasm_debugger._revm import RevmChain

    if not hasattr(RevmChain, "transact_json"):
        pytest.skip("native fork bridge has not been rebuilt")
    callee = "0x" + "44" * 20
    code = bytes.fromhex("5f5f5f5f5f73" + callee[2:] + "620186a0f15000")

    def new_chain():
        chain = RevmChain()
        chain.write_balance(CALLER, 10**20)
        chain.write_code_at(TARGET, code)
        chain.write_code_at(callee, bytes.fromhex("5b600160005500"))
        return chain

    plan = FakePlan()
    plan.new_chain = new_chain
    plan.target = cli._fork_transaction(
        to=TARGET, caller=CALLER, gas=300000, gas_price=0, nonce=0
    )
    session = DebugSession.from_fork_plan(empty_project(), plan)
    driver = DebugDriver(timeout=5)
    driver._target_kind = "fork"
    try:
        driver._wire(empty_project(), None, session=session)
        driver.set_opcode_breakpoint("SSTORE")
        report = driver.continue_execution()
        assert report["stopped"]
        assert report["address"] == callee
        assert report["mnemonic"] == "SSTORE"
        assert report["pc"] == 5
        assert report["hit_breakpoints"]
        assert not report["diverged"]
        assert driver.continue_execution()["finished"]
        assert driver.restart()["pc"] == 0
        driver.command("delete")
        assert driver.continue_execution()["finished"]
        assert not driver.status()["diverged"]
    finally:
        driver.stop()
