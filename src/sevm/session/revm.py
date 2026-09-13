"""Source-level debugger session backed by the in-process REVM bridge."""

from __future__ import annotations

import queue
import threading
import traceback
from contextlib import contextmanager
from typing import Any, cast

from eth_abi import encode as abi_encode

from .._revm import RevmChain
from ..breakpoints import BP_OPCODE, BP_PC, WATCH_WRITE, BreakpointSet
from ..cheatcodes import (
    CONSOLE_ADDRESS,
    VM_ADDRESS,
    CheatError,
    CheatState,
    apply_cheat,
    cheat_name,
    decode_console_log,
)
from ..compile import Project
from ..decode import decode_revert
from ..frames import (
    EvmFrame,
    FrameSnapshot,
    FunctionIndex,
    InternalFrame,
    StackEntry,
)
from ..locals import LocalsIndex
from ..srcmap import build_line_indexes
from . import framelocals, snapshots
from .code import CodeIndex
from .events import Finished, Paused, Resume, SessionError, StepMode

_ERROR_SELECTOR = bytes.fromhex("08c379a0")
_ANY_ADDRESS = "0x" + "00" * 20
_JUMP_MNEMONICS = frozenset({"JUMP", "JUMPI"})


class _StackView:
    def __init__(self, top_first: list[str]) -> None:
        self.values = [int(value, 16) for value in reversed(top_first)]


class _MemoryView:
    def __init__(self, data: bytes) -> None:
        self._bytes = bytearray(data)

    def __len__(self) -> int:
        return len(self._bytes)


class _GasView:
    def __init__(self, limit: int, remaining: int, refunded: int) -> None:
        self.start_gas = limit
        self.gas_remaining = remaining
        self.gas_refunded = refunded


class _CodeView:
    def __init__(self, pc: int) -> None:
        self.program_counter = pc + 1


class _TransactionView:
    def __init__(self, origin: bytes) -> None:
        self.origin = origin


class _ComputationView:
    def __init__(
        self,
        *,
        pc: int,
        stack: list[str],
        memory: bytes,
        gas_limit: int,
        gas_remaining: int,
        gas_refund: int,
        origin: bytes,
    ) -> None:
        self.code = _CodeView(pc)
        self._stack = _StackView(stack)
        self._memory = _MemoryView(memory)
        self._gas_meter = _GasView(gas_limit, gas_remaining, gas_refund)
        self.transaction_context = _TransactionView(origin)
        self.opcodes: dict[int, Any] = {}

    def get_gas_remaining(self) -> int:
        return self._gas_meter.gas_remaining


class _RevmExecutionContext:
    def __init__(self, session: RevmDebugSession) -> None:
        self._session = session

    @property
    def block_number(self) -> int:
        return int(self._session._chain.read_block_number(), 16)

    @property
    def _block_number(self) -> int:
        return self.block_number

    @_block_number.setter
    def _block_number(self, value: int) -> None:
        self._session._chain.write_block_number(value)

    @property
    def timestamp(self) -> int:
        return int(self._session._chain.read_timestamp(), 16)

    @property
    def _timestamp(self) -> int:
        return self.timestamp

    @_timestamp.setter
    def _timestamp(self, value: int) -> None:
        self._session._chain.write_timestamp(value)

    @property
    def base_fee_per_gas(self) -> int:
        return int(self._session._chain.read_base_fee())

    @property
    def _base_fee_per_gas(self) -> int:
        return self.base_fee_per_gas

    @_base_fee_per_gas.setter
    def _base_fee_per_gas(self, value: int) -> None:
        self._session._chain.write_base_fee(value)

    @property
    def chain_id(self) -> int:
        return int(self._session._chain.read_chain_id())

    @property
    def _chain_id(self) -> int:
        return self.chain_id

    @_chain_id.setter
    def _chain_id(self, value: int) -> None:
        self._session._chain.write_chain_id(value)

    @property
    def coinbase(self) -> bytes:
        return _address_bytes(self._session._chain.read_coinbase())

    @property
    def _coinbase(self) -> bytes:
        return self.coinbase

    @_coinbase.setter
    def _coinbase(self, value: bytes) -> None:
        self._session._chain.write_coinbase(_address_hex(value))

    @property
    def mix_hash(self) -> bytes:
        return bytes(self._session._chain.read_prevrandao())

    @property
    def _mix_hash(self) -> bytes:
        return self.mix_hash

    @_mix_hash.setter
    def _mix_hash(self, value: bytes) -> None:
        self._session._chain.write_prevrandao(value)

    @property
    def difficulty(self) -> int:
        return int(self._session._chain.read_difficulty(), 16)

    @property
    def _difficulty(self) -> int:
        return self.difficulty

    @_difficulty.setter
    def _difficulty(self, value: int) -> None:
        self._session._chain.write_difficulty(value)


