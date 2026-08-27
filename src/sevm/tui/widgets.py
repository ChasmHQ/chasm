"""The panes.

Each pane takes a `FrameSnapshot` (and, where it needs more, data the app fetched from
the VM thread) and renders it. Panes never touch the session directly: they are pure
render functions with a widget wrapped around them.

Colour discipline, because "not intimidating for beginners" is a requirement: one accent
per data class, never reused, so a colour always means the same thing. Source is green,
stack cyan, memory blue, storage amber, gas magenta, control flow yellow, errors red.
Anything the *current* opcode is about to touch is highlighted in every pane at once,
which is what teaches the machine faster than prose does.

Layout note: every wide cell is truncated by hand rather than with Rich's
`no_wrap`/`overflow` columns. In a `Table.grid`, an unwrappable ratio column reports a
minimum width equal to its longest line, and Rich then shrinks the *fixed* columns to
make room, which silently eats the breakpoint gutter and the program counter.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from rich.syntax import Syntax
from rich.text import Text
from textual.app import ComposeResult
from textual.containers import VerticalScroll
from textual.content import Content
from textual.widgets import Static

from ..decode import StorageDecoder
from ..frames import FrameSnapshot

# One accent per data class, in ANSI colours so the debugger wears your terminal's own
# palette: a solarized or gruvbox terminal gets a solarized or gruvbox sevm, and the
# background is whatever your terminal already draws.
C_SOURCE = "ansi_green"
C_STACK = "ansi_cyan"
C_MEMORY = "ansi_blue"
# Plain blue is dark enough to fight the background at hex-digit density, so the bytes
# themselves get the bright one. Zero words are dimmed instead: a memory dump is mostly
# zeros, and dimming them makes the bytes that actually hold something stand out.
C_MEMORY_TEXT = "ansi_bright_blue"
C_MEMORY_ZERO = "ansi_bright_black"
C_STORAGE = "ansi_yellow"
C_GAS = "ansi_magenta"
# Textual parses colours as CSS, not with Rich's names: "bright_yellow" and "grey23" are
# Rich spellings that Textual rejects, and a rejected style is silently dropped rather
# than raised, so the text just quietly loses its colour.
C_FLOW = "ansi_bright_yellow"
C_ERROR = "bold ansi_red"
C_DIM = "dim"

SYNTAX_THEME = "monokai"

# Opcodes worth explaining inline the moment they are about to run. A beginner should not
# have to look up what DELEGATECALL does while staring at one.
OPCODE_HINTS = {
    "SSTORE": "write storage: slot <- value",
    "SLOAD": "read storage at slot",
    "TSTORE": "write transient storage",
    "TLOAD": "read transient storage",
    "MSTORE": "write 32 bytes to memory",
    "MSTORE8": "write 1 byte to memory",
    "MLOAD": "read 32 bytes from memory",
    "CALL": "call another contract (new frame)",
    "STATICCALL": "call another contract, read-only",
    "DELEGATECALL": "run their code against OUR storage",
    "CALLCODE": "run their code against OUR storage (legacy)",
    "CREATE": "deploy a new contract",
    "CREATE2": "deploy at a deterministic address",
    "REVERT": "abort and undo this frame",
    "RETURN": "return from this frame",
    "SELFDESTRUCT": "destroy this contract",
    "JUMP": "unconditional jump",
    "JUMPI": "jump if the condition is non-zero",
    "JUMPDEST": "a legal jump target",
    "KECCAK256": "hash a memory range",
    "CALLDATALOAD": "read 32 bytes of calldata",
    "CALLDATACOPY": "copy calldata into memory",
    "LOG0": "emit an anonymous event",
    "LOG1": "emit an event",
    "LOG2": "emit an event",
    "LOG3": "emit an event",
    "LOG4": "emit an event",
}

# How many stack items the next opcode consumes, so they can be highlighted as operands.
_OPERANDS = {
    "SSTORE": 2,
    "SLOAD": 1,
    "TSTORE": 2,
    "TLOAD": 1,
    "MSTORE": 2,
    "MSTORE8": 2,
    "MLOAD": 1,
    "JUMP": 1,
    "JUMPI": 2,
    "RETURN": 2,
    "REVERT": 2,
    "KECCAK256": 2,
    "CALL": 7,
    "CALLCODE": 7,
    "DELEGATECALL": 6,
    "STATICCALL": 6,
    "CREATE": 3,
    "CREATE2": 4,
    "ADD": 2,
    "SUB": 2,
    "MUL": 2,
    "DIV": 2,
    "SDIV": 2,
    "MOD": 2,
    "SMOD": 2,
    "EXP": 2,
    "LT": 2,
    "GT": 2,
    "SLT": 2,
    "SGT": 2,
    "EQ": 2,
    "AND": 2,
    "OR": 2,
    "XOR": 2,
    "SHL": 2,
    "SHR": 2,
    "SAR": 2,
    "BYTE": 2,
    "ISZERO": 1,
    "NOT": 1,
    "BALANCE": 1,
    "EXTCODESIZE": 1,
    "EXTCODEHASH": 1,
    "BLOCKHASH": 1,
    "LOG0": 2,
    "LOG1": 3,
    "LOG2": 4,
    "LOG3": 5,
    "LOG4": 6,
    "CALLDATALOAD": 1,
    "CALLDATACOPY": 3,
    "CODECOPY": 3,
    "RETURNDATACOPY": 3,
}

# Names for those operands, so the stack reads as arguments rather than as numbers.
_OPERAND_NAMES = {
    "SSTORE": ("slot", "value"),
    "SLOAD": ("slot",),
    "TSTORE": ("slot", "value"),
    "TLOAD": ("slot",),
    "MSTORE": ("offset", "value"),
    "MSTORE8": ("offset", "byte"),
    "MLOAD": ("offset",),
    "JUMP": ("dest",),
    "JUMPI": ("dest", "cond"),
    "RETURN": ("offset", "length"),
    "REVERT": ("offset", "length"),
    "KECCAK256": ("offset", "length"),
    "CALL": ("gas", "to", "value", "in", "insize", "out", "outsize"),
    "STATICCALL": ("gas", "to", "in", "insize", "out", "outsize"),
    "DELEGATECALL": ("gas", "to", "in", "insize", "out", "outsize"),
    "CALLCODE": ("gas", "to", "value", "in", "insize", "out", "outsize"),
    "CREATE": ("value", "offset", "length"),
    "CREATE2": ("value", "offset", "length", "salt"),
    "BALANCE": ("address",),
    "CALLDATALOAD": ("offset",),
    "LOG1": ("offset", "length", "topic0"),
    "LOG2": ("offset", "length", "topic0", "topic1"),
    "LOG3": ("offset", "length", "topic0", "topic1", "topic2"),
}


# ==================================================================
# formatting helpers
# ==================================================================


def _fit(text: Text, width: int) -> Text:
    """Truncate in place to `width`, with an ellipsis. See the module note on no_wrap."""
    out = text.copy()
    out.truncate(max(1, width), overflow="ellipsis")
    return out


def _fit_str(value: str, width: int, style: str = "") -> Text:
    return _fit(Text(value, style=style), width)


# ==================================================================
# laying out a pane as Content
# ==================================================================
#
# Panes are built from `Content`, not from a Rich `Table.grid`, and that choice is
# load-bearing rather than stylistic. Textual implements text selection on the visual
# layer: a `Content` gets character-addressable offsets, a blended selection highlight
# and ctrl+c copy for free, while a Rich renderable is opaque to all three (its
# `RichVisual.render_strips` ignores the selection entirely). Columns are therefore laid
# out by padding here instead of by Rich, which is all `Table.grid` with fixed widths was
# doing anyway.


def _cell(
    value: Any,
    width: int | None = None,
    style: str = "",
    align: str = "left",
) -> Content:
    """One column of a row, padded or ellipsised to `width`."""
    if isinstance(value, Content):
        content = value
    elif isinstance(value, Text):
        content = Content.from_rich_text(value)
        if style:
            content = content.stylize_before(style)
    else:
        content = Content.styled(str(value), style)
    if width is None:
        return content
    width = max(1, width)
    if content.cell_length > width:
        content = content.truncate(width, ellipsis=True)
    padding = width - content.cell_length
    if padding > 0:
        content = (
            content.pad_left(padding) if align == "right" else content.pad_right(padding)
        )
    return content


def _row(*cells: Content) -> Content:
    """Join columns with a single space, as `Table.grid(padding=(0, 1))` did."""
    return Content(" ").join(cells)


def _page(lines: Sequence[Content]) -> Content:
    return Content("\n").join(lines)


def _hex_compact(value: int, budget: int = 20) -> str:
    """Full hex when it fits, otherwise head..tail so magnitude stays readable."""
    text = f"0x{value:x}"
    if len(text) <= budget:
        return text
    keep = max(4, (budget - 4) // 2)
    return f"{text[: 2 + keep]}..{text[-keep:]}"


def _is_zero(word: str) -> bool:
    return not word.strip("0 ")


def memory_region(offset: int) -> str:
    """Solidity's fixed memory layout, so a beginner can orient themselves."""
    if offset < 0x40:
        return "scratch"
    if offset < 0x60:
        return "free mem ptr"
    if offset < 0x80:
        return "zero slot"
    return ""


