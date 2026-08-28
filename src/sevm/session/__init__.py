"""The debug session: a Py-EVM monkeypatch that can stop mid-transaction.

The debugged program runs on a worker thread, the controller (console or TUI) drives it
over queues, and the two strictly alternate so exactly one of them is ever runnable.

Thread contract, and it is not negotiable:

  * Only the VM thread touches Py-EVM objects. The controller receives immutable
    `FrameSnapshot`s and asks for anything else with an inspect command, which the VM
    thread services while parked inside the hook.
  * The controller is the only thing that blocks on user input. The VM thread blocks
    only on its command queue, so cancellation stays clean.

Where things live:

  events.py       the messages the two threads exchange
  core.py         `DebugSession`: the threads, the frame stack, the per-opcode hook
  code.py         resolving running bytecode back to the source it came from
  patch.py        the `apply_computation` monkeypatch and its opcode loop
  stepping.py     when a step stops; when a watchpoint fires
  snapshots.py    building the `FrameSnapshot` the UI renders
  framelocals.py  recovering Solidity locals from the EVM stack
  inspect_ops.py  the reads and mutations the controller can ask for
"""

from __future__ import annotations

from .core import DebugSession
from .events import (
    Failure,
    Finished,
    Inspect,
    Paused,
    Resume,
    SessionError,
    StepMode,
)
from .snapshots import SNAPSHOT_MEMORY_LIMIT

__all__ = [
    "SNAPSHOT_MEMORY_LIMIT",
    "DebugSession",
    "Failure",
    "Finished",
    "Inspect",
    "Paused",
    "Resume",
    "SessionError",
    "StepMode",
]
