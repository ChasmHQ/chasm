"""The pane palette.

One accent per data class, never reused, so a colour always means the same thing:
source green, stack cyan, memory blue, storage amber, gas magenta, control flow yellow,
errors red. Whatever the current opcode is about to touch is highlighted in every pane
at once.

Textual parses these as CSS, not Rich's names ("bright_yellow", "grey23" are Rich
spellings Textual rejects), and a rejected style is silently dropped rather than raised.
`sevm.tcss` mirrors them in the pane borders.
"""

from __future__ import annotations

# One accent per data class, in ANSI colours, so the debugger follows the terminal's own palette.
C_SOURCE = "ansi_green"
C_STACK = "ansi_cyan"
C_MEMORY = "ansi_blue"
# Plain blue is too dark against hex-digit density, so bytes get the bright variant.
# Zero words are dimmed instead, so the memory dump's non-zero bytes stand out.
C_MEMORY_TEXT = "ansi_bright_blue"
C_MEMORY_ZERO = "ansi_bright_black"
C_STORAGE = "ansi_yellow"
C_GAS = "ansi_magenta"
# Textual parses colours as CSS, not Rich's names ("bright_yellow", "grey23" are Rich
# spellings Textual rejects). A rejected style is silently dropped, not raised.
C_FLOW = "ansi_bright_yellow"
C_ERROR = "bold ansi_red"
C_DIM = "dim"

SYNTAX_THEME = "monokai"

# Opcodes worth explaining inline the moment they are about to run. A beginner should not
