"""The MCP frontend's session driver.

The console and the TUI are thin renderers over `CommandProcessor`; this is the
third frontend, and the only thing it draws is JSON. `DebugDriver` owns one
debug session (single active target, replaced by the next start), mirrors the
wiring `chasm run` does for both target kinds, and remembers the previous stop so
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
        self._provenance_next = False
        self._fork_plan: Any = None
        self._fork_baseline: Any = None
        self.notices: list[str] = []

    # -- lifecycle ---------------------------------------------------------

    def _notice(self, message: str) -> None:
        self.notices.append(message)

    def _require(self) -> tuple[DebugSession, CommandProcessor]:
        if self._session is None or self._processor is None:
            raise DriverError(
                "no active debug session; call chasm_start_session first "
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
        reference_runtime_hex: str | None = None,
        provenance: bool = False,
        safe_foundry: bool = False,
        pause_at_entry: bool = True,
        breakpoints: list[str] | None = None,
    ) -> dict[str, Any]:
        """Compile and start a debug target; replaces any existing session.

        For a Foundry test, `breakpoints` (`File.sol:LINE`, `*PC` or a function
        name) are set before the first step. `pause_at_entry=False` runs from
        the test's first line to the first of them, or to the end.

        `provenance=true` records every executed opcode from the start, enabling
        `chasm_why_stack` and `chasm_export_trace` without a second pass.
        `reference_runtime_hex` (the deployed runtime, as an explorer or
        `cast code` prints it) hard-fails the start when the compiled contract
        diverges beyond the metadata tail: pcs and gadget offsets silently
        describe a different program otherwise.
        """
        with self._lock:
            self.stop()
            self.notices = []
            if timeout is not None:
                self.timeout = timeout
            if not os.path.isfile(target):
                raise DriverError(f"no such file: {target}")
            self._target_path = target
            self._provenance_next = bool(provenance)
            if target.endswith(".sol"):
                self._target_kind = "foundry"
                report = self._start_foundry(
                    target,
                    solc,
                    optimize,
                    match,
                    match_contract,
                    safe=safe_foundry,
                    pause_at_entry=pause_at_entry,
                    breakpoints=breakpoints or [],
                )
            else:
                self._target_kind = "script"
                report = self._start_script(target, args or [], contracts, solc, optimize)
            if reference_runtime_hex:
                verdict = self.check_parity(reference_runtime_hex)
                if not verdict["match"]:
                    detail = (
                        f"first divergence at 0x{verdict['first_diff_offset']:04x} "
                        f"(compiled {verdict['compiled_size']}B, reference "
                        f"{verdict['reference_size']}B)"
                    )
                    self.stop()
                    raise DriverError(
                        f"bytecode parity failed for {verdict['contract']}: {detail}; "
                        "rebuild with the deployed solc version and flags — every pc "
                        "past the divergence describes a different program"
                    )
                report["parity"] = verdict
            return report

    def start_replay(
        self,
        tx_hash: str | None = None,
        rpc_env: str | None = None,
        contracts: str | None = None,
        offline: str | None = None,
        capture: str | None = None,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        return self._start_mainnet(
            "replay",
            tx_hash=tx_hash,
            rpc_env=rpc_env,
            contracts=contracts,
            offline=offline,
            capture=capture,
            timeout=timeout,
        )

    def start_fork(
        self,
        block_number: int,
        rpc_env: str,
        to: str,
        caller: str,
        data: str = "0x",
        value: int = 0,
        gas: int = 3_000_000,
        gas_price: int | None = None,
        nonce: int | None = None,
        contracts: str | None = None,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        from ..cli import _fork_transaction

        try:
            target = _fork_transaction(
                to=to,
                caller=caller,
                data=data,
                value=value,
                gas=gas,
                gas_price=gas_price,
                nonce=nonce,
            )
        except ValueError as exc:
            raise DriverError(str(exc)) from None
        return self._start_mainnet(
            "fork",
            rpc_env=rpc_env,
            block=block_number,
            target=target,
            contracts=contracts,
            timeout=timeout,
        )

    def _start_mainnet(
        self,
        kind: str,
        *,
        tx_hash: str | None = None,
        rpc_env: str | None = None,
        block: int | None = None,
        contracts: str | None = None,
        offline: str | None = None,
        capture: str | None = None,
        timeout: float | None = None,
        target: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        from ..cli import _mainnet_project, _prepare_mainnet

        with self._lock:
            self.stop()
            self.notices = []
            if timeout is not None:
                self.timeout = timeout
            try:
                project = _mainnet_project(contracts)
                plan, baseline = _prepare_mainnet(
                    kind,
                    tx_hash=tx_hash,
                    rpc_env=rpc_env,
                    block=block,
                    offline=offline,
                    capture=capture,
                )
                session = DebugSession.from_fork_plan(
                    project, plan, target=target, baseline=baseline
                )
                self._target_kind = kind
                self._target_path = tx_hash if kind == "replay" else f"block {block}"
                self._provenance_next = False
                report = self._wire(project, None, session=session)
                self._fork_plan = plan
                self._fork_baseline = baseline
                return report
            except Exception as exc:
                self.stop()
                endpoint = os.environ.get(rpc_env or "", "")
                message = (
                    str(exc).replace(endpoint, "[RPC endpoint redacted]")
                    if endpoint
                    else str(exc)
                )
                raise DriverError(message) from None

    def fork_info(self) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            info = session.fork_info()
            if info is None:
                raise DriverError("the active session is not a fork or replay")
            return info

    def capabilities(self) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            snap = session.last_snapshot
            mapped = bool(
                snap
                and snap.contract_name
                and processor.project.artifact(snap.contract_name)
            )
            source = bool(snap and snap.has_source)
            locals_supported = mapped and not (
                processor.project.optimized
                or getattr(processor.project, "compiler_settings", {}).get("viaIR", False)
            )
            result: dict[str, Any] = {
                "source": source,
                "solidity": mapped,
                "locals": locals_supported,
                "mutate": bool(snap and not session.finished),
                "restart": self._target_kind in {"replay", "fork"},
            }
            if not mapped:
                result["reason"] = (
                    "raw bytecode; matching build-info is required for Solidity evaluation"
                )
            elif not locals_supported:
                result["reason"] = "optimized/viaIR local reconstruction is unsupported"
            elif not source:
                result["reason"] = "no source mapping at the current pc"
            return result

    def export_capture(self) -> dict[str, Any]:
        from ..fork.capture import capture_bundle
        from ..fork.execution import export_witness

        with self._lock:
            session, _ = self._require()
            if self._fork_plan is None or self._fork_baseline is None:
                raise DriverError("capture export requires a replay baseline")

            def merge(base: dict[str, Any], extra: dict[str, Any]) -> dict[str, Any]:
                combined = dict(base)
                for key, value in extra.items():
                    previous = combined.get(key)
                    combined[key] = (
                        merge(previous, value)
                        if isinstance(previous, dict) and isinstance(value, dict)
                        else value
                    )
                return combined

            witness = merge(self._fork_baseline.witness, export_witness(session._chain))
            return capture_bundle(
                self._fork_plan,
                self._fork_baseline,
                witness=witness,
            )

    def _start_script(
        self,
        script: str,
        args: list[str],
        contracts: str | None,
        solc: str | None,
        optimize: bool,
    ) -> dict[str, Any]:
        # Same wiring as `chasm run <script>`: the contracts dir becomes the compile
        # root unless it sits inside a Foundry project.
        from ..cli import _find_contracts_dir, _run_script
        from ..foundry import prepare_project
        from ..provider import RevmWeb3Driver

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

        def restart_factory(argv: list[str]) -> RevmWeb3Driver:
            return RevmWeb3Driver(_run_script(script, argv))

        return self._wire(
            project,
            restart_factory(expanded),
            restart_factory=restart_factory,
            restart_argv=expanded,
        )

    def _start_foundry(
        self,
        sol: str,
        solc: str | None,
        optimize: bool,
        match: str | None,
        match_contract: str | None,
        *,
        safe: bool = False,
        pause_at_entry: bool = True,
        breakpoints: list[str] | None = None,
    ) -> dict[str, Any]:
        from ..foundry import (
            compile_test,
            discover_tests,
            make_revm_tests_driver,
            prepare_project,
            select_tests,
        )

        if safe and os.name == "nt" and sol.startswith("\\\\?\\"):
            # Solidity source keys introduce forward slashes, which verbatim roots reject.
            if sol[4:8].upper() == "UNC\\":
                normalized = "\\\\" + sol[8:]
            elif len(sol) > 6 and sol[5:7] == ":\\":
                normalized = sol[4:]
            else:
                raise DriverError("unsupported Windows device path for a Foundry target")
            if not os.path.samefile(sol, normalized):
                raise DriverError(
                    "normalized Foundry path does not identify the approved target"
                )
            sol = normalized
        root = find_foundry_root(sol)
        if safe and root is None:
            raise DriverError("web Foundry mode requires an existing foundry.toml")
        if safe:
            from ..compile.foundry_config import read_foundry_config
            from ..compile.solcbin import find

            assert root is not None
            solc = solc or read_foundry_config(root).solc_version
            if not solc or find(solc) is None:
                raise DriverError(
                    "web Foundry mode requires a pinned, installed native solc"
                )
            compile_root, install_missing = root, False
        else:
            prepared = prepare_project(sol, assume_yes=True, allow_install=True)
            compile_root, install_missing = prepared.root, prepared.may_install
        try:
            if safe:
                project = compile_foundry_project(
                    compile_root,
                    target_file=os.path.abspath(sol),
                    solc_version=solc,
                    install_missing=False,
                    use_cache=False,
                    on_notice=self._notice,
                )
            else:
                project = compile_test(
                    sol,
                    compile_root,
                    solc_version=solc,
                    install_missing=install_missing,
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
        driver_fn = make_revm_tests_driver(project, selected)
        return self._wire(
            project,
            driver_fn,
            stop_functions=[f"{t.contract}.{t.function}" for t in selected],
            breakpoints=breakpoints or [],
            pause_at_entry=pause_at_entry,
        )

    def _wire(
        self,
        project: Any,
        target: Any,
        stop_functions: list[str] | None = None,
        restart_factory: Any = None,
        restart_argv: list[str] | None = None,
        session: Any = None,
        breakpoints: list[str] | None = None,
        pause_at_entry: bool = True,
    ) -> dict[str, Any]:
        session = session if session is not None else DebugSession(project)
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
        entry_points = list(session.breakpoints.breakpoints)
        if getattr(self, "_provenance_next", False):
            session.provenance.enabled = True
        self._session = session
        self._processor = CommandProcessor(session, evaluator)
        self._prev_snapshot = None
        # Launch breakpoints, set before the first step. One that does not
        # resolve is reported, not fatal: the rest still apply.
        for spec in breakpoints or []:
            try:
                row = self.set_breakpoint(spec)
                self._notice(f"breakpoint {row['id']}: {row['description']}")
            except Exception as exc:
                self._notice(f"breakpoint {spec!r} not set: {exc}")
        try:
            session.start(target)
            first = session.wait(timeout=self.timeout)
            if first is None:
                raise DriverError(
                    "timed out waiting for the target to reach contract code"
                )
            # Skip constructor stops before the selected test body.
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
            if isinstance(first, Finished) and not first.ok:
                raise DriverError(first.error or "target failed during startup")
            if not pause_at_entry and not isinstance(first, Finished):
                # The entry stops only found the test body; run on from there.
                for number in entry_points:
                    session.breakpoints.remove(number)
                advanced = session.resume(StepMode.RUN, count=1, timeout=self.timeout)
                if advanced is not None:
                    first = advanced
            if isinstance(first, Finished):
                self._notice(
                    "the test ran to the end without stopping"
                    + ("; no breakpoint was reached" if breakpoints else "")
                    + (" and passed" if first.ok else f" and failed: {first.error}")
                )
            return self._report(first)
        except Exception:
            self.stop()
            raise

    def stop(self) -> dict[str, Any]:
        with self._lock:
            session = self._session
            self._session = None
            self._processor = None
            self._target_path = None
            self._target_kind = None
            self._prev_snapshot = None
            self._fork_plan = None
            self._fork_baseline = None
            if session is None:
                return {"stopped_session": False}
            session.detach()
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
            if session.finished:
                return self._report(
                    Finished(ok=session.exit_error is None, error=session.exit_error)
                )
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
        if session is not None and session.fork_anchor is not None:
            report["fork"] = session.fork_info()
            report["baseline"] = session.baseline_report
            report["diverged"] = session.diverged
        if event is None or isinstance(event, Finished):
            report.update(
                stopped=False,
                finished=True,
                ok=bool(getattr(event, "ok", False)),
                exit_error=getattr(event, "error", None),
                exit_traceback=getattr(event, "traceback", None),
                last_revert=getattr(session, "last_revert", None) if session else None,
            )
            if session is not None:
                outcome = session.experiment_result
                report["execution"] = (
                    {
                        key: "0x" + bytes(outcome[key]).hex()
                        if isinstance(outcome.get(key), (bytes, bytearray))
                        else outcome.get(key)
                        for key in ("success", "gas_used", "output", "error")
                    }
                    if outcome is not None
                    else None
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
        self,
        slots: list[int] | None = None,
        decode: bool = True,
        limit: int | None = None,
    ) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            snap = self._require_snapshot()
            decoded_by_slot: dict[int, dict[str, Any]] = {}
            items: list[dict[str, Any]] = []
            decoder = None
            if decode:
                decoder = processor.decoder(snap.contract_name)
                if decoder:
                    reads = 0

                    def reader(slot: int) -> int:
                        nonlocal reads
                        reads += 1
                        if limit is not None and reads > 1024:
                            raise DriverError(
                                "storage decode read budget exceeded; request raw slots"
                            )
                        return int(session.inspect("read_storage", slot))

                    variables = (
                        decoder.variables
                        if limit is None
                        else decoder.variables[: max(1, limit)]
                    )
                    for var in variables:
                        try:
                            display = decoder.read_variable(reader, var).display
                        except Exception as exc:
                            display = f"<error: {exc}>"
                        row = {
                            "slot": var.slot,
                            "offset": var.offset,
                            "name": var.name,
                            "type": var.type_label,
                            "value": display,
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
            result: dict[str, Any] = {"address": hex_short(snap.address), "items": items}
            if limit is not None:
                result["truncated"] = bool(
                    decode and decoder and len(decoder.variables) > limit
                )
            return result

    def get_backtrace(self) -> dict[str, Any]:
        snap = self._require_snapshot()
        return {"frames": backtrace_rows(snap)}

    def get_locals(self) -> dict[str, Any]:
        with self._lock:
            _, processor = self._require()
            rows = processor.read_locals()
            return {"locals": rows}

    def get_arguments(self, max_calldata: int | None = None) -> dict[str, Any]:
        with self._lock:
            session, processor = self._require()
            snap = self._require_snapshot()
            info = session.inspect("frame_info")
            calldata = bytes(info["calldata"])
            if max_calldata is not None and len(calldata) > max_calldata:
                return {
                    "decoded": False,
                    "calldata_hex": "0x" + calldata[:max_calldata].hex(),
                    "total": len(calldata),
                    "truncated": True,
                }
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
            return {"rows": rows, "note": "row.current marks the execution pc"}

    def read_source(
        self,
        file: str | None = None,
        around_line: int | None = None,
        context: int = 10,
    ) -> dict[str, Any]:
        with self._lock:
            _, processor = self._require()
            snap = processor.snapshot
            source_key = file or (snap.source_key if snap else None)
            if source_key is None:
                raise DriverError("source unavailable for the current raw-bytecode frame")
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

    def get_logs(
        self, limit: int | None = None, data_limit: int | None = None
    ) -> dict[str, Any]:
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
                    "data": "0x"
                    + bytes(data[:data_limit] if data_limit is not None else data).hex(),
                    "event": event_name(topics),
                    **(
                        {"truncated": True, "data_size": len(data)}
                        if data_limit is not None and len(data) > data_limit
                        else {}
                    ),
                }
                for address, topics, data in (raw if limit is None else raw[:limit])
            ]
            result: dict[str, Any] = {"logs": items}
            if limit is not None:
                result.update(total=len(raw), truncated=len(raw) > limit)
            return result

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

    # -- provenance & trace ------------------------------------------------

    def set_provenance(self, enabled: bool) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            session.provenance.enabled = bool(enabled)
            return {
                "enabled": session.provenance.enabled,
                "records": len(session.provenance.records),
                "note": "why_stack and export_trace read this recording",
            }

    def why_stack(self, index_from_top: int) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            snap = self._require_snapshot()
            prov = session.provenance
            if not prov.enabled and not prov.records:
                raise DriverError(
                    "provenance recording is off; call chasm_set_provenance(True) "
                    "(or start the session with provenance=true) and navigate"
                )
            from ..session.provenance import why_stack as slice_stack

            bottom_up = tuple(entry.value for entry in reversed(snap.stack))
            result = slice_stack(prov.records, snap.depth, bottom_up, int(index_from_top))
            result["stack_index"] = int(index_from_top)
            return result

    def export_trace(
        self, path: str | None = None, offset: int = 0, limit: int = 500
    ) -> dict[str, Any]:
        with self._lock:
            session, _ = self._require()
            prov = session.provenance
            if not prov.records:
                raise DriverError(
                    "nothing recorded; enable provenance first and navigate"
                )
            import json

            if path:
                full = prov.structlog(0, len(prov.records))
                with open(path, "w", encoding="utf-8") as fh:
                    json.dump(full, fh)
                return {
                    "path": path,
                    "total": full["total"],
                    "written": len(full["structLogs"]),
                }
            return prov.structlog(max(0, offset), max(1, min(limit, 2000)))

    # -- parity ------------------------------------------------------------

    def check_parity(
        self, reference_hex: str, contract: str | None = None
    ) -> dict[str, Any]:
        """Compare a compiled runtime against a reference deployment (explorer hex)."""
        with self._lock:
            _session, processor = self._require()
            snap = self._require_snapshot()
            cleaned = reference_hex.lower().removeprefix("0x")
            if (
                not cleaned
                or len(cleaned) % 2
                or any(c not in "0123456789abcdef" for c in cleaned)
            ):
                raise DriverError(f"not a hex runtime: {reference_hex[:24]!r}")
            reference = bytes.fromhex(cleaned)
            name = contract or snap.contract_name
            art = processor.project.artifact(name) if name else None
            if art is None or not art.deployed_bytecode:
                raise DriverError(f"no compiled artifact for {name!r}")
            from ..compile import compare_runtime

            verdict = compare_runtime(art, reference)
            verdict["contract"] = art.name
            return verdict

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
                    "opcodes go through chasm_set_opcode_breakpoint"
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

    def list_breakpoints(self, structured: bool = False) -> dict[str, Any]:
        session, _ = self._require()
        if structured:
            points: list[Any] = [
                *session.breakpoints.breakpoints.values(),
                *session.breakpoints.watchpoints.values(),
            ]
            return {
                "breakpoints": [
                    {**self._breakpoint_row(bp), "enabled": bp.enabled}
                    for bp in sorted(points, key=lambda bp: bp.number)
                ]
            }
        return {"breakpoints": session.breakpoints.listing()}

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
        session, processor = self._require()
        res = processor.evaluate(expression, keep=keep)
        if keep:
            session.mark_diverged()
            session.refresh_snapshot()
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
        session.refresh_snapshot()
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
        session.refresh_snapshot()
        return {"index": int(index), "value": hex_word(result)}

    def write_memory(self, offset: int, hex_data: str) -> dict[str, Any]:
        session, _ = self._require()
        cleaned = hex_data.lower().removeprefix("0x")
        if not cleaned or len(cleaned) % 2:
            raise DriverError(f"not hex data: {hex_data!r}")
        data = bytes.fromhex(cleaned)
        session.inspect("write_memory", int(offset), data)
        session.refresh_snapshot()
        return {"offset": int(offset), "size": len(data)}

    def write_storage(self, slot: int, value: int) -> dict[str, Any]:
        session, _ = self._require()
        result = int(session.inspect("write_storage", int(slot), int(value)))
        session.refresh_snapshot()
        return {"slot": int(slot), "value": hex_word(result)}

    def set_pc(self, pc: int) -> dict[str, Any]:
        session, _ = self._require()
        try:
            result = int(session.inspect("set_pc", int(pc)))
        except SessionError as exc:
            raise DriverError(f"{exc} (only JUMPDEST targets are reachable)") from exc
        session.refresh_snapshot()
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
