"""The stepping engine.

A monkeypatch on `BaseComputation.apply_computation` that reimplements Py-EVM's opcode
loop with a blocking hook in it. The debugged program runs on a worker thread; the
controller (console or TUI) drives it over two queues and the two threads strictly
alternate, so exactly one of them is ever runnable.

Thread contract, and it is not negotiable:

  * Only the VM thread touches Py-EVM objects. The controller receives immutable
    `FrameSnapshot`s and asks for anything else with an inspect command, which the VM
    thread services while parked inside the hook.
  * The controller is the only thing that blocks on user input. The VM thread blocks
    only on its command queue, so cancellation stays clean.

Verified behaviours this file relies on (see research/spikes/):
  * A `CALL` re-enters `apply_computation`, so EVM frames come for free.
  * `computation._stack.values`, `._memory._bytes`, `._gas_meter`, and
    `code.program_counter` are all live and writable at a pause.
  * `state.snapshot()` / `state.revert()` make speculative execution safe.
"""

from __future__ import annotations

import queue
import threading
import traceback
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field, replace
from enum import Enum
from typing import Any

from eth.chains.base import Chain
from eth.exceptions import Halt, Revert, VMError
from eth.vm.computation import NO_RESULT, BaseComputation
from eth.vm.logic.invalid import InvalidOpcode

from . import assembly
from .breakpoints import WATCH_ACCESS, WATCH_READ, WATCH_WRITE, BreakpointSet, Watchpoint
from .cheatcodes import (
    CONSOLE_ADDRESS,
    VM_ADDRESS,
    CheatError,
    CheatState,
    apply_cheat,
    decode_console_log,
)
from .compile import Artifact, Project
from .disasm import Disassembly
from .frames import (
    BacktraceRow,
    EvmFrame,
    FrameSnapshot,
    FunctionIndex,
    InternalFrame,
    StackEntry,
)
from .locals import (
    LocalsIndex,
    LocalValue,
    declaration_pcs,
    read_local,
)
from .srcmap import Location, PcMap, build_line_indexes

# The raw classmethod DESCRIPTOR, never the bound method. Restoring a bound method pins
# `cls` to BaseComputation, and the next real call gets opcodes=None and dies.
_ORIGINAL_APPLY = BaseComputation.__dict__["apply_computation"]

# `eth_estimateGas` binary-searches the gas limit by *running the transaction* many times,
# starting from the intrinsic gas, so the first probes fail with OutOfGas by design. Left
# untouched, the debugger stops inside those probes and the user sees a bogus out-of-gas
# in a transaction that succeeds. Estimation therefore runs with the hook suspended.
_ORIGINAL_ESTIMATE = Chain.__dict__["estimate_gas"]

_JUMP_MNEMONICS = frozenset({"JUMP", "JUMPI"})

# Call opcodes a prank applies to (the child's msg.sender / value source is the caller's
# storage_address): CALL, CALLCODE, STATICCALL. DELEGATECALL (0xF4) keeps the real caller.
_PRANK_CALL_OPCODES = frozenset({0xF1, 0xF2, 0xFA})

# `Error(string)` selector, so a failed cheatcode reverts with a decodable reason.
_ERROR_SELECTOR = bytes.fromhex("08c379a0")


def _error_payload(reason: str) -> bytes:
    from eth_abi import encode

    return _ERROR_SELECTOR + encode(["string"], [reason])


# How much memory travels with each snapshot. The rest is available on request.
SNAPSHOT_MEMORY_LIMIT = 4096


class StepMode(Enum):
    RUN = "run"  # breakpoints only
    STEPI = "stepi"  # one opcode, into calls
    NEXTI = "nexti"  # one opcode, over calls
    STEP = "step"  # one source line, into calls
    NEXT = "next"  # one source line, over calls
    FINISH = "finish"  # to the end of the current frame
    UNTIL = "until"  # to a specific pc


# -- controller -> VM commands -----------------------------------------------


@dataclass
class Resume:
    mode: StepMode = StepMode.STEPI
    count: int = 1
    target_pc: int | None = None
    detach: bool = False


@dataclass
class Inspect:
    """A read or mutation performed by the VM thread against the live computation."""

    op: str
    args: tuple[Any, ...] = ()
    kwargs: dict[str, Any] = field(default_factory=dict)
    frame_index: int | None = None  # which EVM frame; None means the innermost


# -- VM -> controller events -------------------------------------------------


@dataclass
class Paused:
    snapshot: FrameSnapshot


@dataclass
class Finished:
    ok: bool
    error: str | None = None
    traceback: str | None = None


@dataclass
class Failure:
    error: str


