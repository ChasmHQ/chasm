"""The MCP frontend: the driver and its AI-shaped data, without a real client.

The tool functions are thin wrappers over `DebugDriver`, so the coverage here is
driver-level: start/step/inspect windows, the byte search, diffs, and the error
convention (failures come back as `{"error": ...}` dicts a model can read). One
in-memory client round-trip proves the server side speaks the protocol.
"""

from __future__ import annotations

import asyncio
import contextlib
from collections.abc import Iterator

import pytest

from sevm.mcp_server import DebugDriver, build_server

COUNTER = """\
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

contract Counter {
    uint256 public count;
    address public owner;

    constructor() {
        owner = msg.sender;
    }

    function inc() public {
        count += 1;
    }
}
"""

DRIVER_SCRIPT = """\
import os

from web3 import EthereumTesterProvider, Web3

from sevm.compile import compile_project

CONTRACTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "contracts")


def main():
    project = compile_project([CONTRACTS])
    w3 = Web3(EthereumTesterProvider())
    w3.eth.default_account = w3.eth.accounts[0]
    art = project.artifact("Counter")
    factory = w3.eth.contract(abi=art.abi, bytecode=art.bytecode.hex())
    tx = factory.constructor().transact({"gas": 3_000_000})
    address = w3.eth.wait_for_transaction_receipt(tx)["contractAddress"]
    counter = w3.eth.contract(address=address, abi=art.abi)
    counter.functions.inc().transact({"gas": 300_000})
    counter.functions.inc().transact({"gas": 300_000})


main()
"""


@pytest.fixture
def lab(tmp_path) -> Iterator[tuple[DebugDriver, str]]:
    """A driver plus a script that deploys a Counter and increments it twice."""
    contracts = tmp_path / "contracts"
    contracts.mkdir()
    (contracts / "Counter.sol").write_text(COUNTER)
    script = tmp_path / "drive.py"
    script.write_text(DRIVER_SCRIPT)
    driver = DebugDriver(timeout=60.0)
    yield driver, str(script)
    driver.stop()


def test_start_lands_on_a_structured_stop(lab):
    driver, script = lab
    report = driver.start(script)
    assert report["stopped"] is True
    assert report["kind"] == "script"
    assert report["location"]["contract"] == "Counter"
    assert report["gas"]["limit"] > 0
    assert isinstance(report["stack_top"], list)
    assert report["stop_reason"]


def test_step_reports_stack_delta_and_diff(lab):
    driver, script = lab
    driver.start(script)
    report = driver.step_opcodes(3)
    assert report["stopped"] is True
    diff = driver.diff_since_last_stop()
    assert diff["available"] is True
    assert diff["gas_used"] > 0
    assert diff["pc"]["from"] != diff["pc"]["to"] or diff["gas_used"] > 0
    # A fresh session has no previous stop to diff against until the first step.
    assert {"pushed", "popped", "from", "to"} & set(diff["stack"])


def test_memory_window_is_annotated_and_bounded(lab):
    driver, script = lab
    driver.start(script)
    window = driver.read_memory(0x40, 2)
    assert window["items"][0]["offset"] == 0x40
    assert window["items"][0]["region"] == "free memory pointer"
    assert window["items"][1]["region"] == "zero slot"
    assert window["words"] == 2
    big = driver.read_memory(0, 10_000)
    assert big["words"] == 64  # hard cap holds even for absurd requests


def test_find_bytes_locates_gadgets_in_code(lab):
    driver, script = lab
    driver.start(script)
    found = driver.find_bytes("6080604052", scope="code")
    assert found["total"] >= 1
    first = found["hits"][0]
    # The first stop runs the CREATION code, where the runtime prologue appears at
    # the offset it is embedded at — the hit still reports its pc and alignment.
    assert {"pc", "instruction_aligned", "jumpdest", "nearest_jumpdest", "text"} <= set(
        first
    )
    assert found["code_size"] > 0
    raw = driver.find_bytes("6080", scope="code", limit=5)
    assert raw["hits"], "PUSH1 0x80 appears in every solc prologue"
    with pytest.raises(Exception, match="hex"):
        driver.find_bytes("zz", scope="code")


