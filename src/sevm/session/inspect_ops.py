"""The operations the controller may ask the VM thread to run while it is parked.

`session.inspect("read_storage", 3)` dispatches by name to `_op_read_storage` here, on the
VM thread, with the frame still alive. The controller thread never touches a Py-EVM object
itself; this is the only way it reads or writes one.
"""

from __future__ import annotations

from typing import Any

from .. import assembly
from ..cheatcodes import apply_cheat
from ..frames import EvmFrame
from . import snapshots
from .events import Failure, Inspect, SessionError


class InspectOps:
    """Reads and mutations against the live computation, bound to one session."""

    def __init__(self, session: Any) -> None:
        self.session = session

    def handle(self, cmd: Inspect, frame: EvmFrame, computation: Any) -> Any:
        handler = getattr(self, f"_op_{cmd.op}", None)
        if handler is None:
            return Failure(f"unknown inspect op: {cmd.op}")
        if cmd.frame_index is not None:
            frames = self.session._frames
            if not 0 <= cmd.frame_index < len(frames):
                return Failure(f"no such frame: {cmd.frame_index}")
            frame = frames[cmd.frame_index]
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
            for v in self.session.frame_locals(frame, computation, internal_index)
        ]

    def _op_resnapshot(self, frame: EvmFrame, computation: Any) -> dict:
        """The live stack/memory/gas/locals, for `refresh_snapshot`."""
        return snapshots.live_view(self.session, frame, computation)

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
        for local in self.session.frame_locals(frame, computation, internal_index):
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
                for v in self.session.frame_locals(frame, computation, internal_index)
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
        # If this refill happens while parked on an out-of-gas error, the opcode that
        # failed never ran; arming the flag makes the loop retry it on resume.
        self.session._gas_rescued = True
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
            return assembly.run(self.session, computation, source)
        except assembly.AsmError as exc:
            return Failure(str(exc))

    def _op_cheat(
        self,
        frame: EvmFrame,
        computation: Any,
        calldata: bytes,
        caller: bytes | None = None,
    ) -> bytes:
        """Interactive `vm.*` cheat: apply it against the frame we are stopped in.

        Pranks target the current frame's address unless a caller is given.
        """
        return apply_cheat(
            self.session.cheats,
            computation.state,
            bytes(calldata),
            caller if caller is not None else frame.address,
        )

    # -- speculative execution ---------------------------------------------

    def _op_evaluate(
        self,
        frame: EvmFrame,
        computation: Any,
        expression: str,
        keep: bool = False,
        internal_index: int | None = None,
    ) -> Any:
        if self.session._eval_hook is None:
            raise SessionError("no evaluator installed")
        return self.session._eval_hook(
            self.session,
            frame,
            computation,
            expression,
            keep=keep,
            bindings=self.session.frame_locals(frame, computation, internal_index),
        )
