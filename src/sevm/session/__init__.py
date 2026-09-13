"""The REVM debug session shared by the console, TUI, and headless clients.

The debugged program runs on a worker thread, the controller (console or TUI) drives it
over queues, and the two strictly alternate so exactly one of them is ever runnable.

Thread contract, and it is not negotiable:

  * Only the VM thread touches REVM. The controller receives immutable
    `FrameSnapshot`s and asks for anything else with an inspect command, which the VM
    thread services while parked inside the hook.
  * The controller is the only thing that blocks on user input. The VM thread blocks
    only on its command queue, so cancellation stays clean.

Where things live:

  events.py       the messages the two threads exchange
  code.py         resolving running bytecode back to the source it came from
  revm.py         `DebugSession`: source stepping backed by the Rust engine
  snapshots.py    building the `FrameSnapshot` the UI renders
  framelocals.py  recovering Solidity locals from the EVM stack
"""

from __future__ import annotations

from .events import (
    Failure,
    Finished,
    Inspect,
    Paused,
    Resume,
    SessionError,
    StepMode,
)
from .revm import RevmDebugSession
from .snapshots import SNAPSHOT_MEMORY_LIMIT

DebugSession = RevmDebugSession

__all__ = [
    "SNAPSHOT_MEMORY_LIMIT",
    "DebugSession",
    "Failure",
    "Finished",
    "Inspect",
    "Paused",
    "Resume",
    "RevmDebugSession",
    "SessionError",
    "StepMode",
]
