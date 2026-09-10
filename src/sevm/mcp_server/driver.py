"""The MCP frontend's session driver.

The console and the TUI are thin renderers over `CommandProcessor`; this is the
third frontend, and the only thing it draws is JSON. `DebugDriver` owns one
debug session (single active target, replaced by the next start), mirrors the
wiring `sevm run` does for both target kinds, and remembers the previous stop so
it can answer "what changed?" — the question a human answers by watching panes.
"""

from __future__ import annotations

import os
import threading
from typing import Any

from ..commands.parsing import expand_file_args
from ..commands.processor import CommandProcessor
from ..commands.render import _plain
from ..compile import CompileError, compile_foundry_project, find_foundry_root
from ..evaluate import Evaluator, make_eval_hook
from ..frames import FrameSnapshot
from ..session import DebugSession, Finished, SessionError, StepMode
from .serialize import (
    WORD,
    backtrace_rows,
    hex_short,
    hex_word,
    memory_window,
    snapshot_report,
    stack_entry,
)

# Caps that keep one tool call from flooding a model's context window. Every
# truncated result says so, so the model knows to narrow its next request.
MAX_MEMORY_WORDS = 64
MAX_DISASM_ROWS = 128
MAX_SOURCE_LINES = 101
MAX_FIND_HITS = 200
MAX_MEMORY_SCAN = 65536


class DriverError(RuntimeError):
    """A tool-level failure with an actionable message."""