def operand_count(mnemonic: str) -> int:
    return _OPERANDS.get(mnemonic, 0)


def operand_name(mnemonic: str, index: int) -> str:
    labels = _OPERAND_NAMES.get(mnemonic)
    if labels and index < len(labels):
        return labels[index]
    return ""


def pending_storage_slot(snap: FrameSnapshot | None) -> int | None:
    """If the next opcode is SSTORE/SLOAD, which slot it is about to touch."""
    if snap is None or not snap.stack:
        return None
    if snap.mnemonic in ("SSTORE", "SLOAD"):
        return snap.stack[0].value
    return None


def storage_rows(
    decoder: StorageDecoder | None, reader
) -> list[tuple[int, int, str, str, str]]:
    """(slot, offset, name, type, display) for the storage pane."""
    if not decoder:
        return []
    return [
        (var.slot, var.offset, var.name, var.type_label, value.display)
        for var, value in decoder.read_all(reader)
    ]


# ==================================================================
# base
# ==================================================================


class PaneBody(Static):
    """The rendered content of a pane.

    Selection is deliberately *not* implemented here. sevm runs Textual with mouse
    reporting off, so dragging is handled by the terminal (or tmux) exactly as it is in
    any other program: it reaches the system clipboard, it works with tmux copy-mode, and
    on macOS Cmd+C does what it always does. An in-app implementation could only copy via
    OSC 52, which tmux swallows unless `set-clipboard` is on, and could only paste back
    into the debugger.
    """


