"""The MCP tool surface.

Every tool is a thin async wrapper that runs a driver method off the event loop
(the session blocks by design) and returns a dict: models get structured JSON,
and every windowed result says whether it was truncated so the next call can
narrow itself. Errors return as `{"error": ...}` dicts rather than protocol
errors, so the model can read and recover from them.
"""

from __future__ import annotations

from typing import Any

import anyio
from mcp.server.mcpserver import MCPServer

from .driver import DebugDriver, DriverError

INSTRUCTIONS = """sevm: a gdb-style Solidity/EVM debugger running on Py-EVM.

Workflow: sevm_start_session (a web3.py driver script or a .t.sol Foundry test)
loads and compiles the target and stops at the first contract instruction.
Then navigate (sevm_continue / sevm_step_opcodes / sevm_run_to), inspect with
windowed reads (sevm_read_memory, sevm_disassemble, sevm_read_source), evaluate
Solidity against the paused frame (sevm_evaluate), and mutate live state
(sevm_write_storage, sevm_set_stack_slot, sevm_set_gas).

Every navigation tool returns the same stop report (pc, opcode, gas, stack top,
stack delta since the previous stop), so you always know where you are in one
call. Results that can exceed your context are windowed and carry
`truncated: true` — narrow the offset instead of asking for more.

Unlike a human at a terminal you cannot scroll or watch panes: rely on
sevm_diff_since_last_stop after each step to see what changed (stack pushes /
pops with values, changed memory ranges, gas spent).
"""


def _call(driver: DebugDriver, fn: str, *args: Any, **kwargs: Any) -> dict[str, Any]:
    try:
        method = getattr(driver, fn)
        return method(*args, **kwargs)
    except DriverError as exc:
        return {"error": str(exc)}
    except Exception as exc:  # a bug must not kill the server
        return {"error": f"{type(exc).__name__}: {exc}"}


async def _run(driver: DebugDriver, fn: str, *args: Any, **kwargs: Any) -> dict[str, Any]:
    return await anyio.to_thread.run_sync(lambda: _call(driver, fn, *args, **kwargs))


