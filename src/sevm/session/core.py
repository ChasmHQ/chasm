"""`DebugSession`: the patch, the threads, and the frame stack.

The rest of the engine lives beside this file and is called with the session passed in, so
what each part may touch is visible at the call site: `stepping` decides when to stop,
`snapshots` builds what the UI renders, `framelocals` recovers Solidity locals, and
`inspect_ops` serves the controller while the VM thread is parked.

Verified Py-EVM behaviours this relies on:
  * A `CALL` re-enters `apply_computation`, so EVM frames come for free.
  * `computation._stack.values`, `._memory._bytes`, `._gas_meter` and
    `code.program_counter` are all live and writable at a pause.
  * `state.snapshot()` / `state.revert()` make speculative execution safe.
"""

from __future__ import annotations

import queue
import threading
import traceback
from collections.abc import Callable, Sequence
from dataclasses import replace
from typing import Any

from eth.chains.base import Chain
from eth.exceptions import OutOfGas, Revert
from eth.vm.computation import BaseComputation

from ..breakpoints import WATCH_WRITE, BreakpointSet
from ..cheatcodes import (
    CONSOLE_ADDRESS,
    VM_ADDRESS,
    CheatError,
    CheatState,
    apply_cheat,
    decode_console_log,
)
from ..compile import Project
from ..frames import EvmFrame, FrameSnapshot, FunctionIndex, InternalFrame
from ..locals import LocalsIndex, LocalValue
from ..srcmap import Location, build_line_indexes
from . import framelocals, patch, snapshots, stepping
from .code import CodeIndex
from .events import Failure, Finished, Inspect, Paused, Resume, SessionError, StepMode
from .inspect_ops import InspectOps

_JUMP_MNEMONICS = frozenset({"JUMP", "JUMPI"})

# `Error(string)` selector, so a failed cheatcode reverts with a decodable reason.
_ERROR_SELECTOR = bytes.fromhex("08c379a0")


def _error_payload(reason: str) -> bytes:
    from eth_abi import encode

    return _ERROR_SELECTOR + encode(["string"], [reason])