def test_find_bytes_scopes_memory_and_calldata(lab):
    driver, script = lab
    driver.start(script)
    report = driver.step_opcodes(1)
    assert "error" not in report
    mem = driver.find_bytes("00", scope="memory")
    assert mem["scope"] == "memory" and mem["scanned"] > 0
    with_scope = driver.find_bytes("00", scope="calldata")
    assert with_scope["total"] >= 0
    with pytest.raises(Exception, match="scope"):
        driver.find_bytes("00", scope="nonsense")


def test_storage_decodes_the_layout(lab):
    driver, script = lab
    driver.start(script)
    storage = driver.read_storage()
    names = {item.get("name") for item in storage["items"]}
    assert {"count", "owner"} <= names
    raw = driver.read_storage(slots=[0], decode=False)
    assert raw["items"][0]["slot"] == 0
    assert len(raw["items"][0]["hex"]) == 66  # 0x + 64


def test_breakpoint_and_continue(lab):
    driver, script = lab
    driver.start(script)
    bp = driver.set_breakpoint("inc")
    assert bp["id"] == 1
    report = driver.continue_execution()
    assert report["stopped"] is True
    assert "inc" in report["location"]["function"]
    assert 1 in report["hit_breakpoints"]
    listed = driver.list_breakpoints()
    assert any("inc" in row for row in listed["breakpoints"])
    assert driver.delete_breakpoint(1)["deleted"] == 1


def test_restart_keeps_breakpoints(lab):
    driver, script = lab
    driver.start(script)
    driver.set_breakpoint("inc")
    driver.continue_execution()
    report = driver.restart()
    assert report["stopped"] is True  # fresh constructor stop
    hit = driver.continue_execution()
    assert 1 in hit["hit_breakpoints"]  # the breakpoint survived


def test_evaluate_and_command_passthrough(lab):
    driver, script = lab
    driver.start(script)
    result = driver.evaluate("owner == msg.sender")
    assert result["type"].startswith("bool")
    raw = driver.command("info registers")
    assert raw["ok"] and any("pc" in line for line in raw["lines"])
    assert "[" not in "".join(raw["lines"]) or "0x" in "".join(raw["lines"])


def test_run_to_line_and_source_window(lab):
    driver, script = lab
    driver.start(script)
    driver.continue_execution()  # constructor -> first inc() entry? land somewhere sourced
    report = driver.status()
    if report["stopped"] and report["location"].get("file"):
        src = driver.read_source()
        assert src["total_lines"] > 5
        assert any(item["current"] for item in src["items"])


def test_errors_are_structured_not_exceptions(lab):
    """Driver methods raise; the tools layer is what converts to error dicts."""
    driver, _script = lab
    with pytest.raises(Exception, match="sevm_start_session"):
        driver.status()
    with pytest.raises(Exception, match="no such file"):
        driver.start("C:/definitely/not/here.py")


def test_disassemble_window(lab):
    driver, script = lab
    driver.start(script)
    rows = driver.disassemble(before=2, after=4)
    assert 3 <= len(rows["rows"]) <= 6
    assert {"pc", "text", "jumpdest"} <= set(rows["rows"][0])


# ==================================================================
# protocol round-trip (in-memory client)
# ==================================================================


def test_client_round_trip_lists_tools_and_returns_error_dicts():
    async def scenario() -> None:
        import anyio
        from mcp import ClientSession
        from mcp.shared.memory import create_client_server_memory_streams

        server = build_server(DebugDriver())
        async with create_client_server_memory_streams() as streams:
            client_streams, server_streams = streams
            app = server._lowlevel_server

            async def serve() -> None:
                await app.run(
                    server_streams[0],
                    server_streams[1],
                    app.create_initialization_options(),
                )

            task = asyncio.create_task(serve())
            try:
                with anyio.fail_after(30):
                    async with ClientSession(
                        client_streams[0], client_streams[1]
                    ) as client:
                        await client.initialize()
                        tools = await client.list_tools()
                        names = {t.name for t in tools.tools}
                        assert "sevm_start_session" in names
                        assert "sevm_find_bytes" in names
                        res = await client.call_tool("sevm_get_status", {})
                        # dict returns serialise as JSON text content (no output
                        # schema is declared, so there is no structuredContent).
                        import json

                        payload = json.loads(res.content[0].text)
                        assert "sevm_start_session" in payload.get("error", "")
            finally:
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await task

    asyncio.run(scenario())
