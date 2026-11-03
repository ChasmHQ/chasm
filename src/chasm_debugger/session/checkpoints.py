"""Named checkpoints: persist a stop, experiment freely, come back without replay.

The reviewer's shape: reach a deep stop once, then try `set $stack[i] = v` +
`jump` variants against a saved snapshot instead of re-running a 50k-step
prefix per experiment. Three layers have to come back:

  * the EVM frame state — stack, memory, gas, pc, logs — per live computation;
  * the chain state — storage, balances, transient storage, warm/cold — through
    py-evm's journal, the same `state.snapshot()`/`revert()` pair the evaluator
    already exercises while parked;
  * the debugger's own bookkeeping — cheat state, gas profiles, watchpoint
    baselines, the Solidity frame model, the provenance record.

Two hard rules, both documented on the verbs:

  * RESTORE only works while the frame stack is exactly the one that was saved.
    A checkpoint taken before a CALL cannot be restored from inside that CALL
    (the VM thread is parked in the callee's Python frame; finish out first),
    and after the frames returned the saved inner computations are gone.
  * restoring discards every checkpoint saved after the restored one: py-evm's
    journal nests checkpoints, and reverting an outer one pops the inner ones.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from typing import Any


@dataclass
class FrameCapture:
    """The mutable state of one live computation at the checkpoint."""

    computation_id: int
    depth: int
    stack: tuple[Any, ...]
    memory: bytes
    gas_remaining: int
    gas_refunded: int
    pc: int
    logs: list[tuple]
    # The Solidity frame model, copied: InternalFrames are small dataclasses, the
    # FunctionInfo inside them is immutable, and `slots` needs a dict copy.
    internal: list[Any]


@dataclass
class Checkpoint:
    name: str
    frames: list[FrameCapture]
    journal: Any  # computation.state.snapshot(): (state_root, checkpoint)
    step_index: int
    gas_by_line: dict[tuple[int, int], int]
    gas_by_opcode: dict[str, int]
    cheats: dict[str, Any]
    watch_baselines: dict[int, tuple[bool, Any]]
    provenance_len: int
    last_revert: str | None
    note: str = ""
    captures: int = 0  # times restored from


def capture_frames(frames: list[Any]) -> list[FrameCapture]:
    """Deep-capture every live frame's computation state (VM thread only)."""
    out: list[FrameCapture] = []
    for frame in frames:
        computation = frame.computation
        out.append(
            FrameCapture(
                computation_id=id(computation),
                depth=frame.depth,
                stack=tuple(computation._stack.values),
                memory=bytes(computation._memory._bytes),
                gas_remaining=computation._gas_meter.gas_remaining,
                gas_refunded=computation._gas_meter.gas_refunded,
                # The boundary pc (the opcode about to run), NOT program_counter:
                # at a pause the stream already consumed the opcode byte, and the
                # loop's next() must re-read from the boundary after a restore.
                pc=max(0, computation.code.program_counter - 1),
                logs=list(computation._log_entries),
                internal=[replace(f, slots=dict(f.slots)) for f in frame.internal],
            )
        )
    return out


def restore_frames(frames: list[Any], captures: list[FrameCapture]) -> None:
    """Write captured state back into the live computations (VM thread only).

    Callers must have verified the frame stack matches the captures exactly.
    """
    for frame, capture in zip(frames, captures, strict=True):
        computation = frame.computation
        computation._stack.values[:] = list(capture.stack)
        mem = computation._memory._bytes
        if len(mem) > len(capture.memory):
            del mem[len(capture.memory) :]
        else:
            mem.extend(b"\x00" * (len(capture.memory) - len(mem)))
        mem[:] = capture.memory
        computation._gas_meter.gas_remaining = capture.gas_remaining
        computation._gas_meter.gas_refunded = capture.gas_refunded
        computation.code.program_counter = capture.pc
        computation._log_entries[:] = list(capture.logs)
        frame.internal[:] = [replace(f, slots=dict(f.slots)) for f in capture.internal]


def frames_match(frames: list[Any], captures: list[FrameCapture]) -> bool:
    """True when every live computation is the very object that was captured."""
    if len(frames) != len(captures):
        return False
    return all(
        id(frame.computation) == capture.computation_id
        for frame, capture in zip(frames, captures, strict=True)
    )


def capture_cheats(cheats: Any) -> dict[str, Any]:
    prank = None
    if cheats.prank is not None:
        prank = replace(cheats.prank)  # frozen-ish dataclass; copy defensively
    return {
        "prank": prank,
        "labels": dict(cheats.labels),
        "console_len": len(cheats.console_lines),
        "seed": cheats.seed,
    }


def restore_cheats(cheats: Any, saved: dict[str, Any]) -> None:
    cheats.prank = saved["prank"]
    cheats.labels.clear()
    cheats.labels.update(saved["labels"])
    del cheats.console_lines[saved["console_len"] :]
    cheats.reseed(saved["seed"])
    cheats._rng = None


@dataclass
class CheckpointSet:
    """Ordered named checkpoints owned by one DebugSession.

    Insertion order matters: restoring a checkpoint discards every one saved
    after it, because the journal checkpoint it holds pops the journal
    checkpoints nested above it.
    """

    _by_name: dict[str, Checkpoint] = field(default_factory=dict)

    def save(self, checkpoint: Checkpoint) -> Checkpoint:
        # Re-saving a name replaces it in place, keeping its original position.
        self._by_name[checkpoint.name] = checkpoint
        return checkpoint

    def get(self, name: str) -> Checkpoint | None:
        return self._by_name.get(name)

    def latest(self) -> Checkpoint | None:
        return next(reversed(self._by_name.values()), None)

    def discard_after(self, name: str) -> list[str]:
        """Drop every checkpoint saved after `name` (journal nesting rule)."""
        dropped: list[str] = []
        names = list(self._by_name)
        position = names.index(name)
        for later in names[position + 1 :]:
            dropped.append(later)
            del self._by_name[later]
        return dropped

    def clear(self) -> None:
        self._by_name.clear()

    def listing(self) -> list[dict[str, Any]]:
        rows = []
        for position, cp in enumerate(self._by_name.values()):
            rows.append(
                {
                    "name": cp.name,
                    "order": position,
                    "frames": len(cp.frames),
                    "step": cp.step_index,
                    "restores": cp.captures,
                    "note": cp.note,
                }
            )
        return rows

    def __bool__(self) -> bool:
        return bool(self._by_name)

    def __len__(self) -> int:
        return len(self._by_name)