class DebugDriver:
    """One live debug target at a time, driven entirely from tool calls."""

    def __init__(self, timeout: float = 120.0) -> None:
        self.timeout = timeout
        self._lock = threading.RLock()
        self._session: DebugSession | None = None
        self._processor: CommandProcessor | None = None
        self._target_path: str | None = None
        self._target_kind: str | None = None  # "script" | "foundry"
        self._prev_snapshot: FrameSnapshot | None = None
        self.notices: list[str] = []

    # -- lifecycle ---------------------------------------------------------

    def _notice(self, message: str) -> None:
        self.notices.append(message)

    def _require(self) -> tuple[DebugSession, CommandProcessor]:
        if self._session is None or self._processor is None:
            raise DriverError(
                "no active debug session; call sevm_start_session first "
                "(script_path for a web3.py driver or a .t.sol Foundry test)"
            )
        return self._session, self._processor

    def start(
        self,
        target: str,
        args: list[str] | None = None,
        contracts: str | None = None,
        solc: str | None = None,
        optimize: bool = False,
        match: str | None = None,
        match_contract: str | None = None,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        """Compile and start a debug target; replaces any existing session."""
        with self._lock:
            self.stop()
            self.notices = []
            if timeout is not None:
                self.timeout = timeout
            if not os.path.isfile(target):
                raise DriverError(f"no such file: {target}")
            self._target_path = target
            if target.endswith(".sol"):
                self._target_kind = "foundry"
                return self._start_foundry(target, solc, optimize, match, match_contract)
            self._target_kind = "script"
            return self._start_script(target, args or [], contracts, solc, optimize)

    def _start_script(
        self,
        script: str,
        args: list[str],
        contracts: str | None,
        solc: str | None,
        optimize: bool,
    ) -> dict[str, Any]:
        # Same wiring as `sevm run <script>`: the contracts dir becomes the compile
        # root unless it sits inside a Foundry project.
        from ..cli import _find_contracts_dir, _run_script
        from ..foundry import prepare_project

        contracts_dir = _find_contracts_dir(script, contracts)
        if not os.path.isdir(contracts_dir):
            raise DriverError(
                f"no contracts directory at {contracts}; pass one via contracts="
            )
        contracts_dir = os.path.abspath(contracts_dir)
        root = find_foundry_root(contracts_dir) or contracts_dir
        source_dirs = [contracts_dir] if os.path.abspath(root) != contracts_dir else None
        prepared = prepare_project(
            contracts_dir,
            assume_yes=True,
            allow_install=True,
            needs_forge_std=False,
            source_dirs=source_dirs,
        )
        try:
            project = compile_foundry_project(
                prepared.root,
                source_dirs=source_dirs,
                solc_version=solc,
                optimize=optimize,
                install_missing=prepared.may_install,
                on_notice=self._notice,
            )
        except CompileError as exc:
            raise DriverError(f"compile failed: {exc}") from exc
        expanded = expand_file_args(list(args), " ".join(args))
        if isinstance(expanded, str):
            raise DriverError(expanded)
        target_fn = _run_script(script, expanded)
        return self._wire(
            project,
            target_fn,
            foundry_mode=True,
            restart_factory=lambda argv: _run_script(script, argv),
            restart_argv=expanded,
        )

    def _start_foundry(
        self,
        sol: str,
        solc: str | None,
        optimize: bool,
        match: str | None,
        match_contract: str | None,
    ) -> dict[str, Any]:
        from ..foundry import (
            compile_test,
            discover_tests,
            make_tests_driver,
            prepare_project,
            select_tests,
        )

        prepared = prepare_project(sol, assume_yes=True, allow_install=True)
        try:
            project = compile_test(
                sol,
                prepared.root,
                solc_version=solc,
                install_missing=prepared.may_install,
                on_notice=self._notice,
            )
        except CompileError as exc:
            raise DriverError(f"compile failed: {exc}") from exc
        targets = discover_tests(project)
        if not targets:
            raise DriverError("no test functions found (no-argument test*/invariant*)")
        selected = select_tests(targets, match=match, match_contract=match_contract)
        if not selected:
            available = ", ".join(f"{t.contract}.{t.function}" for t in targets)
            raise DriverError(
                f"no test matched match={match!r} contract={match_contract!r}; "
                f"available: {available}"
            )
        self._notice(
            "debugging " + ", ".join(f"{t.contract}.{t.function}" for t in selected)
        )
        driver_fn = make_tests_driver(project, selected)
        return self._wire(
            project,
            driver_fn,
            foundry_mode=True,
            stop_functions=[f"{t.contract}.{t.function}" for t in selected],
        )

    def _wire(
        self,
        project: Any,
        target: Any,
        foundry_mode: bool,
        stop_functions: list[str] | None = None,
        restart_factory: Any = None,
        restart_argv: list[str] | None = None,
    ) -> dict[str, Any]:
        session = DebugSession(project)
        session.foundry_mode = foundry_mode
        evaluator = Evaluator(project)
        session.set_eval_hook(make_eval_hook(evaluator))
        if restart_factory is not None:
            session.set_restart_factory(restart_factory, restart_argv or [])
        first_fn = first_contract = None
        for name in stop_functions or []:
            try:
                session.break_at_function(name)
            except Exception:  # a miss must not sink the rest, as in cli._debug
                continue
            if first_contract is None:
                first_contract, _, first_fn = name.partition(".")
        session.start(target)
        first = session.wait(timeout=self.timeout)
        if first is None:
            session.detach()
            raise DriverError("timed out waiting for the target to reach contract code")
        # Advance past the unavoidable constructor stop to the first test body.
        if first_fn is not None:
            for _ in range(64):
                snap = session.last_snapshot
                fn = getattr(snap, "function", None)
                if (
                    fn is not None
                    and getattr(fn, "name", None) == first_fn
                    and getattr(fn, "contract", None) == first_contract
                ):
                    break
                advanced = session.resume(StepMode.RUN, count=1, timeout=self.timeout)
                if advanced is not None:
                    first = advanced
                if isinstance(advanced, Finished):
                    break
        self._session = session
        self._processor = CommandProcessor(session, evaluator)
        self._prev_snapshot = None
        return self._report(first)

    def stop(self) -> dict[str, Any]:
        with self._lock:
            if self._session is None:
                return {"stopped_session": False}
            try:
                self._session.detach()
            except Exception:
                self._session.uninstall()
            self._session = None
            self._processor = None
            self._target_path = None
            self._target_kind = None
            self._prev_snapshot = None
            return {"stopped_session": True}

    def restart(self, args: list[str] | None = None) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            argv: list[str] | None = None
            if args:
                expanded = expand_file_args(list(args), " ".join(args))
                if isinstance(expanded, str):
                    raise DriverError(expanded)
                argv = expanded
            self._prev_snapshot = None
            try:
                event = session.restart(argv=argv, timeout=self.timeout)
            except SessionError as exc:
                raise DriverError(str(exc)) from exc
            return self._report(event)

    def status(self) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            return self._report(session.last_snapshot)

    # -- navigation --------------------------------------------------------

    def _navigate(
        self, mode: StepMode, count: int = 1, target_pc: int | None = None
    ) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            prev = session.last_snapshot
            result = processor.resume(mode, count=count, target_pc=target_pc)
            if result.error:
                raise DriverError(result.error)
            self._prev_snapshot = prev
            return self._report(result.event)

    def _report(self, event: Any) -> dict[str, Any]:
        session = self._session
        report: dict[str, Any] = {
            "target": self._target_path,
            "kind": self._target_kind,
        }
        if self.notices:
            report["notices"] = self.notices[-8:]
        if event is None or isinstance(event, Finished):
            report.update(
                stopped=False,
                finished=True,
                ok=bool(getattr(event, "ok", False)),
                exit_error=getattr(event, "error", None),
                last_revert=getattr(session, "last_revert", None) if session else None,
            )
            return report
        snap = getattr(event, "snapshot", None)
        if snap is None:
            snap = session.last_snapshot if session else None
        if snap is None:
            report.update(
                stopped=False,
                finished=True,
                ok=False,
                exit_error="no snapshot for this stop",
            )
            return report
        report.update(snapshot_report(snap))
        report["stack_delta"] = self._stack_delta(self._prev_snapshot, snap)
        return report

    @staticmethod
    def _stack_delta(prev: FrameSnapshot | None, snap: FrameSnapshot) -> dict[str, Any]:
        if prev is None or prev.depth != snap.depth:
            return {"available": False}
        old_sp, new_sp = len(prev.stack), len(snap.stack)
        delta: dict[str, Any] = {"available": True, "from": old_sp, "to": new_sp}
        old_values = [e.value for e in prev.stack]
        new_values = [e.value for e in snap.stack]
        if new_sp > old_sp:
            delta["pushed"] = [
                {"index": i, "hex": hex_word(v)}
                for i, v in enumerate(reversed(new_values[old_sp:]))
            ]
        elif new_sp < old_sp:
            delta["popped"] = [
                {"index": i, "hex": hex_word(v)}
                for i, v in enumerate(reversed(old_values[new_sp:]))
            ]
        return delta

    def continue_execution(self) -> dict[str, Any]:
        return self._navigate(StepMode.RUN)

    def step_opcodes(self, count: int = 1) -> dict[str, Any]:
        return self._navigate(StepMode.STEPI, count=min(max(count, 1), 1000))

    def step_lines(self, count: int = 1) -> dict[str, Any]:
        return self._navigate(StepMode.STEP, count=max(count, 1))

    def next_lines(self, count: int = 1) -> dict[str, Any]:
        return self._navigate(StepMode.NEXT, count=max(count, 1))

    def finish_frame(self) -> dict[str, Any]:
        return self._navigate(StepMode.FINISH)

    def run_to(
        self, location: str | None = None, pc: int | None = None
    ) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            target_pc: int | None = None
            if pc is not None:
                target_pc = pc
            elif location:
                if location.startswith("*"):
                    target_pc = int(location[1:], 0)
                else:
                    snap = processor.require_stop()
                    source_key, line = processor.parse_location(location, snap)
                    file_id = session.file_id_for(source_key)
                    if file_id is None:
                        raise DriverError(f"no source file matching {source_key!r}")
                    _snapped, pcs = session.resolve_line(file_id, line)
                    if not pcs:
                        raise DriverError(f"no code at {source_key}:{line}")
                    target_pc = min(pcs)
            else:
                raise DriverError("run_to needs a pc or a location like 'Bank.sol:46'")
            return self._navigate(StepMode.UNTIL, target_pc=target_pc)

    # -- inspection --------------------------------------------------------

    def read_memory(self, offset: int = 0, words: int = 8) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            snap = self._require_snapshot()
            words = max(1, min(words, MAX_MEMORY_WORDS))
            offset = max(0, offset)
            data = session.inspect("read_memory", offset, words * WORD)
            return {
                "offset": offset,
                "words": words,
                "memory_size": snap.memory_size,
                "items": memory_window(bytes(data), offset, snap.memory_size),
                "truncated": offset + words * WORD < snap.memory_size,
            }

    def _require_snapshot(self) -> FrameSnapshot:
        session, _ = self._require()
        snap = session.last_snapshot
        if snap is None or session.finished:
            raise DriverError("not stopped; nothing to inspect")
        return snap

    def read_stack(self, offset: int = 0, limit: int = 32) -> dict[str, Any]:
        snap = self._require_snapshot()
        limit = max(1, min(limit, 128))
        entries = snap.stack[offset : offset + limit]
        return {
            "offset": offset,
            "total": len(snap.stack),
            "items": [stack_entry(e) for e in entries],
            "truncated": offset + limit < len(snap.stack),
        }

    def read_calldata(self, offset: int = 0, size: int = 256) -> dict[str, Any]:
        with self._lock:
            _session, processor = self._require()
            snap = self._require_snapshot()
            size = max(1, min(size, 4096))
            data = bytes(snap.calldata[offset : offset + size])
            result: dict[str, Any] = {
                "offset": offset,
                "size": len(data),
                "total": len(snap.calldata),
                "hex": "0x" + data.hex() if data else "0x",
                "truncated": offset + size < len(snap.calldata),
            }
            if offset == 0 and len(snap.calldata) >= 4:
                result["selector"] = "0x" + snap.calldata[:4].hex()
                for candidate in processor.project.artifacts.values():
                    if snap.calldata[:4] in candidate.selectors:
                        result["signature"] = candidate.selectors[snap.calldata[:4]]
                        break
            return result

    def read_storage(
        self, slots: list[int] | None = None, decode: bool = True
    ) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            snap = self._require_snapshot()
            decoded_by_slot: dict[int, dict[str, Any]] = {}
            items: list[dict[str, Any]] = []
            if decode:
                decoder = processor.decoder(snap.contract_name)
                if decoder:

                    def reader(slot: int) -> int:
                        return int(session.inspect("read_storage", slot))

                    for var, value in decoder.read_all(reader):
                        row = {
                            "slot": var.slot,
                            "offset": var.offset,
                            "name": var.name,
                            "type": var.type_label,
                            "value": value.display,
                        }
                        decoded_by_slot[var.slot] = row
                        items.append(row)
            if slots:
                known = set(decoded_by_slot)
                for slot in slots:
                    slot = int(slot)
                    if slot in known:
                        continue
                    raw = int(session.inspect("read_storage", slot))
                    items.append({"slot": slot, "hex": hex_word(raw)})
            return {"address": hex_short(snap.address), "items": items}

    def get_backtrace(self) -> dict[str, Any]:
        snap = self._require_snapshot()
        return {"frames": backtrace_rows(snap)}

    def get_locals(self) -> dict[str, Any]:
        with self._lock:
            _, processor = self._require()
            rows = processor.read_locals()
            return {"locals": rows}

    def get_arguments(self) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            snap = self._require_snapshot()
            info = session.inspect("frame_info")
            calldata = bytes(info["calldata"])
            art = (
                processor.project.artifact(snap.contract_name)
                if snap.contract_name
                else None
            )
            decoded = None
            if art is not None:
                from ..decode import decode_calldata

                decoded = decode_calldata(art.abi, calldata)
            if decoded is None:
                return {"decoded": False, "calldata_hex": "0x" + calldata.hex()}
            signature, args = decoded
            return {
                "decoded": True,
                "signature": signature,
                "args": [
                    {"name": name, "type": type_, "value": str(value)}
                    for type_, name, value in args
                ],
            }

    def disassemble(
        self, around_pc: int | None = None, before: int = 6, after: int = 18
    ) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            self._require_snapshot()
            rows = session.inspect(
                "disassembly",
                max(1, min(before, MAX_DISASM_ROWS)),
                max(1, min(after, MAX_DISASM_ROWS)),
                around_pc,
            )
            return {"rows": rows, "note": "row.current marks the window centre"}

    def read_source(
        self,
        file: str | None = None,
        around_line: int | None = None,
        context: int = 10,
    ) -> dict[str, Any]:
        with self._lock:
            _, processor = self._require()
            snap = processor.snapshot
            source_key = file or (
                snap.source_key
                if snap and snap.source_key
                else next(iter(processor.project.sources))
            )
            lines = processor.source_lines(source_key)
            if not lines:
                raise DriverError(f"no source for {source_key}")
            centre = around_line or (snap.line if snap and snap.line else 1)
            context = max(1, min(context, MAX_SOURCE_LINES // 2))
            start = max(1, centre - context)
            end = min(len(lines), centre + context)
            items = [
                {
                    "line": n,
                    "text": lines[n - 1],
                    "current": bool(
                        snap and snap.source_key == source_key and n == snap.line
                    ),
                }
                for n in range(start, end + 1)
            ]
            return {
                "file": source_key,
                "total_lines": len(lines),
                "items": items,
                "truncated": start > 1 or end < len(lines),
            }

    def get_gas_profile(self, limit: int = 20) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            by_opcode = sorted(session.gas_by_opcode.items(), key=lambda kv: -kv[1])[
                :limit
            ]
            lines = []
            for (file_id, line), gas in sorted(
                session.gas_by_line.items(), key=lambda kv: -kv[1]
            )[:limit]:
                src = session.project.source_by_id(file_id)
                name = src.key if src else f"file {file_id}"
                try:
                    text = src.text.split("\n")[line - 1].strip() if src else ""
                except IndexError:
                    text = ""
                lines.append({"file": name, "line": line, "gas": gas, "text": text})
            return {"by_opcode": dict(by_opcode), "by_source_line": lines}

    def get_logs(self) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            self._require_snapshot()
            raw = session.inspect("logs")
            from eth_utils import event_abi_to_log_topic

            def event_name(topics: tuple[int, ...]) -> str | None:
                if not topics:
                    return None
                topic0 = topics[0].to_bytes(32, "big")
                for art in processor.project.artifacts.values():
                    for entry in art.abi:
                        if entry.get("type") != "event":
                            continue
                        try:
                            event: Any = entry  # eth_utils wants an ABIEvent-typed dict
                            if event_abi_to_log_topic(event) == topic0:
                                return str(entry.get("name"))
                        except Exception:
                            continue
                return None

            items = [
                {
                    "address": hex_short(address),
                    "topics": [f"0x{t:064x}" for t in topics],
                    "data": "0x" + bytes(data).hex(),
                    "event": event_name(topics),
                }
                for address, topics, data in raw
            ]
            return {"logs": items}

    # -- symbols -----------------------------------------------------------

    def list_contracts(self) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            return {
                "contracts": [
                    {
                        "name": art.name,
                        "qualified": qualified,
                        "runtime_size": len(art.deployed_bytecode),
                    }
                    for qualified, art in sorted(session.project.artifacts.items())
                ]
            }

    def list_functions(self, contract: str | None = None) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            snap = session.last_snapshot
            name = contract or (snap.contract_name if snap else None)
            art = session.project.artifact(name) if name else None
            if art is None:
                raise DriverError(f"no artifact named {name!r}")
            return {
                "contract": art.name,
                "functions": [
                    {"signature": sig, "selector": "0x" + sel}
                    for sig, sel in sorted(art.method_identifiers.items())
                ],
            }

    # -- search ------------------------------------------------------------

    def find_bytes(
        self, pattern: str, scope: str = "code", limit: int = 50
    ) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            snap = self._require_snapshot()
            cleaned = pattern.lower().removeprefix("0x")
            if (
                not cleaned
                or len(cleaned) % 2
                or any(c not in "0123456789abcdef" for c in cleaned)
            ):
                raise DriverError(f"not a hex pattern: {pattern!r}")
            needle = bytes.fromhex(cleaned)
            limit = max(1, min(limit, MAX_FIND_HITS))
            if scope == "code":
                found = session.inspect("find_needle", cleaned, limit)
                return {
                    "scope": scope,
                    "pattern": cleaned,
                    "code_size": found["code_size"],
                    "total": found["total"],
                    "hits": found["hits"][:limit],
                    "truncated": found["total"] > len(found["hits"]),
                }
            if scope == "calldata":
                data = bytes(snap.calldata)
            elif scope == "memory":
                window = min(snap.memory_size, MAX_MEMORY_SCAN)
                data = bytes(session.inspect("read_memory", 0, window))
            else:
                raise DriverError(
                    f"scope must be code, memory or calldata, not {scope!r}"
                )
            hits: list[dict[str, Any]] = []
            start = 0
            while len(hits) < limit:
                idx = data.find(needle, start)
                if idx < 0:
                    break
                hits.append({"offset": idx})
                start = idx + 1
            return {
                "scope": scope,
                "pattern": cleaned,
                "scanned": len(data),
                "total": data.count(needle),
                "hits": hits,
                "truncated": data.count(needle) > len(hits),
            }

    # -- breakpoints, mutation, evaluation ---------------------------------

    def set_breakpoint(
        self, location: str, condition: str | None = None, temporary: bool = False
    ) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            spec = location.strip()
            # break_at_line/function return (breakpoint, snapped_line); the pc and
            # opcode forms return the breakpoint itself. Unpack to the common shape.
            if spec.startswith("*"):
                result: Any = session.break_at_pc(
                    int(spec[1:], 0), temporary=temporary, condition=condition
                )
            elif ":" in spec:
                source_key, line_text = spec.rsplit(":", 1)
                file_id = session.file_id_for(source_key)
                if file_id is None:
                    raise DriverError(f"no source file matching {source_key!r}")
                snapped, _pcs = session.resolve_line(file_id, int(line_text))
                result = session.break_at_line(
                    source_key, snapped, temporary=temporary, condition=condition
                )
            elif spec.replace(".", "").replace("_", "").isalnum():
                result = session.break_at_function(
                    spec, temporary=temporary, condition=condition
                )
            else:
                raise DriverError(
                    "location must be '*0xPC', 'File.sol:LINE', or a function name; "
                    "opcodes go through sevm_set_opcode_breakpoint"
                )
            bp = result[0] if isinstance(result, tuple) else result
            return self._breakpoint_row(bp)

    def set_opcode_breakpoint(
        self, mnemonic: str, condition: str | None = None, temporary: bool = False
    ) -> dict[str, Any]:
        session, _ = self._require()
        bp = session.break_at_opcode(
            mnemonic.upper(), temporary=temporary, condition=condition
        )
        return self._breakpoint_row(bp)

    @staticmethod
    def _breakpoint_row(bp: Any) -> dict[str, Any]:
        return {
            "id": bp.number,
            "description": bp.describe(),
            "temporary": bool(getattr(bp, "temporary", False)),
            "condition": getattr(bp, "condition", None),
        }

    def list_breakpoints(self) -> dict[str, Any]:
        session, _ = self._require()
        rows = session.breakpoints.listing()
        return {"breakpoints": rows}

    def delete_breakpoint(self, id: int) -> dict[str, Any]:
        session, _ = self._require()
        removed = session.breakpoints.remove(int(id))
        if not removed:
            raise DriverError(f"no breakpoint {id}")
        return {"deleted": int(id)}

    def set_watchpoint(self, expression: str, mode: str = "write") -> dict[str, Any]:
        """watch/rwatch/awatch via the command layer, which resolves the expression."""
        _, processor = self._require()
        verb = {"write": "watch", "read": "rwatch", "access": "awatch"}.get(mode)
        if verb is None:
            raise DriverError("mode must be write, read or access")
        result = processor.execute(f"{verb} {expression}")
        if result.error:
            raise DriverError(result.error)
        return {"ok": True, "lines": [_plain(line) for line in result.lines]}

    def evaluate(self, expression: str, keep: bool = False) -> dict[str, Any]:
        _, processor = self._require()
        res = processor.evaluate(expression, keep=keep)
        out: dict[str, Any] = {
            "expression": res.expression,
            "type": res.type_name,
            "display": res.display,
            "kept": bool(res.kept),
            "gas_used": res.gas_used,
        }
        if isinstance(res.value, int):
            out["hex"] = hex_word(res.value)
        return out

    def set_gas(self, value: int) -> dict[str, Any]:
        session, _ = self._require()
        result = int(session.inspect("set_gas", int(value)))
        return {
            "gas_remaining": result,
            "note": (
                "if stopped on an out-of-gas error, continue now retries the failed "
                "instruction with this gas"
            ),
        }

    def set_stack_slot(self, index: int, value: int) -> dict[str, Any]:
        session, _ = self._require()
        result = int(session.inspect("write_stack", int(index), int(value)))
        return {"index": int(index), "value": hex_word(result)}

    def write_memory(self, offset: int, hex_data: str) -> dict[str, Any]:
        session, _ = self._require()
        cleaned = hex_data.lower().removeprefix("0x")
        if not cleaned or len(cleaned) % 2:
            raise DriverError(f"not hex data: {hex_data!r}")
        data = bytes.fromhex(cleaned)
        session.inspect("write_memory", int(offset), data)
        return {"offset": int(offset), "size": len(data)}

    def write_storage(self, slot: int, value: int) -> dict[str, Any]:
        session, _ = self._require()
        result = int(session.inspect("write_storage", int(slot), int(value)))
        return {"slot": int(slot), "value": hex_word(result)}

    def set_pc(self, pc: int) -> dict[str, Any]:
        session, _ = self._require()
        try:
            result = int(session.inspect("set_pc", int(pc)))
        except SessionError as exc:
            raise DriverError(f"{exc} (only JUMPDEST targets are reachable)") from exc
        return {"pc": result}

    def command(self, line: str) -> dict[str, Any]:
        """Raw passthrough to the gdb verb surface, markup stripped."""
        _, processor = self._require()
        result = processor.execute(line)
        return {
            "ok": result.ok,
            "lines": [_plain(entry).rstrip() for entry in result.lines],
            "error": result.error,
            "notice": result.notice,
        }

    def diff_since_last_stop(self) -> dict[str, Any]:
        """What the last navigation changed — the AI's replacement for watching panes."""
        with self._lock:
            self._require()
            snap = self._require_snapshot()
            prev = self._prev_snapshot
            if prev is None:
                return {
                    "available": False,
                    "reason": "no previous stop recorded in this session",
                }
            out: dict[str, Any] = {"available": True}
            out["stack"] = self._stack_delta(prev, snap)
            out["gas_used"] = prev.gas_remaining - snap.gas_remaining
            out["pc"] = {"from": prev.pc, "to": snap.pc}
            out["depth"] = {"from": prev.depth, "to": snap.depth}
            out["memory_size"] = {"from": prev.memory_size, "to": snap.memory_size}
            old_mem, new_mem = bytes(prev.memory), bytes(snap.memory)
            shared = min(len(old_mem), len(new_mem))
            ranges: list[tuple[int, int]] = []
            run_start = None
            for i in range(shared):
                changed = old_mem[i] != new_mem[i]
                if changed and run_start is None:
                    run_start = i
                elif not changed and run_start is not None:
                    ranges.append((run_start, i))
                    run_start = None
                    if len(ranges) >= 16:
                        break
            if run_start is not None and len(ranges) < 16:
                ranges.append((run_start, shared))
            out["memory_changed"] = [
                {
                    "offset": start,
                    "length": end - start,
                    "new_hex": "0x" + new_mem[start : min(end, start + 32)].hex(),
                }
                for start, end in ranges
            ]
            out["memory_note"] = (
                f"compared the first {shared} bytes captured at each stop "
                "(snapshots carry a bounded memory window)"
            )
            return out