class _RevmState:
    def __init__(self, session: RevmDebugSession) -> None:
        self._session = session
        self.execution_context = _RevmExecutionContext(session)

    def get_balance(self, address: bytes) -> int:
        return int(self._session._chain.read_balance(_address_hex(address)), 16)

    def set_balance(self, address: bytes, value: int) -> None:
        self._session._chain.write_balance(_address_hex(address), value)

    def get_code(self, address: bytes) -> bytes:
        return bytes(self._session._chain.read_code_at(_address_hex(address)))

    def set_code(self, address: bytes, code: bytes) -> None:
        self._session._chain.write_code_at(_address_hex(address), code)

    def get_storage(self, address: bytes, slot: int) -> int:
        return int(self._session._chain.read_storage_at(_address_hex(address), slot), 16)

    def set_storage(self, address: bytes, slot: int, value: int) -> None:
        self._session._chain.write_storage_at(_address_hex(address), slot, value)

    def get_transient_storage(self, address: bytes, slot: int) -> bytes:
        value = int(self._session._chain.read_transient(_address_hex(address), slot), 16)
        return value.to_bytes(32, "big")

    def set_transient_storage(self, address: bytes, slot: int, value: int) -> None:
        self._session._chain.write_transient(_address_hex(address), slot, value)

    def get_nonce(self, address: bytes) -> int:
        return int(self._session._chain.read_nonce(_address_hex(address)))

    def set_nonce(self, address: bytes, value: int) -> None:
        self._session._chain.write_nonce(_address_hex(address), value)

    def mark_storage_warm(self, address: bytes, slot: int) -> None:
        self._session._chain.warm_storage(_address_hex(address), slot)