class Pane(VerticalScroll):
    """A titled panel that renders from a snapshot, and scrolls.

    Panes render *more* than fits on purpose: the whole source file, the disassembly
    around the pc, all of memory. That is what lets you look away from where execution
    is paused, which is most of what reading a trace involves.

    Each pane knows the line it is anchored to (the current source line, the pc, the top
    of the stack) and re-centres on it whenever the debugger stops. Scrolling away from
    that anchor puts a marker in the border, so a pane showing something other than the
    current state always says so rather than quietly lying about where you are.
    """

    TITLE = ""
    ANCHOR_HINT = "back to pc"
    # Panes are scrolled with the wheel, so they never take focus: a click that stole it
    # from the prompt would send your next keystrokes nowhere.
    can_focus = False

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.border_title = self.TITLE
        self._body = PaneBody("")
        self._rendered: Any = ""
        self._anchor_line: int | None = None  # content row to keep in view
        self._marker = ""

    def compose(self) -> ComposeResult:
        yield self._body

    @property
    def inner_width(self) -> int:
        """Usable width inside the border and padding."""
        return max(12, self.size.width - 4)

    @property
    def visible_rows(self) -> int:
        """How many content lines the pane can show at once."""
        return max(1, self.content_size.height or (self.size.height - 2))

    def show(self, renderable: Any) -> None:
        self._rendered = renderable
        self._body.update(renderable)

    def clear(self, message: str = "") -> None:
        self._anchor_line = None
        self._rendered = Content.styled(message or "", "dim")
        self._body.update(self._rendered)
        self._update_anchor_marker()

    @property
    def text(self) -> str:
        """What the pane is currently showing, as plain text.

        Textual keeps a Static's content behind a name-mangled private attribute, so the
        pane remembers what it was handed rather than leaving tests to reach in after it.
        """
        rendered = self._rendered
        if isinstance(rendered, Content):
            return rendered.plain
        if isinstance(rendered, Text):
            return rendered.plain
        return str(rendered)

    # -- anchoring ----------------------------------------------------------

    def anchor_at(self, line: int | None, centre: bool = True) -> None:
        """Record the row that represents the current debug state, and scroll to it."""
        self._anchor_line = line
        if line is None:
            self._update_anchor_marker()
            return
        self.scroll_to_anchor()

    def scroll_to_anchor(self) -> None:
        if self._anchor_line is None:
            self._update_anchor_marker()
            return
        target = max(0, self._anchor_line - self.visible_rows // 2)
        # `animate=False` matters: an animated scroll lands a frame later, and the
        # marker would flicker on for that frame every time the debugger steps.
        self.scroll_to(y=target, animate=False)
        self.call_after_refresh(self._update_anchor_marker)

    @property
    def at_anchor(self) -> bool:
        """True when the row representing the current state is on screen."""
        if self._anchor_line is None:
            return True
        top = int(self.scroll_offset.y)
        return top <= self._anchor_line < top + self.visible_rows

    def action_jump_to_anchor(self) -> None:
        self.scroll_to_anchor()

    def _update_anchor_marker(self) -> None:
        # Clickable: the marker is the affordance, so it should also be the control.
        hint = "" if self.at_anchor else f"[@click=jump_to_anchor] {self.ANCHOR_HINT} [/]"
        if self._marker != hint:
            self._marker = hint
            self.border_subtitle = hint

    def watch_scroll_y(self, old_value: float, new_value: float) -> None:
        super().watch_scroll_y(old_value, new_value)
        self._update_anchor_marker()


# ==================================================================
# source
# ==================================================================


class SourcePane(Pane):
    """Syntax-highlighted Solidity with a gdb-style gutter.

    Rich's `Syntax` widget cannot render a custom gutter, so the file is highlighted once
    and then re-laid-out line by line. That buys the breakpoint column, the current-line
    arrow, and a per-line gas column, none of which a plain Syntax could show.

    The whole file is laid out, not a window around the current line, so you can scroll
    to a function that has not run yet and set a breakpoint in it.
    """

    TITLE = "SOURCE"
    ANCHOR_HINT = "f4 back to pc"

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self._cache: dict[str, list[Text]] = {}

    def _highlighted(self, source_key: str, text: str) -> list[Text]:
        if source_key not in self._cache:
            syntax = Syntax(text, "solidity", theme=SYNTAX_THEME, word_wrap=False)
            self._cache[source_key] = list(syntax.highlight(text).split("\n"))
        return self._cache[source_key]

    def render_source(
        self,
        snap: FrameSnapshot | None,
        source_text: str | None,
        breakpoint_lines: set[int],
        gas_by_line: dict[int, int],
        window: int = 0,
    ) -> None:
        if snap is None or source_text is None or not snap.has_source:
            self.clear()
            self.show(
                Content.styled(
                    "no Solidity source for the code in this frame.\n\n"
                    "The low-level views still work:\n"
                    "  disas          disassembly around the pc\n"
                    "  x/32xb 0x40    examine memory\n"
                    "  p $stack[0]    read the stack",
                    "dim",
                )
            )
            return

        lines = self._highlighted(snap.source_key or "?", source_text)
        current = snap.line
        code_width = max(20, self.inner_width - (2 + 1 + 4 + 1 + 7 + 1))

        out: list[Content] = []
        for n in range(1, len(lines) + 1):
            is_current = n == current
            has_bp = n in breakpoint_lines
            if has_bp and is_current:
                gutter = _cell("*>", 2, "bold red")
            elif has_bp:
                gutter = _cell(" *", 2, "bold red")
            elif is_current:
                gutter = _cell("=>", 2, f"bold {C_FLOW}")
            else:
                gutter = _cell("", 2)
            spent = gas_by_line.get(n, 0)
            code = _cell(lines[n - 1], code_width)
            if is_current:
                code = code.stylize("on ansi_bright_black")
            out.append(
                _row(
                    gutter,
                    _cell(n, 4, C_DIM, align="right"),
                    _cell(f"{spent:,}" if spent else "", 7, C_GAS, align="right"),
                    code,
                )
            )
        self.show(_page(out))
        self.anchor_at(current - 1 if current else None)


# ==================================================================
# call stack
# ==================================================================


class CallStackPane(Pane):
    TITLE = "CALL STACK"

    def render_stack(self, snap: FrameSnapshot | None, selected: int = 0) -> None:
        if snap is None or not snap.backtrace:
            self.clear("no frames")
            return
        width = self.inner_width
        out: list[Content] = []
        for entry in snap.backtrace:
            marker = _cell("->" if entry.index == selected else "", 2, f"bold {C_FLOW}")
            parts: list[Any] = [
                (f"#{entry.index} ", "dim"),
                (entry.name, C_STACK if entry.kind == "solidity" else C_GAS),
            ]
            if entry.line:
                key = entry.source_key or snap.source_key or ""
                parts.append((f"  {key}:{entry.line}", C_SOURCE))
            else:
                parts.append((f"  pc 0x{entry.pc:x}", "dim"))
            if entry.detail:
                parts.append((f"  [{entry.detail}]", "dim"))
            out.append(_row(marker, _cell(Content.assemble(*parts), width - 3)))
        self.show(_page(out))
        self.anchor_at(0)


# ==================================================================
# variables
# ==================================================================


class VariablesPane(Pane):
    TITLE = "VARIABLES"

    def render_variables(
        self,
        snap: FrameSnapshot | None,
        state_vars: Sequence[tuple[str, str, str]],
        args: Sequence[tuple[str, str, str]],
        displays: Sequence[tuple[str, str]],
        locals_: Sequence[tuple[str, str, str]] = (),
    ) -> None:
        if snap is None:
            self.clear()
            return
        width = self.inner_width
        name_width = 13
        value_width = max(8, width - 5 - 1 - name_width - 2)

        out: list[Content] = []

        def add(kind: str, first: bool, name: str, style: str, value: str) -> None:
            out.append(
                _row(
                    _cell(kind if first else "", 5, "dim"),
                    _cell(name, name_width, style),
                    _cell(value, value_width, "white"),
                )
            )

        for i, (name, _type_name, value) in enumerate(args):
            add("args", i == 0, name, C_STACK, value)
        for i, (name, _type_name, value) in enumerate(locals_):
            style = "dim" if value == "<unavailable>" else C_SOURCE
            add("local", i == 0, name, style, value)
        for i, (name, _type_name, value) in enumerate(state_vars):
            add("state", i == 0, name, C_STORAGE, value)
        for i, (expr, value) in enumerate(displays):
            add("watch", i == 0, expr, C_FLOW, value)

        if not out:
            self.clear("nothing here yet; `display <expr>` pins an expression")
            return
        self.show(_page(out))
        self.anchor_at(0)


# ==================================================================
# storage
# ==================================================================


class StoragePane(Pane):
    TITLE = "STORAGE"

    def render_storage(
        self,
        snap: FrameSnapshot | None,
        rows: Sequence[tuple[int, int, str, str, str]],
        pending_slot: int | None = None,
    ) -> None:
        if snap is None or not rows:
            self.clear("no storage layout for this contract")
            return
        width = self.inner_width
        name_width = 14
        value_width = max(8, width - 7 - 1 - name_width - 2)

        out: list[Content] = []
        for slot, offset, name, _type_name, value in rows:
            hot = pending_slot is not None and slot == pending_slot
            value_text = _cell(
                value, value_width - (12 if hot else 0), "bold white" if hot else "white"
            )
            if hot:
                value_text = value_text.append_text("  <- writing", "bold red")
            out.append(
                _row(
                    _cell(f"{slot:>3}+{offset:<2}", 7, "bold red" if hot else "dim"),
                    _cell(name, name_width, f"bold {C_STORAGE}" if hot else C_STORAGE),
                    value_text,
                )
            )
        self.show(_page(out))
        self.anchor_at(0)


# ==================================================================
# low level
# ==================================================================


class DisassemblyPane(Pane):
    TITLE = "DISASSEMBLY"
    ANCHOR_HINT = "f4 back to pc"

    def render_disassembly(
        self, rows: Sequence[dict], snap: FrameSnapshot | None
    ) -> None:
        if not rows:
            self.clear()
            return
        width = self.inner_width
        text_width = max(10, width - 2 - 1 - 4 - 1 - 4 - 2)
        out: list[Content] = []
        for entry in rows:
            current = entry["current"]
            body = _cell(entry["text"], text_width, "bold white" if current else "white")
            if entry["jumpdest"]:
                body = body.stylize(C_GAS, 0, 8)
            cost = ""
            if current and snap is not None and snap.static_gas is not None:
                cost = str(snap.static_gas)
            out.append(
                _row(
                    _cell("=>" if current else "", 2, f"bold {C_FLOW}"),
                    _cell(f"{entry['pc']:04x}", 4, C_DIM),
                    body,
                    _cell(cost, 4, C_GAS, align="right"),
                )
            )
        self.show(_page(out))
        current_row = next((i for i, entry in enumerate(rows) if entry["current"]), None)
        self.anchor_at(current_row)


def local_stack_labels(snap: FrameSnapshot | None) -> dict[int, tuple[str, str]]:
    """Stack index (0 = top) -> (label, kind) for slots holding a named local.

    `LocalValue.position` is an absolute position counted from the bottom of the stack,
    because that is how the frame base is measured; the STACK pane counts from the top.
    A local wider than one word (a calldata reference is offset plus length) labels each
    of its words so neither looks like an anonymous temporary.
    """
    labels: dict[int, tuple[str, str]] = {}
    if snap is None or not snap.locals:
        return labels
    depth = len(snap.stack)
    for local in snap.locals:
        position = getattr(local, "position", None)
        if position is None or not local.name or not local.available:
            continue
        words = max(1, len(getattr(local, "words", ()) or ()))
        for offset in range(words):
            index = depth - 1 - (position + offset)
            if 0 <= index < depth:
                suffix = "" if words == 1 else (".ptr" if offset == 0 else ".len")
                labels[index] = (f"{local.name}{suffix}", local.kind)
    return labels


class StackPane(Pane):
    TITLE = "STACK"

    ANCHOR_HINT = "back to top"

    def render_stack_values(self, snap: FrameSnapshot | None, limit: int = 0) -> None:
        if snap is None:
            self.clear()
            return
        if not snap.stack:
            self.clear("empty")
            return
        width = self.inner_width
        operands = operand_count(snap.mnemonic)
        entries = list(snap.stack if not limit else snap.stack[:limit])
        locals_by_index = local_stack_labels(snap)

        # Two different things want the note column: what the *current opcode* is about
        # to consume, and what the *frame* calls this slot. Both are worth knowing, so
        # they share the column and are told apart by colour, red for an operand about to
        # be eaten and cyan for a named local.
        notes: list[tuple[str, str]] = []
        for entry in entries:
            operand = operand_name(snap.mnemonic, entry.index)
            local, kind = locals_by_index.get(entry.index, ("", ""))
            if operand and local:
                notes.append((f"{operand} {local}", "both"))
            elif operand:
                notes.append((operand, "operand"))
            elif local:
                notes.append((local, kind or "local"))
            else:
                notes.append(("", ""))

        # The label must not starve the value it is labelling: cap the note column and
        # keep at least a readable prefix of the hex.
        note_width = min(max((len(n) for n, _ in notes), default=0), max(6, width // 3))
        value_width = max(10, width - 2 - 1 - (note_width + 1 if note_width else 0) - 1)

        out: list[Content] = []
        for entry, (note, note_kind) in zip(entries, notes, strict=False):
            hot = entry.index < operands
            named = bool(note) and note_kind not in ("", "operand")
            cells = [
                _cell(entry.index, 2, "bold white" if hot else "dim"),
                _cell(
                    _hex_compact(entry.value, value_width),
                    value_width,
                    f"bold {C_STACK}" if hot else (C_SOURCE if named else C_STACK),
                ),
            ]
            if note_width:
                if note_kind == "operand":
                    style = "bold red" if hot else "dim"
                elif note_kind == "both":
                    style = "bold red"
                elif note_kind == "param":
                    style = f"bold {C_SOURCE}"
                elif note_kind:
                    style = C_SOURCE
                else:
                    style = "dim"
                cells.append(_cell(note, note_width, style))
            out.append(_row(*cells))

        if limit and len(snap.stack) > limit:
            filler = [
                _cell("", 2),
                _cell(f"+{len(snap.stack) - limit} more", value_width, "dim"),
            ]
            if note_width:
                filler.append(_cell("", note_width))
            out.append(_row(*filler))
        self.show(_page(out))
        # The top of the stack is the interesting end, and it is row zero.
        self.anchor_at(0)


class MemoryPane(Pane):
    """Memory in gdb's `x/g` shape: an address, then 8-byte giant words.

    Byte-per-column hex is what a hex editor wants; the EVM works in 32-byte words, so
    every value you are looking for is aligned to one. Grouping in giants means a word
    reads as four columns rather than as thirty-two loose bytes, and it matches what
    `x/4xg 0x80` prints, so the pane and the command agree.

    How many giants fit is decided by the pane width, since four of them need 75 columns
    and the pane is rarely that wide.
    """

    TITLE = "MEMORY"
    ANCHOR_HINT = "back to top"
    GIANT = 8  # bytes per group, gdb's `g` size

    def render_memory(self, snap: FrameSnapshot | None, rows: int = 0) -> None:
        if snap is None:
            self.clear()
            return
        data = snap.memory
        if not data:
            self.clear("memory is empty")
            return

        width = self.inner_width
        address_width = 7  # "0xffff:"
        giant_width = self.GIANT * 2
        budget = width - address_width - 1
        per_row = max(1, min(4, (budget + 1) // (giant_width + 1)))
        hex_width = per_row * giant_width + (per_row - 1)
        note_width = budget - hex_width - 1
        show_notes = note_width >= 6
        step = per_row * self.GIANT

        out: list[Content] = []

        def add(address: Content, body: Content, note: Content | None = None) -> None:
            cells = [address, body]
            if show_notes:
                cells.append(note if note is not None else _cell("", note_width))
            out.append(_row(*cells))

        last_region = None
        for start in range(0, len(data), step):
            chunk = data[start : start + step]
            giants = [
                chunk[i : i + self.GIANT].hex().ljust(giant_width, " ")
                for i in range(0, len(chunk), self.GIANT)
            ]
            region = memory_region(start)
            if region and region != last_region:
                # A heading rather than a right-hand column: giant words leave no room
                # for one in a narrow pane, and Solidity's fixed layout is worth keeping
                # visible at every width.
                add(
                    _cell("", address_width), _cell(region, hex_width, f"dim {C_STORAGE}")
                )
            last_region = region or last_region
            note = None
            if show_notes:
                printable = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
                note = _cell(printable, note_width, "dim" if not any(chunk) else "white")
            words = Content(" ").join(
                _cell(
                    word, giant_width, C_MEMORY_ZERO if _is_zero(word) else C_MEMORY_TEXT
                )
                for word in giants
            )
            add(
                _cell(f"0x{start:04x}:", address_width, C_STACK),
                _cell(words, hex_width),
                note,
            )

        if snap.memory_size > len(data):
            add(
                _cell("", address_width),
                _cell(f"... {snap.memory_size:,} bytes total", hex_width, "dim"),
            )
        self.show(_page(out))
        self.anchor_at(0)


class CommandLog(Pane):
    """The command transcript, and the one pane whose values are never truncated.

    A `RichLog` would be the obvious widget, but it stores its scrollback as rendered
    Rich strips, which Textual cannot select: dragging over it produced nothing while
    every other pane worked. Keeping the lines as `Content` makes it selectable like the
    rest, and it is the pane most worth copying out of, because `p` prints whole
    addresses here while the panes ellipsise them to fit.
    """

    TITLE = ""
    ANCHOR_HINT = "back to newest"

    MAX_LINES = 2000  # bounded, or a long `continue` session grows without limit

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self._lines: list[Content] = []

    def write(self, markup: Any) -> None:
        """Append a line. Accepts console markup, plain text, or Content."""
        if isinstance(markup, Content):
            content = markup
        elif isinstance(markup, Text):
            content = Content.from_rich_text(markup)
        else:
            try:
                content = Content.from_markup(str(markup))
            except Exception:
                content = Content(str(markup))
        self._lines.extend(content.split("\n"))
        del self._lines[: max(0, len(self._lines) - self.MAX_LINES)]
        self.show(_page(self._lines))
        self._anchor_line = max(0, len(self._lines) - 1)
        self.scroll_end(animate=False)

    def clear(self, message: str = "") -> None:
        self._lines = []
        self.show(Content.styled(message or "", "dim"))
        self._anchor_line = None
        self._update_anchor_marker()

    @property
    def text(self) -> str:
        return "\n".join(line.plain for line in self._lines)


class StatusBar(Static):
    """The one-line summary above the panes."""

    def render_status(self, snap: FrameSnapshot | None, running: bool = False) -> None:
        if running:
            self.update(Content.styled("  running ...", f"bold {C_FLOW}"))
            return
        if snap is None:
            self.update(Content.styled("  not running", "dim"))
            return
        fn = (
            snap.function.signature
            if snap.function
            else (snap.contract_name or "unknown code")
        )
        parts: list[Any] = ["  ", (fn, f"bold {C_STACK}")]
        if snap.has_source:
            parts.append((f"  {snap.source_key}:{snap.line}", C_SOURCE))
        parts += [
            ("   gas ", "dim"),
            (f"{snap.gas_remaining:,}", C_GAS),
            (f"/{snap.gas_limit:,}", "dim"),
            ("   depth ", "dim"),
            (str(snap.depth), "white"),
            ("   pc ", "dim"),
            (f"0x{snap.pc:x}", "white"),
            "   ",
            (snap.mnemonic, f"bold {C_FLOW}"),
        ]
        hint = OPCODE_HINTS.get(snap.mnemonic)
        if hint:
            parts.append((f"  ({hint})", "dim"))
        if snap.is_static:
            parts.append(("   STATIC", "bold red"))
        if snap.stop_reason == "error":
            parts.append((f"   {snap.annotation}", C_ERROR))
        self.update(Content.assemble(*parts))
