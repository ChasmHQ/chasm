"""What the controller and the VM thread say to each other.

The two threads strictly alternate over three queues: the controller puts a `Resume` or
an `Inspect`, the VM thread answers with a `Paused`/`Finished` event or an inspect reply.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from ..frames import FrameSnapshot


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
