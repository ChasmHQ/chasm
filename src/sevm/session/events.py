"""Messages exchanged by the debugger controller and VM thread."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum

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


# -- VM -> controller events -------------------------------------------------


@dataclass
class Paused:
    snapshot: FrameSnapshot


@dataclass
class Finished:
    ok: bool
    error: str | None = None
    traceback: str | None = None


class SessionError(RuntimeError):
    pass