class SessionError(RuntimeError):
    pass


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
        self._pending_annotation = ""  # detail for the stop `_should_stop` just decided

        # Per-code caches, keyed by code identity.
        self._pcmap_cache: dict[bytes, PcMap | None] = {}
        self._declpc_cache: dict[bytes, dict[int, int]] = {}
        self._disasm_cache: dict[bytes, Disassembly] = {}
        self._artifact_cache: dict[bytes, Artifact | None] = {}

        self._eval_hook: Callable[..., Any] | None = None

    # ==================================================================
    # lifecycle
    # ==================================================================

    def install(self) -> None:
        if self._patched:
            return
        BaseComputation.apply_computation = self._make_patch()
        Chain.estimate_gas = self._make_estimate_patch()
        self._patched = True

    def uninstall(self) -> None:
        if not self._patched:
            return
        BaseComputation.apply_computation = _ORIGINAL_APPLY
        Chain.estimate_gas = _ORIGINAL_ESTIMATE
        self._patched = False

    def _make_estimate_patch(self) -> Any:
        session = self

        def estimate_gas(chain, transaction, at_header=None):  # type: ignore[no-untyped-def]
            session.estimations += 1
            with session.suspended():
                return _ORIGINAL_ESTIMATE(chain, transaction, at_header)

        return estimate_gas

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
    # code resolution
    # ==================================================================

    def _artifact_for(self, code: bytes, is_create: bool) -> Artifact | None:
        key = (
            bytes(code[:64])
            + len(code).to_bytes(4, "big")
            + (b"C" if is_create else b"R")
        )
        if key in self._artifact_cache:
            return self._artifact_cache[key]
        art: Artifact | None = None
        if is_create:
            # Creation code is `constructor bytecode + abi-encoded args`, so match on prefix.
            for candidate in self.project.artifacts.values():
                if candidate.bytecode and code.startswith(candidate.bytecode):
                    art = candidate
                    break
        else:
            art = self.project.artifact_for_code(code)
        self._artifact_cache[key] = art
        return art

    def _pcmap_for(
        self, code: bytes, artifact: Artifact | None, is_create: bool
    ) -> PcMap | None:
        if artifact is None:
            return None
        key = (
            bytes(code[:64])
            + len(code).to_bytes(4, "big")
            + (b"C" if is_create else b"R")
        )
        if key in self._pcmap_cache:
            return self._pcmap_cache[key]
        source_map = artifact.source_map if is_create else artifact.deployed_source_map
        pcmap = PcMap(code, source_map, self.line_indexes) if source_map else None
        self._pcmap_cache[key] = pcmap
        if pcmap is not None:
            self._resolve_pending_breakpoints(pcmap)
        return pcmap

    def _declpcs_for(
        self, code: bytes, pcmap: PcMap | None, is_create: bool
    ) -> dict[int, int]:
        """pc -> declaration AST id for this code object, built once and shared.

        This is the table that makes local-variable observation affordable: the hook
        does one dict lookup per opcode instead of resolving a source location.
        """
        if pcmap is None:
            return {}
        key = (
            bytes(code[:64])
            + len(code).to_bytes(4, "big")
            + (b"C" if is_create else b"R")
        )
        cached = self._declpc_cache.get(key)
        if cached is None:
            cached = declaration_pcs(pcmap, self.locals)
            self._declpc_cache[key] = cached
        return cached

    def _disasm_for(self, code: bytes) -> Disassembly:
        key = bytes(code[:64]) + len(code).to_bytes(4, "big")
        cached = self._disasm_cache.get(key)
        if cached is None:
            cached = Disassembly(code)
            self._disasm_cache[key] = cached
        return cached

    def _resolve_pending_breakpoints(self, pcmap: PcMap) -> None:
        for bp in list(self.breakpoints.breakpoints.values()):
            if bp.pending and bp.file_id >= 0 and bp.line > 0:
                pcs = pcmap.pcs_for_line(bp.file_id, bp.line)
                if pcs:
                    self.breakpoints.resolve_pending(bp.file_id, bp.line, [min(pcs)])

    def resolve_line(self, file_id: int, line: int) -> tuple[int, list[int]]:
        """Snap a source line to the nearest line with code and return its pcs.

        Searches every artifact, because a `break Foo.sol:12` should work before the
        contract in question has been deployed.
        """
        maps = []
        for art in self.project.artifacts.values():
            if not art.deployed_bytecode or not art.deployed_source_map:
                continue
            maps.append(
                PcMap(art.deployed_bytecode, art.deployed_source_map, self.line_indexes)
            )

        # Pick the snapped line FIRST, across every artifact, then collect pcs only for
        # that line. Doing it per-artifact would mix pcs from different lines: a file's
        # second contract snaps line 48 forward to its own first executable line, and
        # those pcs would silently join the breakpoint.
        candidates = [
            snapped
            for snapped in (
                pcmap.nearest_executable_line(file_id, line) for pcmap in maps
            )
            if snapped is not None
        ]
        if not candidates:
            return line, []
        best_line = min(candidates)
        pcs: list[int] = []
        for pcmap in maps:
            found = pcmap.pcs_for_line(file_id, best_line)
            if found:
                pcs.append(min(found))
        return best_line, sorted(set(pcs))

    # ==================================================================
    # breakpoint helpers (controller thread)
    # ==================================================================

    def file_id_for(self, source_key: str) -> int | None:
        """Accepts 'Bank.sol' or a suffix of the path, as gdb accepts a basename."""
        src = self.project.sources.get(source_key)
        if src is not None:
            return src.file_id
        for key, candidate in self.project.sources.items():
            if key.endswith(source_key) or candidate.abs_path.endswith(source_key):
                return candidate.file_id
        return None

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
    # the patch
    # ==================================================================

    def _make_patch(self) -> Any:
        session = self

        @classmethod  # type: ignore[misc]
        def patched(cls, state, message, transaction_context, parent_computation=None):
            # Preamble copied verbatim from BaseComputation.apply_computation so that
            # create-message accounting and precompile dispatch stay bit-identical.
            with cls(state, message, transaction_context) as computation:
                if computation.is_origin_computation:
                    computation.contracts_created = []
                    if message.is_create:
                        cls.consume_initcode_gas_cost(computation)
                    if session.foundry_mode:
                        session._ensure_cheat_code(computation.state)
                if parent_computation is not None:
                    computation.contracts_created = parent_computation.contracts_created
                if message.is_create:
                    computation.contracts_created.append(message.storage_address)

                precompile = computation.precompiles.get(message.code_address, NO_RESULT)
                if precompile is not NO_RESULT:
                    if not message.is_delegation:
                        precompile(computation)
                    return computation

                # Foundry cheatcodes: a call to the magic VM address is interpreted here
                # instead of executing (empty) code. console.log is the same idea.
                if message.code_address == VM_ADDRESS:
                    session._run_cheatcode(computation, message)
                    return computation
                if message.code_address == CONSOLE_ADDRESS:
                    session._run_console(computation, message)
                    return computation

                tracing = session.armed and not getattr(
                    session._local, "suspended", False
                )
                frame = session._enter_frame(computation, message) if tracing else None

                opcode_lookup = computation.opcodes
                try:
                    for opcode in computation.code:
                        try:
                            opcode_fn = opcode_lookup[opcode]
                        except KeyError:
                            opcode_fn = InvalidOpcode(opcode)

                        if frame is not None and session.armed:
                            pc = max(0, computation.code.program_counter - 1)
                            try:
                                mnemonic = opcode_fn.mnemonic
                            except AttributeError:
                                mnemonic = opcode_fn.__wrapped__.mnemonic  # type: ignore[attr-defined]
                            session._on_opcode(frame, computation, pc, opcode, mnemonic)
                            gas_before = computation._gas_meter.gas_remaining
                            try:
                                session._exec_opcode(opcode, opcode_fn, computation)
                            except Halt:
                                session._account_gas(
                                    frame, computation, pc, mnemonic, gas_before
                                )
                                break
                            except VMError as error:
                                # Pause on the failing instruction with the stack, memory
                                # and gas still intact, then let it propagate normally.
                                session._on_vm_error(
                                    frame, computation, pc, mnemonic, error
                                )
                                raise
                            session._account_gas(
                                frame, computation, pc, mnemonic, gas_before
                            )
                            session._after_opcode(frame, computation, pc, mnemonic)
                        else:
                            try:
                                session._exec_opcode(opcode, opcode_fn, computation)
                            except Halt:
                                break
                finally:
                    if frame is not None:
                        session._exit_frame(frame)
            if computation.is_origin_computation and computation.is_error:
                session._record_transaction_revert(computation)
            return computation

        return patched

    # ==================================================================
    # cheatcodes (VM thread)
    # ==================================================================

    def _record_transaction_revert(self, computation: Any) -> None:
        """Keep the top-level revert reason: a receipt only carries `status = 0`."""
        from .decode import decode_revert

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

    def _exec_opcode(self, opcode: int, opcode_fn: Any, computation: Any) -> None:
        """Run one opcode, applying an active prank to a call it makes.

        A prank must take effect at the calling opcode, not when the child frame starts:
        the EVM sources the call's value and gas, and the child's msg.sender, from the
        caller's `storage_address`. Temporarily setting that to the pranked address for the
        duration of the call makes value, gas and msg.sender all follow the prank, exactly
        as forge does. DELEGATECALL is excluded: it preserves the original caller.
        """
        prank = self.cheats.prank
        if (
            prank is None
            or opcode not in _PRANK_CALL_OPCODES
            or (
                prank.caller is not None
                and bytes(computation.msg.storage_address) != prank.caller
            )
        ):
            opcode_fn(computation=computation)
            return

        saved = computation.msg.storage_address
        computation.msg.storage_address = prank.new_sender
        try:
            opcode_fn(computation=computation)
        finally:
            computation.msg.storage_address = saved
            if not prank.persistent:
                self.cheats.prank = None

    def _op_cheat(
        self,
        frame: EvmFrame,
        computation: Any,
        calldata: bytes,
        caller: bytes | None = None,
    ) -> bytes:
        """Interactive `vm.*` cheat entered at the prompt: apply it against the frame we are
        stopped in. Pranks target the current frame's address unless a caller is given."""
        return apply_cheat(
            self.cheats,
            computation.state,
            bytes(calldata),
            caller if caller is not None else frame.address,
        )

    # ==================================================================
    # frame bookkeeping (VM thread)
    # ==================================================================

    def _enter_frame(self, computation: Any, message: Any) -> EvmFrame:
        code = bytes(message.code)
        artifact = self._artifact_for(code, message.is_create)
        pc_map = self._pcmap_for(code, artifact, message.is_create)
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
            disassembly=self._disasm_for(code),
            decl_pcs=self._declpcs_for(code, pc_map, message.is_create),
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
        hits, reason = self._should_stop(frame, computation, pc, opcode, mnemonic, loc)
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
    ) -> None:
        if not self.stop_on_revert or not self.armed:
            return
        detail = (
            f"{type(error).__name__}: {error}" if str(error) else type(error).__name__
        )
        if mnemonic == "REVERT":
            from .decode import decode_revert

            abi = frame.artifact.abi if frame.artifact else None
            detail = decode_revert(bytes(computation.output or b""), abi)
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
            self._check_watchpoints(frame, computation, pc, mnemonic)

    # -- stop policy --------------------------------------------------------

    def _should_stop(
        self,
        frame: EvmFrame,
        computation: Any,
        pc: int,
        opcode: int,
        mnemonic: str,
        loc: Location | None,
    ) -> tuple[list[int], str | None]:
        # Breakpoints fire regardless of step mode, as in gdb.
        hits: list[int] = []
        if not self.breakpoints.is_empty:
            contract = frame.artifact_name
            file_id = loc.file_id if loc else -1
            line = loc.line if loc else 0
            for bp in self.breakpoints.match(pc, mnemonic, file_id, line, contract):
                if bp.condition and not self._condition_holds(bp, frame, computation):
                    continue
                bp.hit_count += 1
                if bp.ignore_count > 0:
                    bp.ignore_count -= 1
                    continue
                hits.append(bp.number)
                if bp.temporary:
                    self.breakpoints.remove(bp.number)
        if hits:
            return hits, "breakpoint"

        read_hit = self._read_watch_hit(frame, computation, mnemonic)
        if read_hit is not None:
            return [read_hit], "watchpoint"
        self._pending_annotation = ""

        mode = self._mode
        if mode is StepMode.RUN:
            return [], None

        depth = frame.depth
        internal = frame.internal_depth

        if mode is StepMode.STEPI:
            return [], self._consume_count("step")
        if mode is StepMode.NEXTI:
            if depth <= self._mode_depth:
                return [], self._consume_count("step")
            return [], None
        if mode is StepMode.UNTIL:
            if pc == self._mode_target_pc and depth <= self._mode_depth:
                return [], "until"
            return [], None
        if mode is StepMode.FINISH:
            if depth < self._mode_depth:
                return [], "finish"
            if depth == self._mode_depth and internal < self._mode_internal:
                return [], "finish"
            return [], None

        # STEP / NEXT: source-line granularity.
        if loc is None or loc.is_generated or loc.line <= 0:
            return [], None
        if self._is_dispatcher_location(loc):
            return [], None

        if mode is StepMode.NEXT:
            if depth > self._mode_depth:
                return [], None
            if depth == self._mode_depth and internal > self._mode_internal:  # noqa: SIM102 - the nested `if` keeps the solc self-jump exemption below readable
                # Deeper in the internal (JUMP-based) call stack, so ordinarily this is a
                # call we are stepping over. The exception is the compiler's own jump from
                # a function's declaration into its body, which solc also marks 'i'. If we
                # treated that as a nested call, `next` at a function's opening line would
                # skip the entire function.
                if not self._entering_own_body(loc, internal):
                    return [], None

        if depth != self._mode_depth or loc.key() != self._mode_key:
            return [], self._consume_count("step", loc)
        return [], None

    def _entering_own_body(self, loc: Location, internal: int) -> bool:
        """True when the only thing that got deeper is the jump into our own body.

        Requires that the previous stop sat exactly on a function's declaration range, so
        a recursive call (whose call site is a statement inside the body, not the
        declaration) is still correctly stepped over.
        """
        if not self._mode_at_function_entry or internal != self._mode_internal + 1:
            return False
        current = self.functions.at_location(loc)
        return current is not None and current == self._mode_function

    def _consume_count(self, reason: str, loc: Location | None = None) -> str | None:
        """Support `next 5`: only actually stop on the last repetition."""
        self._pending_count -= 1
        if self._pending_count > 0:
            frame = self.current_frame
            if frame is not None:
                self._set_baseline(frame, loc)
            return None
        return reason

    def _set_baseline(self, frame: EvmFrame, loc: Location | None) -> None:
        """Record where a step started, which is what every stop test compares against."""
        self._mode_depth = frame.depth
        self._mode_internal = frame.internal_depth
        mapped = loc is not None and not loc.is_generated and loc.line > 0
        self._mode_key = loc.key() if mapped else None
        fn = self.functions.at_location(loc) if mapped else None
        self._mode_function = fn
        self._mode_at_function_entry = bool(
            fn is not None
            and loc is not None
            and loc.entry.start == fn.start
            and loc.entry.length == fn.length
        )

    def _is_dispatcher_location(self, loc: Location) -> bool:
        """Suppress the selector dispatcher, which maps to the whole contract range.

        Without this, every `step` into a contract stops first on `contract Foo {`,
        which teaches the user nothing and costs them a keystroke.
        """
        for start, end, _name in self.functions.contracts.get(loc.file_id, []):
            if loc.entry.start == start and loc.entry.start + loc.entry.length == end:
                return True
        return False

    def _condition_holds(self, bp: Any, frame: EvmFrame, computation: Any) -> bool:
        """Evaluate a breakpoint condition.

        A condition that cannot be evaluated breaks anyway, as gdb does, but the reason
        is recorded on the breakpoint so the UI can say so. Silently treating a broken
        condition as "always true" would leave the user guessing why they stopped.
        """
        if self._eval_hook is None:
            return True
        try:
            result = self._eval_hook(
                self,
                frame,
                computation,
                bp.condition,
                want_bool=True,
                bindings=self.frame_locals(frame, computation),
            )
        except Exception as exc:
            bp.condition_error = str(exc)
            return True
        bp.condition_error = None
        return bool(result)

    # -- watchpoints --------------------------------------------------------

    def _read_watch_hit(
        self, frame: EvmFrame, computation: Any, mnemonic: str
    ) -> int | None:
        """Read watchpoints fire on the SLOAD itself, before the value is fetched.

        Write watchpoints cannot work that way: a write is only observable by comparing
        the slot before and after, which is why the two live in different places.
        """
        if mnemonic != "SLOAD" or not self.breakpoints.has_watchpoints:
            return None
        values = computation._stack.values
        if not values:
            return None
        slot = _as_int(values[-1])
        for wp in self.breakpoints.active_watchpoints():
            if wp.kind != "storage" or wp.mode not in (WATCH_READ, WATCH_ACCESS):
                continue
            if wp.slot == slot and (wp.address is None or wp.address == frame.address):
                wp.hit_count += 1
                current = computation.state.get_storage(frame.address, slot)
                self._pending_annotation = f"{wp.expression}: read 0x{current:x}"
                return wp.number
        return None

    def _check_watchpoints(
        self, frame: EvmFrame, computation: Any, pc: int, mnemonic: str
    ) -> None:
        triggered: list[Watchpoint] = []
        for wp in self.breakpoints.active_watchpoints():
            if wp.mode == WATCH_READ:
                continue  # handled before the SLOAD, in _read_watch_hit
            try:
                current = self._read_watch_value(wp, frame, computation)
            except Exception:
                continue
            if not wp.initialised:
                wp.old_value = current
                wp.initialised = True
                continue
            if current != wp.old_value:
                wp.hit_count += 1
                wp.old_value_previous = wp.old_value  # type: ignore[attr-defined]
                wp.old_value = current
                triggered.append(wp)
        if triggered:
            wp = triggered[0]
            old = getattr(wp, "old_value_previous", None)
            note = f"{wp.expression}: {_fmt_value(old)} -> {_fmt_value(wp.old_value)}"
            new_pc = computation.code.program_counter
            self._pause(
                frame,
                computation,
                new_pc,
                0,
                mnemonic,
                frame.location(new_pc),
                "watchpoint",
                [wp.number],
                annotation=note,
            )

    def _read_watch_value(
        self, wp: Watchpoint, frame: EvmFrame, computation: Any
    ) -> int | None:
        if wp.kind == "storage":
            address = wp.address or frame.address
            if wp.slot is None:
                return None
            return computation.state.get_storage(address, wp.slot)
        if wp.kind == "memory" and wp.offset is not None:
            data = bytes(computation._memory.read_bytes(wp.offset, wp.size))
            return int.from_bytes(data, "big")
        return None

    # -- pausing ------------------------------------------------------------

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
        snapshot = self._build_snapshot(
            frame, computation, pc, opcode, mnemonic, loc, reason, hits, annotation
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
                self._set_baseline(frame, loc)
                self._mode_target_pc = cmd.target_pc
                return
            if isinstance(cmd, Inspect):
                try:
                    result = self._handle_inspect(cmd, frame, computation)
                except Exception as exc:
                    result = Failure(f"{type(exc).__name__}: {exc}")
                self._reply_q.put(result)
                continue
            self._reply_q.put(Failure(f"unknown command: {cmd!r}"))

    def _live_view(self, frame: EvmFrame, computation: Any) -> dict[str, Any]:
        """The parts of a snapshot that a mutation can change under the UI's feet.

        Shared by `_build_snapshot` and the `resnapshot` inspect op, so a refresh after a
        write produces the same fields the pause did rather than a second, drifting copy.
        """
        raw_stack = list(computation._stack.values)
        meter = computation._gas_meter
        return {
            "stack": tuple(
                StackEntry(index=i, value=_as_int(v), raw=v)
                for i, v in enumerate(reversed(raw_stack))
            ),
            "memory": bytes(computation._memory._bytes[:SNAPSHOT_MEMORY_LIMIT]),
            "memory_size": len(computation._memory),
            "gas_remaining": meter.gas_remaining,
            "gas_used": meter.start_gas - meter.gas_remaining,
            "gas_refund": meter.gas_refunded,
            "locals": tuple(self.frame_locals(frame, computation)),
        }

    def _build_snapshot(
        self,
        frame: EvmFrame,
        computation: Any,
        pc: int,
        opcode: int,
        mnemonic: str,
        loc: Location | None,
        reason: str,
        hits: Sequence[int],
        annotation: str,
    ) -> FrameSnapshot:
        live = self._live_view(frame, computation)
        meter = computation._gas_meter

        source_key = None
        if loc is not None and not loc.is_generated:
            src = self.project.source_by_id(loc.file_id)
            source_key = src.key if src else None

        static_gas = None
        opcode_obj = computation.opcodes.get(opcode)
        if opcode_obj is not None:
            static_gas = getattr(opcode_obj, "gas_cost", None)

        return FrameSnapshot(
            step=self.step_index,
            pc=pc,
            opcode=opcode,
            mnemonic=mnemonic,
            depth=frame.depth,
            gas_limit=meter.start_gas,
            address=frame.address,
            code_address=frame.code_address,
            sender=frame.sender,
            origin=bytes(computation.transaction_context.origin),
            value=frame.value,
            calldata=frame.calldata,
            is_static=frame.is_static,
            contract_name=frame.artifact_name,
            source_key=source_key,
            file_id=loc.file_id if loc else -1,
            line=loc.line if loc and not loc.is_generated else 0,
            col=loc.col if loc and not loc.is_generated else 0,
            end_line=loc.end_line if loc and not loc.is_generated else 0,
            jump=loc.jump if loc else "-",
            function=self.functions.at_location(loc),
            backtrace=tuple(self._backtrace_rows()),
            stop_reason=reason,
            **live,
            hit_breakpoints=tuple(hits),
            static_gas=static_gas,
            annotation=annotation,
        )

    def _backtrace_rows(self) -> list[BacktraceRow]:
        """Interleaved EVM and internal frames, innermost first, gdb ordering.

        Each Solidity frame is shown at the line it is *currently executing*, which for
        an outer frame is the call site of the frame it called. Compiler-generated
        helper frames (solc's ABI encode/decode routines) are collapsed unless execution
        is actually inside one, since a backtrace full of `<compiler-generated>` hides
        the program the user wrote.
        """
        rows: list[BacktraceRow] = []
        index = 0
        for evm_index in range(len(self._frames) - 1, -1, -1):
            frame = self._frames[evm_index]
            src = self.project.sources.get(
                frame.artifact.source_key if frame.artifact else ""
            )
            source_key = src.key if src else None
            pc_here = max(0, frame.computation.code.program_counter - 1)
            internals = frame.internal
            for k in range(len(internals) - 1, -1, -1):
                innermost = k == len(internals) - 1
                if internals[k].is_generated and not innermost:
                    continue
                show_pc = pc_here if innermost else internals[k + 1].call_site_pc
                loc = frame.location(show_pc)
                rows.append(
                    BacktraceRow(
                        index=index,
                        name=internals[k].name,
                        line=loc.line if loc and not loc.is_generated else 0,
                        pc=show_pc,
                        kind="solidity",
                        detail=""
                        if not internals[k].is_generated
                        else "compiler-generated",
                        address=frame.address,
                        evm_index=evm_index,
                        internal_index=k,
                        source_key=source_key,
                    )
                )
                index += 1

            # The EVM frame boundary itself. When internal frames are present the
            # outermost of them already named the function, so this row is the call.
            loc = frame.location(pc_here)
            if internals:
                name = frame.artifact_name or "0x" + frame.address.hex()[:8]
                line = 0
                pc_show = internals[0].call_site_pc
            else:
                fn = self.functions.at_location(loc)
                name = (
                    fn.signature
                    if fn
                    else (frame.artifact_name or "0x" + frame.address.hex()[:8])
                )
                line = loc.line if loc and not loc.is_generated else 0
                pc_show = pc_here
            rows.append(
                BacktraceRow(
                    index=index,
                    name=name,
                    line=line,
                    pc=pc_show,
                    kind="evm",
                    detail=f"{frame.kind} depth={frame.depth}",
                    address=frame.address,
                    evm_index=evm_index,
                    source_key=source_key,
                )
            )
            index += 1
        return rows

    # ==================================================================
    # local variables (VM thread)
    # ==================================================================

    def frame_locals(
        self, frame: EvmFrame, computation: Any, internal_index: int | None = None
    ) -> list[LocalValue]:
        """Every local visible in one Solidity frame, decoded.

        Three things have to line up before a value is shown, and each one is a way the
        naive version reads the wrong word:

          * the frame must have been observed from its entry, so the base is real;
          * the slot must lie below the current stack top, which is what retires a
            variable whose block has already been popped;
          * the current instruction must be inside the declaration's scope, which is
            what stops a slot recorded in an exited block from resurfacing under a
            temporary that happens to sit at the same height.

        Anything that fails reports `<unavailable>` with the reason.
        """
        internals = frame.internal
        if not internals:
            return []
        if internal_index is None or not 0 <= internal_index < len(internals):
            internal_index = len(internals) - 1
        internal = internals[internal_index]
        fn = internal.function
        if fn is None:
            return []
        layout = self.locals.for_function(fn.ast_id)
        if layout is None or not layout.all:
            return []

        innermost = internal_index == len(internals) - 1
        pc_here = max(0, computation.code.program_counter - 1)
        show_pc = pc_here if innermost else internals[internal_index + 1].call_site_pc
        loc = frame.location(show_pc)
        if loc is None or loc.is_generated:
            return []
        offset = loc.entry.start

        stack = computation._stack.values
        sp = len(stack)
        memory = computation._memory._bytes

        def read_memory(start: int, size: int) -> bytes:
            data = bytes(memory[start : start + size])
            return data + b"\x00" * (size - len(data))  # unwritten memory reads as zero

        positions = self._param_positions(layout.params, internal.entry_sp)
        for var in layout.returns + layout.body:
            recorded = internal.slots.get(var.ast_id)
            if recorded is not None:
                positions[var.ast_id] = recorded

        # A modifier's locals sit in this same frame, so anything recorded here that the
        # function does not own is a modifier's, and the scope check below decides
        # whether the user is currently standing inside that modifier's body.
        extra: list[Any] = []
        for ast_id in internal.slots:
            var = self.locals.by_ast_id(ast_id)
            if (
                var is not None
                and var.function_id != fn.ast_id
                and var.visible_at(offset)
            ):
                positions[var.ast_id] = internal.slots[ast_id]
                extra.append(var)

        out: list[LocalValue] = []
        candidates = [v for v in self.locals.visible(fn.ast_id, offset) if v.name]
        for var in candidates + extra:
            base = positions.get(var.ast_id)
            width = var.slots
            if base is None:
                out.append(_unavailable(var, "not allocated yet at this instruction"))
                continue
            if width is None:
                out.append(
                    _unavailable(var, f"unknown stack width for {var.display_type}")
                )
                continue
            if base < 0 or base + width > sp:
                # Two different situations look identical from the stack alone, and the
                # user needs to be told which one they are in.
                pending = var.start <= offset < var.end
                out.append(
                    _unavailable(
                        var,
                        "this instruction allocates it; step once to see it"
                        if pending
                        else "out of scope: its stack slot has been popped",
                    )
                )
                continue
            words = tuple(_as_int(stack[base + i]) for i in range(width))
            value = read_local(var, words, read_memory)
            value.position = base
            if (
                var.statement_start >= 0
                and var.statement_start <= offset < var.statement_end
            ):
                value.reason = value.reason or "still inside its own initialiser"
            out.append(value)
        return out

    @staticmethod
    def _param_positions(params: Sequence[Any], entry_sp: int | None) -> dict[int, int]:
        """Place parameters below the frame base, from the top down.

        Walking in reverse matters: a parameter of unknown width only invalidates the
        ones *deeper* than it, so one exotic type does not blind the whole frame.
        """
        positions: dict[int, int] = {}
        if entry_sp is None:
            return positions
        cursor = entry_sp
        for var in reversed(list(params)):
            width = var.slots
            if width is None:
                break
            cursor -= width
            positions[var.ast_id] = cursor
        return positions

    # ==================================================================
    # inspect operations (VM thread, while parked)
    # ==================================================================

    def _handle_inspect(self, cmd: Inspect, frame: EvmFrame, computation: Any) -> Any:
        handler = getattr(self, f"_op_{cmd.op}", None)
        if handler is None:
            return Failure(f"unknown inspect op: {cmd.op}")
        if cmd.frame_index is not None:
            if not 0 <= cmd.frame_index < len(self._frames):
                return Failure(f"no such frame: {cmd.frame_index}")
            frame = self._frames[cmd.frame_index]
            computation = frame.computation
        return handler(frame, computation, *cmd.args, **cmd.kwargs)

    # -- reads --------------------------------------------------------------

    def _op_read_memory(
        self, frame: EvmFrame, computation: Any, offset: int, size: int
    ) -> bytes:
        """Reads past the current memory size return zeros, as the EVM itself does."""
        buf = computation._memory._bytes
        data = bytes(buf[offset : offset + size])
        if len(data) < size:
            data += b"\x00" * (size - len(data))
        return data

    def _op_read_storage(
        self, frame: EvmFrame, computation: Any, slot: int, address: bytes | None = None
    ) -> int:
        return computation.state.get_storage(address or frame.address, slot)

    def _op_read_transient(
        self, frame: EvmFrame, computation: Any, slot: int, address: bytes | None = None
    ) -> bytes:
        return computation.state.get_transient_storage(address or frame.address, slot)

    def _op_read_balance(self, frame: EvmFrame, computation: Any, address: bytes) -> int:
        return computation.state.get_balance(address)

    def _op_read_code(self, frame: EvmFrame, computation: Any, address: bytes) -> bytes:
        return computation.state.get_code(address)

    def _op_read_nonce(self, frame: EvmFrame, computation: Any, address: bytes) -> int:
        return computation.state.get_nonce(address)

    def _op_is_warm(
        self, frame: EvmFrame, computation: Any, slot: int, address: bytes | None = None
    ) -> bool:
        return computation.state.is_storage_warm(address or frame.address, slot)

    def _op_logs(
        self, frame: EvmFrame, computation: Any
    ) -> list[tuple[bytes, tuple[int, ...], bytes]]:
        return [
            (bytes(addr), tuple(topics), bytes(data))
            for _counter, addr, topics, data in computation._log_entries
        ]

    def _op_disassembly(
        self, frame: EvmFrame, computation: Any, before: int = 6, after: int = 18
    ) -> list[dict]:
        pc = max(0, computation.code.program_counter - 1)
        rows = []
        for ins in frame.disassembly.window(pc, before, after):
            loc = frame.location(ins.pc)
            rows.append(
                {
                    "pc": ins.pc,
                    "text": ins.render(),
                    "current": ins.pc == pc,
                    "line": loc.line if loc and not loc.is_generated else 0,
                    "jumpdest": ins.pc in frame.disassembly.jumpdests,
                }
            )
        return rows

    def _op_locals(
        self, frame: EvmFrame, computation: Any, internal_index: int | None = None
    ) -> list[dict]:
        return [
            {
                "name": v.name,
                "type": v.type_label,
                "value": v.display,
                "available": v.available,
                "reason": v.reason,
                "kind": v.kind,
                "position": v.position,
                "writable": v.writable,
            }
            for v in self.frame_locals(frame, computation, internal_index)
        ]

    def _op_resnapshot(self, frame: EvmFrame, computation: Any) -> dict:
        """The live stack/memory/gas/locals, for `refresh_snapshot`."""
        return self._live_view(frame, computation)

    def _op_frame_info(self, frame: EvmFrame, computation: Any) -> dict:
        return {
            "depth": frame.depth,
            "kind": frame.kind,
            "address": frame.address,
            "code_address": frame.code_address,
            "sender": frame.sender,
            "value": frame.value,
            "calldata": frame.calldata,
            "is_static": frame.is_static,
            "artifact": frame.artifact_name,
            "internal": [i.name for i in frame.internal],
            "gas_remaining": computation._gas_meter.gas_remaining,
        }

    # -- mutations ----------------------------------------------------------

    def _op_write_storage(
        self,
        frame: EvmFrame,
        computation: Any,
        slot: int,
        value: int,
        address: bytes | None = None,
    ) -> int:
        computation.state.set_storage(address or frame.address, slot, value)
        return computation.state.get_storage(address or frame.address, slot)

    def _op_write_local(
        self,
        frame: EvmFrame,
        computation: Any,
        name: str,
        value: int,
        internal_index: int | None = None,
    ) -> dict:
        """Write a local by name, straight into its stack slot.

        This cannot go through the evaluator: a local is passed into the injected
        function as a parameter, so assigning to it would modify a copy that the
        snapshot revert then discards, and the debugger would report a change that never
        happened.
        """
        for local in self.frame_locals(frame, computation, internal_index):
            if local.name != name:
                continue
            if not local.available or local.position is None:
                raise ValueError(
                    f"`{name}` is not writable here: {local.reason or 'unavailable'}"
                )
            if not local.writable:
                raise ValueError(
                    f"`{name}` is a {local.type_label}; its stack slot is a reference, not "
                    "the value. Writing it would corrupt the pointer, so it is refused"
                )
            values = computation._stack.values
            existing = values[local.position]
            # Py-EVM keeps stack items as int OR bytes and the opcodes care which.
            values[local.position] = (
                value.to_bytes(32, "big") if isinstance(existing, bytes) else value
            )
            updated = [
                v
                for v in self.frame_locals(frame, computation, internal_index)
                if v.name == name
            ]
            return {
                "name": name,
                "display": updated[0].display if updated else str(value),
            }
        raise ValueError(f"no local named `{name}` in scope here")

    def _op_write_stack(
        self, frame: EvmFrame, computation: Any, index: int, value: int
    ) -> int:
        """index 0 is the top of the stack, matching what the UI displays."""
        values = computation._stack.values
        if not 0 <= index < len(values):
            raise IndexError(f"stack index {index} out of range (depth {len(values)})")
        values[len(values) - 1 - index] = value
        return value

    def _op_write_memory(
        self, frame: EvmFrame, computation: Any, offset: int, data: bytes
    ) -> int:
        buf = computation._memory
        if offset + len(data) > len(buf):
            buf.extend(offset, len(data))
        buf._bytes[offset : offset + len(data)] = data
        return len(data)

    def _op_write_balance(
        self, frame: EvmFrame, computation: Any, address: bytes, value: int
    ) -> int:
        computation.state.set_balance(address, value)
        return computation.state.get_balance(address)

    def _op_set_gas(self, frame: EvmFrame, computation: Any, value: int) -> int:
        computation._gas_meter.gas_remaining = value
        return value

    def _op_set_pc(self, frame: EvmFrame, computation: Any, value: int) -> int:
        if not frame.disassembly.is_valid_jumpdest(value):
            raise ValueError(f"0x{value:x} is not a JUMPDEST; refusing to jump")
        computation.code.program_counter = value
        return value

    def _op_assembly(self, frame: EvmFrame, computation: Any, source: str) -> Any:
        """Run Yul against the live frame. See `assembly.run` for the semantics.

        An `AsmError` is already a sentence aimed at the user, so it is returned as a
        bare Failure rather than left to pick up an `AsmError:` prefix on the way out.
        """
        try:
            return assembly.run(self, computation, source)
        except assembly.AsmError as exc:
            return Failure(str(exc))

    # -- speculative execution ---------------------------------------------

    def _op_evaluate(
        self,
        frame: EvmFrame,
        computation: Any,
        expression: str,
        keep: bool = False,
        internal_index: int | None = None,
    ) -> Any:
        if self._eval_hook is None:
            raise SessionError("no evaluator installed")
        return self._eval_hook(
            self,
            frame,
            computation,
            expression,
            keep=keep,
            bindings=self.frame_locals(frame, computation, internal_index),
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


def _unavailable(var: Any, reason: str) -> LocalValue:
    return LocalValue(
        name=var.name or f"<{var.kind}>",
        type_label=var.display_type,
        display="<unavailable>",
        available=False,
        reason=reason,
        kind=var.kind,
    )


def _as_int(value: Any) -> int:
    """Py-EVM stack items are int OR bytes depending on how they were pushed."""
    if isinstance(value, int):
        return value
    return int.from_bytes(value, "big")


def _fmt_value(value: int | None) -> str:
    if value is None:
        return "<unset>"
    return f"0x{value:x}"