class RevmDebugSession:
    """Drive REVM through the session interface shared by the console and TUI."""

    def __init__(
        self,
        project: Project,
        breakpoints: BreakpointSet | None = None,
        stop_at_start: bool = True,
        skip_to_source: bool = True,
    ) -> None:
        self.project = project
        self.breakpoints = breakpoints or BreakpointSet()
        self.cheats = CheatState()
        self.functions = FunctionIndex(project.asts)
        self.locals = LocalsIndex(project.asts)
        self.line_indexes = build_line_indexes(project.sources.values())
        self.code = CodeIndex(project, self.line_indexes, self.locals, self.breakpoints)
        self.stop_at_start = stop_at_start
        self.skip_to_source = skip_to_source
        self.stop_on_revert = True
        self.foundry_mode = True

        self._chain = RevmChain()
        self._state = _RevmState(self)
        self._deployed: dict[str, Any] = {}
        self._frames: list[EvmFrame] = []
        self._last_raw: dict[str, Any] | None = None
        self._event_q: queue.Queue[Any] = queue.Queue()
        self._cmd_q: queue.Queue[Resume] = queue.Queue()
        self._thread: threading.Thread | None = None
        self._eval_hook: Any = None
        self._mode = StepMode.RUN
        self._mode_depth = 0
        self._mode_internal = 0
        self._mode_key: tuple[int, int] | None = None
        self._mode_target_pc: int | None = None
        self._pending_count = 1
        self._detached = False

        self.armed = False
        self.finished = False
        self.step_index = 0
        self.estimations = 0
        self.gas_by_line: dict[tuple[int, int], int] = {}
        self.gas_by_opcode: dict[str, int] = {}
        self.last_snapshot: FrameSnapshot | None = None
        self.exit_error: str | None = None
        self.last_revert: str | None = None

    def set_eval_hook(self, hook: Any) -> None:
        self._eval_hook = hook

    def start(self, target: Any) -> None:
        if self._thread is not None:
            raise SessionError("session already started")
        if not hasattr(target, "run_revm"):
            raise SessionError("the REVM session requires a REVM-compatible driver")
        self.armed = True

        def runner() -> None:
            try:
                target.run_revm(self)
                self._event_q.put(Finished(ok=True))
            except BaseException as exc:
                self.exit_error = f"{type(exc).__name__}: {exc}"
                self._event_q.put(
                    Finished(
                        ok=False,
                        error=self.exit_error,
                        traceback=traceback.format_exc(),
                    )
                )
            finally:
                self.finished = True
                self.armed = False

        self._thread = threading.Thread(target=runner, name="sevm-revm", daemon=True)
        self._thread.start()

    def wait(self, timeout: float | None = None) -> Any:
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
        if self.finished:
            return Finished(ok=True)
        self._sync_breakpoints()
        self._cmd_q.put(Resume(mode=mode, count=max(1, count), target_pc=target_pc))
        return self.wait(timeout=timeout)

    def detach(self, timeout: float = 10.0) -> None:
        if self._thread is not None and not self.finished:
            self._cmd_q.put(Resume(mode=StepMode.RUN, detach=True))
            while True:
                event = self.wait(timeout=timeout)
                if event is None or isinstance(event, Finished):
                    break
                self._cmd_q.put(Resume(mode=StepMode.RUN, detach=True))
        if self._thread is not None:
            self._thread.join(timeout=timeout)

    def reset_chain(self) -> None:
        self._chain = RevmChain()
        self._deployed.clear()
        self._frames.clear()
        self._last_raw = None

    def deploy(self, artifact: Any, what: str) -> str:
        self._chain.create(artifact.bytecode, gas_limit=30_000_000)
        result = self._drive_transaction()
        self._require_success(result, what, artifact)
        address = result.get("created_address")
        if not address:
            raise SessionError(f"{what} returned no contract address")
        self._deployed[address.lower()] = artifact
        return address

    def call(self, address: str, calldata: bytes, what: str) -> dict[str, Any]:
        self._sync_breakpoints()
        self._chain.call(address, calldata, gas_limit=30_000_000)
        result = self._drive_transaction()
        artifact = self._deployed.get(address.lower())
        self._require_success(result, what, artifact)
        return result

    def _require_success(self, result: dict[str, Any], what: str, artifact: Any) -> None:
        if result.get("type") == "failed":
            raise SessionError(result.get("error") or f"{what} failed")
        if result.get("success"):
            return
        self.last_revert = decode_revert(
            result.get("output", b""), artifact.abi if artifact else None
        )
        from ..foundry import TestFailed

        raise TestFailed(f"{what} reverted: {self.last_revert}")

    def _drive_transaction(self) -> dict[str, Any]:
        while True:
            event = self._chain.wait(timeout=30.0)
            kind = event.get("type")
            if kind == "host_call":
                self._handle_host_call(event)
                continue
            if kind in {"finished", "failed"}:
                return event
            if kind != "paused":
                raise SessionError(f"unknown REVM event: {kind!r}")
            snapshot, hits = self._snapshot_from_raw(event)
            self._last_raw = event
            if self._detached:
                self._chain.resume()
                continue
            if not self._should_surface(event, snapshot, hits):
                self._continue_raw()
                continue
            self.last_snapshot = snapshot
            self._event_q.put(Paused(snapshot))
            command = self._cmd_q.get()
            if command.detach:
                self._detached = True
                self._chain.set_breakpoints([])
                self._chain.resume()
                continue
            self._set_mode(command, snapshot)
            self._continue_raw()

    def _handle_host_call(self, event: dict[str, Any]) -> None:
        address = bytes.fromhex(event["address"][2:])
        data = bytes(event["data"])
        caller = bytes.fromhex(event["caller"][2:])
        if address == CONSOLE_ADDRESS:
            line = decode_console_log(data)
            if line is not None:
                self.cheats.console_lines.append(line)
            self._chain.respond_host()
            return
        if address != VM_ADDRESS:
            self._chain.respond_host(_error_payload("unknown host address"), revert=True)
            return
        try:
            output = apply_cheat(self.cheats, self._state, data, caller)
            if cheat_name(data) in {"prank", "startPrank", "stopPrank"}:
                self._sync_prank()
        except Exception as exc:
            reason = (
                str(exc)
                if isinstance(exc, CheatError)
                else f"{type(exc).__name__}: {exc}"
            )
            self._chain.respond_host(_error_payload(reason), revert=True)
            return
        self._chain.respond_host(output)

    def _sync_prank(self) -> None:
        prank = self.cheats.prank
        if prank is None:
            self._chain.configure_prank()
            return
        self._chain.configure_prank(
            _address_hex(prank.new_sender),
            caller=_address_hex(prank.caller) if prank.caller is not None else None,
            persistent=prank.persistent,
            new_origin=(
                _address_hex(prank.new_origin) if prank.new_origin is not None else None
            ),
            delegate=prank.delegate,
        )
        if not prank.persistent:
            self.cheats.prank = None

    def _set_mode(self, command: Resume, snapshot: FrameSnapshot) -> None:
        self._mode = command.mode
        self._mode_depth = snapshot.depth
        self._mode_internal = (
            len(self.current_frame.internal) if self.current_frame else 0
        )
        self._mode_key = (
            (snapshot.file_id, snapshot.line) if snapshot.has_source else None
        )
        self._mode_target_pc = command.target_pc
        self._pending_count = command.count

    def _continue_raw(self) -> None:
        if self._mode is StepMode.RUN:
            self._sync_breakpoints()
            self._chain.resume()
        else:
            self._chain.step()

    def _should_surface(
        self,
        raw: dict[str, Any],
        snapshot: FrameSnapshot,
        hits: tuple[int, ...],
    ) -> bool:
        reason = raw["reason"]
        if reason == "out_of_gas":
            return True
        if reason == "breakpoint":
            return bool(hits)
        if self._mode is StepMode.STEPI:
            return self._consume_count(snapshot)
        if self._mode is StepMode.NEXTI:
            return snapshot.depth <= self._mode_depth and self._consume_count(snapshot)
        if self._mode is StepMode.UNTIL:
            return (
                snapshot.pc == self._mode_target_pc and snapshot.depth <= self._mode_depth
            )
        if self._mode is StepMode.FINISH:
            internal = len(self.current_frame.internal) if self.current_frame else 0
            return snapshot.depth < self._mode_depth or (
                snapshot.depth == self._mode_depth and internal < self._mode_internal
            )
        if not snapshot.has_source or self._is_dispatcher(snapshot):
            return False
        if self._mode is StepMode.NEXT and snapshot.depth > self._mode_depth:
            return False
        if (
            snapshot.depth != self._mode_depth
            or (
                snapshot.file_id,
                snapshot.line,
            )
            != self._mode_key
        ):
            return self._consume_count(snapshot)
        return False

    def _consume_count(self, snapshot: FrameSnapshot) -> bool:
        self._pending_count -= 1
        if self._pending_count > 0:
            self._mode_depth = snapshot.depth
            self._mode_key = (
                (snapshot.file_id, snapshot.line) if snapshot.has_source else None
            )
            return False
        return True

    def _is_dispatcher(self, snapshot: FrameSnapshot) -> bool:
        if not snapshot.has_source:
            return False
        frame = self.current_frame
        loc = frame.location(snapshot.pc) if frame else None
        if loc is None:
            return False
        return any(
            loc.entry.start == start and loc.entry.start + loc.entry.length == end
            for start, end, _ in self.functions.contracts.get(loc.file_id, [])
        )

    def _snapshot_from_raw(
        self,
        raw: dict[str, Any],
        *,
        reason: str | None = None,
        hits: tuple[int, ...] | None = None,
        annotation: str = "",
    ) -> tuple[FrameSnapshot, tuple[int, ...]]:
        self._advance_internal(raw)
        self._sync_frames(raw)
        frame = self.current_frame
        if frame is None:
            raise SessionError("REVM paused without a frame")
        loc = frame.location(int(raw["pc"]))
        actual_hits = self._breakpoint_hits(frame, raw, loc) if hits is None else hits
        source = (
            self.project.source_by_id(loc.file_id)
            if loc and not loc.is_generated
            else None
        )
        function = self.functions.at_location(loc)
        computation = frame.computation
        self.step_index = int(raw["step"])
        stop_reason = reason or (
            "error" if raw["reason"] == "out_of_gas" else raw["reason"]
        )
        if raw["reason"] == "out_of_gas" and not annotation:
            annotation = "out of gas; `set $gas = N` then `c` retries this instruction"
        snapshot = FrameSnapshot(
            step=self.step_index,
            pc=int(raw["pc"]),
            opcode=int(raw["opcode"]),
            mnemonic=str(raw["mnemonic"]),
            depth=int(raw["depth"]),
            gas_remaining=int(raw["gas_remaining"]),
            gas_used=int(raw["gas_used"]),
            gas_limit=int(raw["gas_limit"]),
            gas_refund=int(raw["gas_refund"]),
            address=_address_bytes(raw["address"]),
            code_address=_address_bytes(raw["code_address"]),
            sender=_address_bytes(raw["caller"]),
            origin=_address_bytes(raw["origin"]),
            value=int(raw["value"], 16),
            calldata=bytes(raw["calldata"]),
            is_static=bool(raw["is_static"]),
            stack=tuple(
                StackEntry(index=index, value=int(value, 16), raw=int(value, 16))
                for index, value in enumerate(raw["stack"])
            ),
            memory_size=int(raw["memory_size"]),
            memory=bytes(raw["memory"]),
            contract_name=frame.artifact_name,
            source_key=source.key if source else None,
            file_id=loc.file_id if loc else -1,
            line=loc.line if loc and not loc.is_generated else 0,
            col=loc.col if loc and not loc.is_generated else 0,
            end_line=loc.end_line if loc and not loc.is_generated else 0,
            jump=loc.jump if loc else "-",
            function=function,
            backtrace=tuple(snapshots.build_backtrace(self)),
            locals=tuple(self.frame_locals(frame, computation)),
            stop_reason=stop_reason,
            hit_breakpoints=actual_hits,
            annotation=annotation,
        )
        return snapshot, actual_hits

    def _sync_frames(self, raw: dict[str, Any]) -> None:
        old = self._frames
        origin = _address_bytes(raw["origin"])
        frames: list[EvmFrame] = []
        for index, item in enumerate(raw["frames"]):
            is_current = index == len(raw["frames"]) - 1
            code = bytes(item["code"])
            is_create = item["kind"] in {"create", "create2"}
            artifact = self.code.artifact_for(code, is_create)
            stack = raw["stack"] if is_current else []
            memory = bytes(raw["memory"]) if is_current else b""
            computation = _ComputationView(
                pc=int(item["pc"]),
                stack=stack,
                memory=memory,
                gas_limit=int(item["gas_limit"]),
                gas_remaining=int(item["gas_remaining"]),
                gas_refund=int(raw["gas_refund"]) if is_current else 0,
                origin=origin,
            )
            address = _address_bytes(item["address"])
            code_address = _address_bytes(item["code_address"])
            previous = old[index] if index < len(old) else None
            reusable = previous is not None and (
                previous.address,
                previous.code_address,
                previous.kind,
            ) == (address, code_address, item["kind"])
            frame: EvmFrame
            if reusable and previous is not None:
                frame = previous
                frame.depth = int(item["depth"])
                frame.sender = _address_bytes(item["caller"])
                frame.value = int(item["value"], 16)
                frame.calldata = bytes(item["calldata"])
                frame.is_static = bool(item["is_static"])
                frame.computation = computation
            else:
                pc_map = self.code.pcmap_for(code, artifact, is_create)
                frame = EvmFrame(
                    depth=int(item["depth"]),
                    address=address,
                    code_address=code_address,
                    sender=_address_bytes(item["caller"]),
                    value=int(item["value"], 16),
                    calldata=bytes(item["calldata"]),
                    is_static=bool(item["is_static"]),
                    is_create=is_create,
                    kind=item["kind"],
                    artifact_name=artifact.name if artifact else None,
                    artifact=artifact,
                    computation=computation,
                    pc_map=pc_map,
                    disassembly=self.code.disassembly_for(code),
                    decl_pcs=cast(
                        Any,
                        self.code.declpcs_for(code, pc_map, is_create),
                    ),
                )
            frames.append(frame)
        self._frames = frames
        for frame in self._frames:
            if frame.internal:
                continue
            loc = frame.location(max(0, frame.computation.code.program_counter - 1))
            function = self.functions.at_location(loc)
            if function is not None and loc is not None:
                frame.internal.append(
                    InternalFrame(
                        function=function,
                        entry_pc=loc.pc,
                        entry_sp=len(frame.computation._stack.values),
                    )
                )

    def _advance_internal(self, raw: dict[str, Any]) -> None:
        previous = self._last_raw
        frame = self.current_frame
        if previous is None or frame is None:
            return
        if (
            int(previous["depth"]) != int(raw["depth"])
            or previous["address"] != raw["address"]
        ):
            return
        pc = int(previous["pc"])
        if frame.decl_pcs and pc in frame.decl_pcs and frame.internal:
            var = cast(Any, frame.decl_pcs[pc])
            internal = frame.internal[-1]
            if (
                internal.function is not None
                and internal.function.ast_id == var.function_id
            ):
                internal.slots[var.ast_id] = len(frame.computation._stack.values)
        if previous["mnemonic"] not in _JUMP_MNEMONICS:
            return
        loc = frame.location(pc)
        if loc is None or loc.jump not in {"i", "o"} or int(raw["pc"]) == pc + 1:
            return
        if loc.jump == "o":
            if frame.internal:
                frame.internal.pop()
            return
        destination = int(raw["pc"])
        function = self.functions.at_location(frame.location(destination))
        frame.internal.append(
            InternalFrame(
                function=function,
                entry_pc=destination,
                call_site_pc=pc,
                entry_sp=len(raw["stack"]),
            )
        )

    def _breakpoint_hits(
        self, frame: EvmFrame, raw: dict[str, Any], loc: Any
    ) -> tuple[int, ...]:
        matches = self.breakpoints.match(
            int(raw["pc"]),
            str(raw["mnemonic"]),
            loc.file_id if loc else -1,
            loc.line if loc else 0,
            frame.artifact_name,
        )
        hits = []
        for breakpoint in matches:
            if breakpoint.condition and self._eval_hook is not None:
                try:
                    holds = self._eval_hook(
                        self,
                        frame,
                        frame.computation,
                        breakpoint.condition,
                        want_bool=True,
                        bindings=self.frame_locals(frame, frame.computation),
                    )
                except Exception as exc:
                    breakpoint.condition_error = str(exc)
                else:
                    breakpoint.condition_error = None
                    if not holds:
                        continue
            breakpoint.hit_count += 1
            if breakpoint.ignore_count:
                breakpoint.ignore_count -= 1
                continue
            hits.append(breakpoint.number)
            if breakpoint.temporary:
                self.breakpoints.remove(breakpoint.number)
        return tuple(hits)

    def _sync_breakpoints(self) -> None:
        points: set[tuple[str, int]] = set()
        for artifact in self.project.artifacts.values():
            for code, is_create in (
                (artifact.deployed_bytecode, False),
                (artifact.bytecode, True),
            ):
                if not code:
                    continue
                pc_map = self.code.pcmap_for(code, artifact, is_create)
                disassembly = self.code.disassembly_for(code)
                points.update(
                    (_ANY_ADDRESS, pc)
                    for pc in self.code.declpcs_for(code, pc_map, is_create)
                )
                for breakpoint in self.breakpoints.breakpoints.values():
                    if not breakpoint.enabled:
                        continue
                    if breakpoint.contract and breakpoint.contract != artifact.name:
                        continue
                    if breakpoint.kind == BP_OPCODE:
                        pcs = [
                            instruction.pc
                            for instruction in disassembly.instructions
                            if instruction.mnemonic == breakpoint.opcode
                        ]
                    else:
                        pcs = list(breakpoint.pcs)
                    for pc in pcs:
                        if breakpoint.kind not in {BP_PC, BP_OPCODE}:
                            loc = pc_map.at(pc) if pc_map else None
                            if loc is None or (
                                loc.file_id,
                                loc.line,
                            ) != (breakpoint.file_id, breakpoint.line):
                                continue
                        points.add((_ANY_ADDRESS, pc))
        self._chain.set_breakpoints(sorted(points))

    @property
    def current_frame(self) -> EvmFrame | None:
        return self._frames[-1] if self._frames else None

    def frame_locals(
        self,
        frame: EvmFrame,
        computation: Any,
        internal_index: int | None = None,
    ) -> list[Any]:
        return framelocals.read_frame_locals(
            self.locals,
            frame,
            computation,
            internal_index,
        )

    def evaluate_code(
        self,
        frame: EvmFrame,
        computation: Any,
        code: bytes,
        data: bytes,
        keep: bool,
    ) -> dict[str, Any]:
        return self._chain.evaluate_call(
            code,
            data,
            _address_hex(frame.sender),
            value=frame.value,
            gas_limit=max(computation.get_gas_remaining(), 1_000_000),
            keep=keep,
        )

    def inspect(
        self,
        op: str,
        *args: Any,
        frame_index: int | None = None,
        **kwargs: Any,
    ) -> Any:
        if self.finished or self.last_snapshot is None:
            raise SessionError("not stopped; nothing to inspect")
        index = len(self._frames) - 1 if frame_index is None else frame_index
        if not 0 <= index < len(self._frames):
            raise SessionError(f"no such frame: {index}")
        frame = self._frames[index]
        current = index == len(self._frames) - 1
        if op == "frame_info":
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
                "internal": [item.name for item in frame.internal],
                "gas_remaining": frame.computation.get_gas_remaining(),
            }
        if op == "disassembly":
            before, after = (*args, 6, 18)[:2]
            pc = max(0, frame.computation.code.program_counter - 1)
            return [
                {
                    "pc": instruction.pc,
                    "text": instruction.render(),
                    "current": instruction.pc == pc,
                    "line": _line_at(frame, instruction.pc),
                    "jumpdest": instruction.pc in frame.disassembly.jumpdests,
                }
                for instruction in frame.disassembly.window(pc, before, after)
            ]
        if op == "locals":
            internal = kwargs.get("internal_index")
            return [
                {
                    "name": value.name,
                    "type": value.type_label,
                    "value": value.display,
                    "available": value.available,
                    "reason": value.reason,
                    "kind": value.kind,
                    "position": value.position,
                    "writable": value.writable,
                }
                for value in self.frame_locals(frame, frame.computation, internal)
            ]
        if op == "read_memory":
            offset, size = int(args[0]), int(args[1])
            data = bytes(frame.computation._memory._bytes[offset : offset + size])
            return data + bytes(size - len(data))
        if op == "read_code":
            return self._state.get_code(bytes(args[0]))
        if op == "read_balance":
            return self._state.get_balance(bytes(args[0]))
        if op == "read_nonce":
            return self._state.get_nonce(bytes(args[0]))
        if op == "logs":
            return []
        if op == "is_warm":
            return False
        if not current:
            raise SessionError(f"inspect {op!r} requires the innermost REVM frame")
        if op == "read_storage":
            address = bytes(args[1]) if len(args) > 1 and args[1] else frame.address
            return self._state.get_storage(address, int(args[0]))
        if op == "read_transient":
            address = bytes(args[1]) if len(args) > 1 and args[1] else frame.address
            return self._state.get_transient_storage(address, int(args[0]))
        if op == "write_storage":
            address = bytes(args[2]) if len(args) > 2 and args[2] else frame.address
            self._state.set_storage(address, int(args[0]), int(args[1]))
            return self._state.get_storage(address, int(args[0]))
        if op == "write_balance":
            address, value = bytes(args[0]), int(args[1])
            self._state.set_balance(address, value)
            return self._state.get_balance(address)
        if op == "write_local":
            name, value = str(args[0]), int(args[1])
            internal = kwargs.get("internal_index")
            for local in self.frame_locals(frame, frame.computation, internal):
                if local.name != name:
                    continue
                if not local.available or local.position is None:
                    raise SessionError(
                        f"`{name}` is not writable here: {local.reason or 'unavailable'}"
                    )
                if not local.writable:
                    raise SessionError(
                        f"`{name}` is a {local.type_label}; its stack slot is a "
                        "reference, not the value. Writing it would corrupt the pointer, "
                        "so it is refused"
                    )
                values = frame.computation._stack.values
                stack_index = len(values) - 1 - local.position
                self._chain.set_stack(stack_index, value)
                values[local.position] = value
                updated = [
                    item
                    for item in self.frame_locals(frame, frame.computation, internal)
                    if item.name == name
                ]
                return {
                    "name": name,
                    "display": updated[0].display if updated else str(value),
                }
            raise SessionError(f"no local named `{name}` in scope here")
        if op == "write_stack":
            index, value = int(args[0]), int(args[1])
            written = int(self._chain.set_stack(index, value), 16)
            values = frame.computation._stack.values
            values[len(values) - 1 - index] = written
            return written
        if op == "write_memory":
            return self._chain.write_memory(int(args[0]), bytes(args[1]))
        if op == "set_gas":
            return self._chain.set_gas(int(args[0]))
        if op == "set_pc":
            return self._chain.set_pc(int(args[0]))
        if op == "reseat_frame":
            pc = max(0, frame.computation.code.program_counter - 1)
            function = self.functions.at_location(frame.location(pc))
            if function is None:
                raise SessionError(
                    f"no Solidity function at pc 0x{pc:x}; nothing to reseat to"
                )
            entry_sp = kwargs.get("entry_sp")
            base = (
                int(entry_sp)
                if entry_sp is not None
                else len(frame.computation._stack.values)
            )
            seat = InternalFrame(
                function=function,
                entry_pc=pc,
                call_site_pc=-1,
                entry_sp=base,
            )
            if kwargs.get("push") or not frame.internal:
                frame.internal.append(seat)
            else:
                frame.internal[-1] = seat
            return {
                "name": function.signature,
                "entry_sp": base,
                "depth": len(frame.internal),
            }
        if op == "bind_local":
            name, stack_index = str(args[0]), int(args[1])
            internals = frame.internal
            if not internals:
                raise SessionError("no Solidity frame here; `reseat` to a function first")
            requested = kwargs.get("internal_index")
            internal_index = (
                int(requested) if requested is not None else len(internals) - 1
            )
            if not 0 <= internal_index < len(internals):
                raise SessionError(f"no such internal frame: {internal_index}")
            internal = internals[internal_index]
            function = internal.function
            if function is None:
                raise SessionError("this frame has no function; `reseat` first")
            pc = max(0, frame.computation.code.program_counter - 1)
            location = frame.location(pc)
            offset = (
                location.entry.start
                if location is not None and not location.is_generated
                else -1
            )
            matches = [
                var
                for var in self.locals.visible(function.ast_id, offset)
                if var.name == name
            ]
            if not matches:
                raise SessionError(
                    f"no local named `{name}` in scope in {function.signature} here"
                )
            stack_depth = len(frame.computation._stack.values)
            if not 0 <= stack_index < stack_depth:
                raise SessionError(
                    f"stack index {stack_index} out of range (depth {stack_depth})"
                )
            internal.slots[matches[0].ast_id] = stack_depth - 1 - stack_index
            reread = [
                value
                for value in self.frame_locals(
                    frame,
                    frame.computation,
                    internal_index,
                )
                if value.name == name
            ]
            return {
                "name": name,
                "stack_index": stack_index,
                "display": reread[0].display if reread else "<unavailable>",
                "available": bool(reread and reread[0].available),
            }
        if op == "resnapshot":
            fresh, _ = self._snapshot_from_raw(
                self._chain.snapshot(),
                reason=self.last_snapshot.stop_reason,
                hits=self.last_snapshot.hit_breakpoints,
                annotation=self.last_snapshot.annotation,
            )
            self.last_snapshot = fresh
            return fresh
        if op == "cheat":
            try:
                output = apply_cheat(
                    self.cheats, self._state, bytes(args[0]), frame.address
                )
                if cheat_name(bytes(args[0])) in {
                    "prank",
                    "startPrank",
                    "stopPrank",
                }:
                    self._sync_prank()
                return output
            except CheatError as exc:
                raise SessionError(str(exc)) from exc
        if op == "evaluate":
            if self._eval_hook is None:
                raise SessionError("no evaluator installed")
            return self._eval_hook(
                self,
                frame,
                frame.computation,
                str(args[0]),
                keep=bool(kwargs.get("keep", False)),
                bindings=self.frame_locals(
                    frame,
                    frame.computation,
                    kwargs.get("internal_index"),
                ),
            )
        raise SessionError(f"inspect {op!r} is not available on REVM yet")

    def refresh_snapshot(self) -> FrameSnapshot | None:
        if self.finished or self.last_snapshot is None:
            return self.last_snapshot
        self.inspect("resnapshot")
        return self.last_snapshot

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
    ) -> tuple[Any, int]:
        file_id = self.file_id_for(source_key)
        if file_id is None:
            raise SessionError(f"no source file matching {source_key!r}")
        snapped, pcs = self.resolve_line(file_id, line)
        return (
            self.breakpoints.add_line(
                f"{source_key}:{snapped}",
                file_id,
                snapped,
                pcs,
                temporary=temporary,
                condition=condition,
            ),
            snapped,
        )

    def break_at_function(
        self,
        name: str,
        temporary: bool = False,
        condition: str | None = None,
    ) -> tuple[Any, int]:
        matches = self.functions.find(name)
        if not matches:
            raise SessionError(f"no function named {name!r}")
        if len(matches) > 1:
            names = ", ".join(sorted({match.display_name for match in matches}))
            raise SessionError(f"{name!r} is ambiguous; try one of: {names}")
        function = matches[0]
        index = self.line_indexes.get(function.file_id)
        line = index.line_col(function.start)[0] if index else 0
        body_line, pcs = self.resolve_line(function.file_id, line + 1)
        if not pcs:
            body_line, pcs = self.resolve_line(function.file_id, line)
        return (
            self.breakpoints.add_function(
                function.display_name,
                function.file_id,
                body_line,
                pcs,
                temporary=temporary,
                condition=condition,
                contract=function.contract,
            ),
            body_line,
        )

    def break_at_opcode(
        self,
        mnemonic: str,
        temporary: bool = False,
        condition: str | None = None,
    ) -> Any:
        return self.breakpoints.add_opcode(
            mnemonic,
            temporary=temporary,
            condition=condition,
        )

    def break_at_pc(
        self,
        pc: int,
        temporary: bool = False,
        condition: str | None = None,
    ) -> Any:
        return self.breakpoints.add_pc(
            pc,
            temporary=temporary,
            condition=condition,
        )

    def watch_storage(
        self,
        expression: str,
        slot: int,
        address: bytes | None = None,
        mode: str = WATCH_WRITE,
    ) -> Any:
        return self.breakpoints.add_watch(
            expression,
            kind="storage",
            slot=slot,
            address=address,
            mode=mode,
        )

    def watch_memory(self, expression: str, offset: int, size: int = 32) -> Any:
        return self.breakpoints.add_watch(
            expression,
            kind="memory",
            offset=offset,
            size=size,
        )

    @contextmanager
    def suspended(self):
        yield


def _address_bytes(value: str) -> bytes:
    return bytes.fromhex(value.removeprefix("0x"))


def _address_hex(value: bytes) -> str:
    return "0x" + value.hex()


def _line_at(frame: EvmFrame, pc: int) -> int:
    location = frame.location(pc)
    return location.line if location is not None else 0


def _error_payload(reason: str) -> bytes:
    return _ERROR_SELECTOR + abi_encode(["string"], [reason])
