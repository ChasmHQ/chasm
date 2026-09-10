"""The MCP frontend: an AI-oriented, headless third frontend over the debugger.

Where the console renders Rich markup and the TUI draws panes, this frontend
returns structured JSON with explicit windows and truncation flags — an LLM
cannot scroll a pane or watch colour, but it can narrow an offset. Launch with
`sevm mcp` (stdio transport).

Where things live:

  driver.py     `DebugDriver`: owns the one session, mirrors `sevm run`'s wiring,
                and remembers the previous stop for diffs
  serialize.py  value formatting: hex words, memory windows, the stop report
  tools.py      the tool registrations over the driver
"""

from __future__ import annotations

from .driver import DebugDriver, DriverError
from .tools import build_server

__all__ = ["DebugDriver", "DriverError", "build_server"]