def build_server(driver: DebugDriver | None = None) -> MCPServer:
    driver = driver or DebugDriver()
    server = MCPServer("sevm", instructions=INSTRUCTIONS)

    # -- lifecycle ---------------------------------------------------------

    @server.tool()
    async def sevm_start_session(
        target: str,
        args: list[str] | None = None,
        contracts: str | None = None,
        solc: str | None = None,
        optimize: bool = False,
        match: str | None = None,
        match_contract: str | None = None,
        timeout: float | None = None,
    ) -> dict:
        """Start debugging a target and stop at the first contract instruction.

        `target` is either a web3.py driver script (.py — it deploys and calls
        contracts against an in-process chain; extra `args` are forwarded to it,
        `@path` args are read from that file) or a Foundry test (.t.sol — use
        `match`/`match_contract` to select tests). Replaces any existing session.
        Returns the uniform stop report. `optimize` on = gadget-accurate bytecode
        but coarser source stepping.
        """
        return await _run(
            driver,
            "start",
            target,
            args=args,
            contracts=contracts,
            solc=solc,
            optimize=optimize,
            match=match,
            match_contract=match_contract,
            timeout=timeout,
        )

    @server.tool()
    async def sevm_stop_session() -> dict:
        """Stop and dispose the active debug session (chain state is discarded)."""
        return await _run(driver, "stop")

    @server.tool()
    async def sevm_restart_session(args: list[str] | None = None) -> dict:
        """Re-run the target from scratch: fresh chain, breakpoints kept.

        Pass `args` to replace the script's arguments (e.g. new calldata for a
        payload-driver script; `@path` args are read from file).
        """
        return await _run(driver, "restart", args=args)

    # -- navigation (uniform stop report) ----------------------------------

    @server.tool()
    async def sevm_continue() -> dict:
        """Run until the next breakpoint, watchpoint, error/revert, or program end."""
        return await _run(driver, "continue_execution")

    @server.tool()
    async def sevm_step_opcodes(count: int = 1) -> dict:
        """Execute `count` opcodes (max 1000), stepping into calls."""
        return await _run(driver, "step_opcodes", count)

    @server.tool()
    async def sevm_step_lines(count: int = 1) -> dict:
        """Advance `count` Solidity source lines, stepping into internal calls."""
        return await _run(driver, "step_lines", count)

    @server.tool()
    async def sevm_next_lines(count: int = 1) -> dict:
        """Advance `count` Solidity lines, stepping over internal calls."""
        return await _run(driver, "next_lines", count)

    @server.tool()
    async def sevm_finish_frame() -> dict:
        """Run to the end of the current frame (function call or internal call)."""
        return await _run(driver, "finish_frame")

    @server.tool()
    async def sevm_run_to(location: str | None = None, pc: int | None = None) -> dict:
        """Run to a location: `pc` (int), or `location` like 'Bank.sol:46' / '*0x108'."""
        return await _run(driver, "run_to", location=location, pc=pc)

    @server.tool()
    async def sevm_get_status(stack_top: int = 5) -> dict:
        """Where am I? Full stop report: pc, opcode, gas, sp, location, stack top."""
        return await _run(driver, "status")

    @server.tool()
    async def sevm_diff_since_last_stop() -> dict:
        """What did the last navigation change? Stack pushes/pops with values,
        changed memory byte-ranges, gas spent, pc move. Your replacement for
        watching panes scroll."""
        return await _run(driver, "diff_since_last_stop")

    # -- inspection --------------------------------------------------------

    @server.tool()
    async def sevm_read_memory(offset: int = 0, words: int = 8) -> dict:
        """Read a window of EVM memory as 32-byte words (0x + 64 hex chars each),
        annotated with Solidity regions (scratch/free-memory-pointer/zero-slot);
        words past the allocation carry `beyond: true`."""
        return await _run(driver, "read_memory", offset, words)

    @server.tool()
    async def sevm_read_stack(offset: int = 0, limit: int = 32) -> dict:
        """Read the operand stack; index 0 is the top. Hex + decimal per slot."""
        return await _run(driver, "read_stack", offset, limit)

    @server.tool()
    async def sevm_read_storage(
        slots: list[int] | None = None, decode: bool = True
    ) -> dict:
        """Read storage. By default decodes the running contract's whole layout
        (names, types, values — packed slots included); pass `slots` for raw
        32-byte reads of specific slots."""
        return await _run(driver, "read_storage", slots, decode)

    @server.tool()
    async def sevm_read_calldata(offset: int = 0, size: int = 256) -> dict:
        """Read the transaction calldata (windowed; huge payloads stay bounded).
        The first window includes the 4-byte selector and its signature."""
        return await _run(driver, "read_calldata", offset, size)

    @server.tool()
    async def sevm_get_backtrace() -> dict:
        """The call stack: Solidity frames interleaved with EVM frames."""
        return await _run(driver, "get_backtrace")

    @server.tool()
    async def sevm_get_locals() -> dict:
        """Local variables of the selected Solidity frame, decoded."""
        return await _run(driver, "get_locals")

    @server.tool()
    async def sevm_get_arguments() -> dict:
        """The current external frame's call arguments, ABI-decoded."""
        return await _run(driver, "get_arguments")

    @server.tool()
    async def sevm_disassemble(
        around_pc: int | None = None, before: int = 6, after: int = 18
    ) -> dict:
        """Disassembly rows {pc, text, jumpdest, line} around a pc (default: current)."""
        return await _run(driver, "disassemble", around_pc, before, after)

    @server.tool()
    async def sevm_read_source(
        file: str | None = None, around_line: int | None = None, context: int = 10
    ) -> dict:
        """A window of Solidity source with line numbers; the current line is
        flagged. `file` defaults to the file of the current stop."""
        return await _run(driver, "read_source", file, around_line, context)

    @server.tool()
    async def sevm_get_gas_profile(limit: int = 20) -> dict:
        """Gas spent so far, profiled by opcode and by source line."""
        return await _run(driver, "get_gas_profile", limit)

    @server.tool()
    async def sevm_get_logs() -> dict:
        """Event logs emitted so far this transaction, with decoded event names."""
        return await _run(driver, "get_logs")

    @server.tool()
    async def sevm_list_contracts() -> dict:
        """The compiled contracts in this session (name, runtime size)."""
        return await _run(driver, "list_contracts")

    @server.tool()
    async def sevm_list_functions(contract: str | None = None) -> dict:
        """External functions of a contract: signature + 4-byte selector.
        Defaults to the running contract."""
        return await _run(driver, "list_functions", contract)

    @server.tool()
    async def sevm_find_bytes(pattern: str, scope: str = "code", limit: int = 50) -> dict:
        """Find a hex byte pattern (e.g. '60515255' or '0x5b50505050').

        scope=code searches the running contract's bytecode for gadget hunting:
        each hit reports its pc, whether it starts on an instruction boundary,
        and the nearest preceding JUMPDEST (the address an indirect jump can
        reach). scope=memory/calldata search those buffers instead."""
        return await _run(driver, "find_bytes", pattern, scope, limit)

    # -- breakpoints -------------------------------------------------------

    @server.tool()
    async def sevm_set_breakpoint(
        location: str, condition: str | None = None, temporary: bool = False
    ) -> dict:
        """Break at 'File.sol:LINE', '*0xPC', a function name ('deposit' or
        'Bank.deposit'), or use sevm_set_opcode_breakpoint for opcodes.
        `condition` is real Solidity evaluated at the location."""
        return await _run(driver, "set_breakpoint", location, condition, temporary)

    @server.tool()
    async def sevm_set_opcode_breakpoint(
        mnemonic: str, condition: str | None = None, temporary: bool = False
    ) -> dict:
        """Break on every occurrence of an opcode, e.g. SSTORE, DELEGATECALL, JUMP."""
        return await _run(driver, "set_opcode_breakpoint", mnemonic, condition, temporary)

    @server.tool()
    async def sevm_list_breakpoints() -> dict:
        """All breakpoints and watchpoints with their hit counts."""
        return await _run(driver, "list_breakpoints")

    @server.tool()
    async def sevm_delete_breakpoint(id: int) -> dict:
        """Delete breakpoint or watchpoint number `id`."""
        return await _run(driver, "delete_breakpoint", id)

    @server.tool()
    async def sevm_set_watchpoint(expression: str, mode: str = "write") -> dict:
        """Break when a storage value is written ('write'), read ('read'), or
        either ('access'). Expression: a state variable, mapping element like
        'balances[0xabc...]', or '*0x40' for a memory word."""
        return await _run(driver, "set_watchpoint", expression, mode)

    # -- mutation & evaluation ---------------------------------------------

    @server.tool()
    async def sevm_evaluate(expression: str, keep: bool = False) -> dict:
        """Evaluate real Solidity against the paused frame: state variables,
        mappings, locals, internal functions, keccak256, abi.encode, units
        ('1 ether'). Side effects are discarded unless keep=true (like gdb's
        `call`). The most powerful inspection tool — prefer it over raw reads."""
        return await _run(driver, "evaluate", expression, keep)

    @server.tool()
    async def sevm_set_gas(value: int) -> dict:
        """Overwrite the gas meter. If stopped on an out-of-gas error, continuing
        afterwards retries the failed instruction with this gas."""
        return await _run(driver, "set_gas", value)

    @server.tool()
    async def sevm_set_stack_slot(index: int, value: int) -> dict:
        """Rewrite a stack operand (index 0 = top) before its opcode consumes it."""
        return await _run(driver, "set_stack_slot", index, value)

    @server.tool()
    async def sevm_write_memory(offset: int, hex_data: str) -> dict:
        """Write bytes (hex) into EVM memory at `offset`."""
        return await _run(driver, "write_memory", offset, hex_data)

    @server.tool()
    async def sevm_write_storage(slot: int, value: int) -> dict:
        """Write a raw 32-byte value to a storage slot of the running contract."""
        return await _run(driver, "write_storage", slot, value)

    @server.tool()
    async def sevm_set_pc(pc: int) -> dict:
        """Move the program counter; JUMPDEST targets only."""
        return await _run(driver, "set_pc", pc)

    @server.tool()
    async def sevm_command(line: str) -> dict:
        """Raw gdb-verb passthrough for everything not first-class: `vm.deal(a, 1
        ether)` cheatcodes, Yul like `mstore(0x80, 1)`, `jump 0x108`, `reseat`,
        `watch`, `tbreak`, `x/32xb 0x40`... Returns plain-text lines."""
        return await _run(driver, "command", line)

    return server