class DebugSession:
    """Owns the patch, the threads, and the stop policy."""

    def __init__(
        self,
        project: Project,
        breakpoints: BreakpointSet | None = None,
        stop_at_start: bool = True,
        skip_to_source: bool = True,
    ) -> None:
        self.project = project
        self.breakpoints = breakpoints or BreakpointSet()
        # Foundry cheatcode bookkeeping (prank/labels/console output). Populated by the
        # cheatcode intercept in the patched loop and by the interactive `vm.*` command.
        self.cheats = CheatState()
        # When driving a Foundry test, the cheatcode/console addresses are given non-empty
        # code so Solidity's extcodesize guard on `vm.*` external calls does not revert
        # before dispatch. Off for plain web3 scripts, which never touch those addresses.
        self.foundry_mode = False
        self.functions = FunctionIndex(project.asts)
        self.locals = LocalsIndex(project.asts)
        self.line_indexes = build_line_indexes(project.sources.values())
        self.stop_at_start = stop_at_start
        self.skip_to_source = skip_to_source
        # Stops at the failing instruction with the frame still alive, unlike a post-mortem
        # trace. On by default.
        self.stop_on_revert = True

        self._cmd_q: queue.Queue[Any] = queue.Queue()
        self._event_q: queue.Queue[Any] = queue.Queue()
        self._reply_q: queue.Queue[Any] = queue.Queue()

        self._frames: list[EvmFrame] = []
        self._thread: threading.Thread | None = None
        self._patched = False
        self._local = threading.local()  # re-entrancy guard for speculative execution

        # How to relaunch the target for `reset` / `run`: argv -> target callable.
        # Bound by `sevm run`; sessions started directly (tests, embedding) have none.
        self._restart_factory: Callable[[list[str]], Callable[[], Any]] | None = None
        self._restart_argv: list[str] = []
        # Set by `set $gas` while parked on an out-of-gas error; the opcode loop reads
        # it right after the pause lifts and retries the instruction instead of raising.
        self._gas_rescued = False

        self.armed = False
        self.finished = False
        self.step_index = 0
        self.estimations = 0  # gas-estimation passes skipped, surfaced by `info frame`
        self.gas_by_line: dict[tuple[int, int], int] = {}
        self.gas_by_opcode: dict[str, int] = {}
        self.last_snapshot: FrameSnapshot | None = None
        self.exit_error: str | None = None
        # Decoded reason of the last transaction that reverted; a receipt is only status 0.
        self.last_revert: str | None = None

        # Stop policy state, only mutated on the VM thread.
        # The first stop uses STEP so the session opens on the user's first line of
        # Solidity rather than on the selector dispatcher at pc 0.
        self._mode = StepMode.STEP if skip_to_source else StepMode.STEPI
        self._mode_depth = 0
        self._mode_internal = 0
        self._mode_key: tuple[int, int] | None = None
        self._mode_target_pc: int | None = None
        self._mode_function: Any = None
        self._mode_at_function_entry = False
        self._pending_count = 1
        self._pending_annotation = ""  # detail for the stop `stepping` just decided

        self.code = CodeIndex(project, self.line_indexes, self.locals, self.breakpoints)

        self._eval_hook: Callable[..., Any] | None = None
        self._inspect_ops = InspectOps(self)

    # ==================================================================
    # lifecycle
    # ==================================================================

    def install(self) -> None:
        if self._patched:
            return
        BaseComputation.apply_computation = patch.make_apply_patch(self)
        Chain.estimate_gas = patch.make_estimate_patch(self)
        self._patched = True

    def uninstall(self) -> None:
        if not self._patched:
            return
        BaseComputation.apply_computation = patch.ORIGINAL_APPLY
        Chain.estimate_gas = patch.ORIGINAL_ESTIMATE
        self._patched = False

    def set_eval_hook(self, hook: Callable[..., Any]) -> None:
        """Wire in the Solidity expression evaluator (avoids a circular import)."""
        self._eval_hook = hook

    def start(self, target: Callable[[], Any]) -> None:
        """Run `target` on the VM thread with the debugger armed."""
        if self._thread is not None:
            raise SessionError("session already started")
        self.install()
        self.armed = True

        def runner() -> None:
            try:
                target()
                self._event_q.put(Finished(ok=True))
            except BaseException as exc:
                self.exit_error = f"{type(exc).__name__}: {exc}"
                self._event_q.put(
                    Finished(
                        ok=False, error=self.exit_error, traceback=traceback.format_exc()
                    )
                )
            finally:
                self.finished = True
                self.armed = False

        self._thread = threading.Thread(target=runner, name="sevm-vm", daemon=True)
        self._thread.start()

    def wait(self, timeout: float | None = None) -> Any:
        """Block for the next event: Paused or Finished."""
        try:
            event = self._event_q.get(timeout=timeout)
        except queue.Empty:
            return None
        if isinstance(event, Paused):
            self.last_snapshot = event.snapshot
        return event

    def set_restart_factory(
        self, factory: Callable[[list[str]], Callable[[], Any]], initial_argv: list[str]
    ) -> None:
        """Remember how to relaunch the target, for the `reset` and `run` commands."""
        self._restart_factory = factory
        self._restart_argv = list(initial_argv)

    def restart(self, argv: list[str] | None = None, timeout: float = 120.0) -> Any:
        """Re-run the target from scratch and return the first stop event.

        Breakpoints, watchpoints and displays survive the restart, as they do in gdb;
        the chain does not — a fresh script run builds a fresh tester chain, which is
        what makes a restart a genuine clean slate for iterating on calldata.
        """
        if self._restart_factory is None:
            raise SessionError(
                "no restart target; the session was not started by `sevm run`"
            )
        if self._thread is not None and not self.finished:
            self._cmd_q.put(Resume(mode=StepMode.RUN, detach=True))
            while True:
                event = self.wait(timeout=timeout)
                if event is None:
                    raise SessionError("the current run did not finish; restart aborted")
                if isinstance(event, Finished):
                    break
        if self._thread is not None:
            self._thread.join(timeout=timeout)
        for q in (self._event_q, self._reply_q, self._cmd_q):
            while True:
                try:
                    q.get_nowait()
                except queue.Empty:
                    break
        self._thread = None
        self.finished = False
        self.last_snapshot = None
        self.exit_error = None
        self.last_revert = None
        self.estimations = 0
        self.step_index = 0
        self.gas_by_line.clear()
        self.gas_by_opcode.clear()
        # Stop policy back to the opening stop, exactly as a fresh session.
        self._mode = StepMode.STEP if self.skip_to_source else StepMode.STEPI
        self._mode_depth = 0
        self._mode_internal = 0
        self._mode_key = None
        self._mode_target_pc = None
        self._mode_function = None
        self._mode_at_function_entry = False
        self._pending_count = 1
        self._pending_annotation = ""
        # Watchpoints re-arm against the new chain instead of diffing a dead one.
        for wp in self.breakpoints.active_watchpoints():
            wp.initialised = False
            wp.old_value = None
        if argv is not None:
            self._restart_argv = list(argv)
        self.start(self._restart_factory(self._restart_argv))
        return self.wait(timeout=timeout)

    def resume(
        self,
        mode: StepMode = StepMode.STEPI,
        count: int = 1,
        target_pc: int | None = None,
        timeout: float | None = None,
    ) -> Any:
        """Release the VM and block until it stops again or the program ends."""
        if self.finished:
            return Finished(ok=True)
        self._cmd_q.put(Resume(mode=mode, count=max(1, count), target_pc=target_pc))
        return self.wait(timeout=timeout)

    def detach(self, timeout: float = 10.0) -> None:
        """Let the program run to completion untraced, then restore Py-EVM."""
        if self._thread is not None and not self.finished:
            self._cmd_q.put(Resume(mode=StepMode.RUN, detach=True))
            deadline_event = self.wait(timeout=timeout)
            while isinstance(deadline_event, Paused):
                self._cmd_q.put(Resume(mode=StepMode.RUN, detach=True))
                deadline_event = self.wait(timeout=timeout)
        if self._thread is not None:
            self._thread.join(timeout=timeout)
        self.uninstall()

    def inspect(
        self,
        op: str,
        *args: Any,
        timeout: float = 30.0,
        frame_index: int | None = None,
        **kwargs: Any,
    ) -> Any:
        """Run an operation on the VM thread while it is parked at a pause."""
        if self.finished or self.last_snapshot is None:
            raise SessionError("not stopped; nothing to inspect")
        self._cmd_q.put(Inspect(op=op, args=args, kwargs=kwargs, frame_index=frame_index))
        try:
            result = self._reply_q.get(timeout=timeout)
        except queue.Empty as exc:
            raise SessionError(f"inspect {op!r} timed out") from exc
        if isinstance(result, Failure):
            raise SessionError(result.error)
        return result

    def refresh_snapshot(self) -> FrameSnapshot | None:
        """Re-read the live frame into `last_snapshot` after a mutation.

        A snapshot is a copy taken at the pause; a write to memory, stack or a local
        would otherwise leave every pane showing the pre-write state (storage reads
        already go live). This copies the mutated fields back in.

        Returns the existing snapshot if the program is no longer stopped; a mutation
        is always followed by a refresh, and a resume in between is not an error.
        """
        if self.finished or self.last_snapshot is None:
            return self.last_snapshot
        try:
            live = self.inspect("resnapshot")
        except SessionError:
            return self.last_snapshot
        self.last_snapshot = replace(self.last_snapshot, **live)
        return self.last_snapshot

    # ==================================================================
    # breakpoint helpers (controller thread)
    # ==================================================================

    def file_id_for(self, source_key: str) -> int | None:
        return self.code.file_id_for(source_key)

    def resolve_line(self, file_id: int, line: int) -> tuple[int, list[int]]:
        return self.code.resolve_line(file_id, line)

    def break_at_line(
        self,
        source_key: str,
        line: int,
        temporary: bool = False,
        condition: str | None = None,
    ):
        file_id = self.file_id_for(source_key)
        if file_id is None:
            raise SessionError(f"no source file matching {source_key!r}")
        snapped, pcs = self.resolve_line(file_id, line)
        bp = self.breakpoints.add_line(
            f"{source_key}:{snapped}",
            file_id,
            snapped,
            pcs,
            temporary=temporary,
            condition=condition,
        )
        return bp, snapped

    def break_at_function(
        self, name: str, temporary: bool = False, condition: str | None = None
    ):
        matches = self.functions.find(name)
        if not matches:
            raise SessionError(f"no function named {name!r}")
        if len(matches) > 1:
            names = ", ".join(sorted({m.display_name for m in matches}))
            raise SessionError(f"{name!r} is ambiguous; try one of: {names}")
        fn = matches[0]
        index = self.line_indexes.get(fn.file_id)
        line = index.line_col(fn.start)[0] if index else 0
        # Attach to the first executable line INSIDE the body, matching gdb, which
        # breaks after the prologue rather than on the signature.
        body_line, pcs = self.resolve_line(fn.file_id, line + 1)
        if not pcs:
            body_line, pcs = self.resolve_line(fn.file_id, line)
        bp = self.breakpoints.add_function(
            fn.display_name,
            fn.file_id,
            body_line,
            pcs,
            temporary=temporary,
            condition=condition,
            contract=fn.contract,
        )
        return bp, body_line

    def break_at_opcode(
        self, mnemonic: str, temporary: bool = False, condition: str | None = None
    ):
        return self.breakpoints.add_opcode(
            mnemonic, temporary=temporary, condition=condition
        )

    def break_at_pc(self, pc: int, temporary: bool = False, condition: str | None = None):
        return self.breakpoints.add_pc(pc, temporary=temporary, condition=condition)

    def watch_storage(
        self,
        expression: str,
        slot: int,
        address: bytes | None = None,
        mode: str = WATCH_WRITE,
    ):
        return self.breakpoints.add_watch(
            expression, kind="storage", slot=slot, address=address, mode=mode
        )

    def watch_memory(self, expression: str, offset: int, size: int = 32):
        return self.breakpoints.add_watch(
            expression, kind="memory", offset=offset, size=size
        )

    # ==================================================================
    # the patched opcode loop lives in patch.py
    # ==================================================================
    # ==================================================================
    # cheatcodes (VM thread)
    # ==================================================================

    def _record_transaction_revert(self, computation: Any) -> None:
        """Keep the top-level revert reason: a receipt only carries `status = 0`."""
        from ..decode import decode_revert

        self.last_revert = decode_revert(bytes(computation.output or b""))

    def _ensure_cheat_code(self, state: Any) -> None:
        """Etch a byte at the cheatcode and console addresses so `extcodesize` is non-zero.

        Solidity guards a typed external call (`vm.deal(...)`) with `extcodesize(target) > 0`
        and reverts before the call if it is zero. forge does the same etch; without it the
        cheatcode call never dispatches to our intercept.
        """
        for addr in (VM_ADDRESS, CONSOLE_ADDRESS):
            if not state.get_code(addr):
                state.set_code(addr, b"\x00")

    def _run_cheatcode(self, computation: Any, message: Any) -> None:
        """Interpret a call to the Foundry VM address against the live state. On failure,
        revert the caller with an Error(string) so Solidity sees a normal revert."""
        try:
            output = apply_cheat(
                self.cheats,
                computation.state,
                bytes(message.data),
                bytes(message.sender),
            )
        except CheatError as exc:
            computation.output = _error_payload(str(exc))
            raise Revert(f"cheatcode: {exc}".encode()) from exc
        computation.output = output

    def _run_console(self, computation: Any, message: Any) -> None:
        """Decode a console.log payload and record it; the call always succeeds empty."""
        line = decode_console_log(bytes(message.data))
        if line is not None:
            self.cheats.console_lines.append(line)

    # ==================================================================
    # frame bookkeeping (VM thread)
    # ==================================================================

    def _enter_frame(self, computation: Any, message: Any) -> EvmFrame:
        code = bytes(message.code)
        artifact = self.code.artifact_for(code, message.is_create)
        pc_map = self.code.pcmap_for(code, artifact, message.is_create)
        frame = EvmFrame(
            depth=message.depth,
            address=bytes(message.storage_address),
            code_address=bytes(message.code_address or b""),
            sender=bytes(message.sender),
            value=message.value,
            calldata=bytes(message.data),
            is_static=bool(message.is_static),
            is_create=bool(message.is_create),
            kind=self._frame_kind(message),
            artifact_name=artifact.name if artifact else None,
            computation=computation,
            pc_map=pc_map,
            disassembly=self.code.disassembly_for(code),
            decl_pcs=self.code.declpcs_for(code, pc_map, message.is_create),
        )
        frame.artifact = artifact  # type: ignore[attr-defined]
        self._frames.append(frame)
        return frame

    @staticmethod
    def _frame_kind(message: Any) -> str:
        if message.is_create:
            return "create"
        if message.depth == 0:
            return "tx"
        if message.is_static:
            return "staticcall"
        if bytes(message.code_address or b"") != bytes(message.storage_address):
            return "delegatecall"
        return "call"

    def _exit_frame(self, frame: EvmFrame) -> None:
        if self._frames and self._frames[-1] is frame:
            self._frames.pop()
        else:  # defensive: unwind to this frame if an exception skipped levels
            while self._frames and frame in self._frames:
                popped = self._frames.pop()
                if popped is frame:
                    break

    @property
    def current_frame(self) -> EvmFrame | None:
        return self._frames[-1] if self._frames else None

    # ==================================================================
    # the hook (VM thread)
    # ==================================================================

    def _on_opcode(
        self, frame: EvmFrame, computation: Any, pc: int, opcode: int, mnemonic: str
    ) -> None:
        self.step_index += 1
        if frame.decl_pcs and pc in frame.decl_pcs:
            self._observe_declaration(frame, computation, pc)
        loc = frame.location(pc)
        hits, reason = stepping.should_stop(
            self, frame, computation, pc, opcode, mnemonic, loc
        )
        if not hits and reason is None:
            return
        annotation, self._pending_annotation = self._pending_annotation, ""
        self._pause(
            frame,
            computation,
            pc,
            opcode,
            mnemonic,
            loc,
            reason or "breakpoint",
            hits,
            annotation=annotation,
        )

    def _observe_declaration(self, frame: EvmFrame, computation: Any, pc: int) -> None:
        """Record where a local just landed on the stack.

        The instruction about to run is the one that allocates the local, so the current
        stack height *is* its absolute position. Recording it again on a later pass (a
        loop body, a second call) simply overwrites with the same or the correct new
        value, which is what makes iteration and recursion work for free.
        """
        if not frame.internal:
            return
        var = frame.decl_pcs[pc]
        internal = frame.internal[-1]
        fn = internal.function
        # The declaration must belong to the function we are actually inside. Solc's
        # generated helpers borrow unrelated source ranges, and without this check one
        # of them could overwrite a good slot with a position from another frame.
        if fn is None or (
            fn.ast_id != var.function_id and not self.locals.owned_by_modifier(var)
        ):
            return
        internal.slots[var.ast_id] = len(computation._stack.values)

    def _on_vm_error(
        self, frame: EvmFrame, computation: Any, pc: int, mnemonic: str, error: Exception
    ) -> bool:
        """Park on the failing instruction; returns True iff a pause actually happened."""
        if not self.stop_on_revert or not self.armed:
            return False
        detail = (
            f"{type(error).__name__}: {error}" if str(error) else type(error).__name__
        )
        if mnemonic == "REVERT":
            from ..decode import decode_revert

            abi = frame.artifact.abi if frame.artifact else None
            detail = decode_revert(bytes(computation.output or b""), abi)
        if isinstance(error, OutOfGas):
            detail += "  — `set $gas = N` then `c` refills the meter and retries this"
            detail += " instruction"
        self._pause(
            frame,
            computation,
            pc,
            0,
            mnemonic,
            frame.location(pc),
            "error",
            (),
            annotation=detail,
        )
        return True

    def _account_gas(
        self, frame: EvmFrame, computation: Any, pc: int, mnemonic: str, gas_before: int
    ) -> None:
        """Attribute the opcode's real cost to its source line. This is a free profiler."""
        spent = gas_before - computation._gas_meter.gas_remaining
        if spent <= 0:
            return
        self.gas_by_opcode[mnemonic] = self.gas_by_opcode.get(mnemonic, 0) + spent
        loc = frame.location(pc)
        if loc is not None and not loc.is_generated and loc.line > 0:
            key = (loc.file_id, loc.line)
            self.gas_by_line[key] = self.gas_by_line.get(key, 0) + spent

    def _after_opcode(
        self, frame: EvmFrame, computation: Any, pc: int, mnemonic: str
    ) -> None:
        # Track the Solidity-level (internal) call stack. Internal calls are JUMPs, so
        # the EVM depth never moves and the source map's i/o field is the only signal.
        if mnemonic in _JUMP_MNEMONICS:
            loc = frame.location(pc)
            if loc is not None and loc.jump in ("i", "o"):
                taken = computation.code.program_counter != pc + 1
                if taken:
                    if loc.jump == "i":
                        dest = computation.code.program_counter
                        dest_loc = frame.location(dest)
                        frame.internal.append(
                            InternalFrame(
                                function=self.functions.at_location(dest_loc),
                                entry_pc=dest,
                                call_site_pc=pc,
                                # The stack height here is the frame base: the arguments
                                # the caller pushed sit directly below it.
                                entry_sp=len(computation._stack.values),
                            )
                        )
                    elif frame.internal:
                        frame.internal.pop()
        if self.breakpoints.has_watchpoints:
            stepping.check_watchpoints(self, frame, computation, pc, mnemonic)

    def _pause(
        self,
        frame: EvmFrame,
        computation: Any,
        pc: int,
        opcode: int,
        mnemonic: str,
        loc: Location | None,
        reason: str,
        hits: Sequence[int],
        annotation: str = "",
    ) -> None:
        # A `set $gas` from an earlier stop must not arm a rescue for this one.
        self._gas_rescued = False
        snapshot = snapshots.build_snapshot(
            self, frame, computation, pc, opcode, mnemonic, loc, reason, hits, annotation
        )
        self._event_q.put(Paused(snapshot))
        while True:
            cmd = self._cmd_q.get()
            if isinstance(cmd, Resume):
                if cmd.detach:
                    self.armed = False
                    return
                self._mode = cmd.mode
                self._pending_count = max(1, cmd.count)
                stepping.set_baseline(self, frame, loc)
                self._mode_target_pc = cmd.target_pc
                return
            if isinstance(cmd, Inspect):
                try:
                    result = self._inspect_ops.handle(cmd, frame, computation)
                except Exception as exc:
                    result = Failure(f"{type(exc).__name__}: {exc}")
                self._reply_q.put(result)
                continue
            self._reply_q.put(Failure(f"unknown command: {cmd!r}"))

    # ==================================================================
    # local variables (VM thread)
    # ==================================================================

    def frame_locals(
        self, frame: EvmFrame, computation: Any, internal_index: int | None = None
    ) -> list[LocalValue]:
        """Every local visible in one Solidity frame, decoded. See `framelocals`."""
        return framelocals.read_frame_locals(
            self.locals, frame, computation, internal_index
        )

    def suspended(self) -> _Suspend:
        """Context manager that stops the hook re-entering during speculative execution."""
        return _Suspend(self._local)


class _Suspend:
    def __init__(self, local: threading.local) -> None:
        self._local = local
        self._previous = False

    def __enter__(self) -> _Suspend:
        self._previous = getattr(self._local, "suspended", False)
        self._local.suspended = True
        return self

    def __exit__(self, *exc: Any) -> None:
        self._local.suspended = self._previous
