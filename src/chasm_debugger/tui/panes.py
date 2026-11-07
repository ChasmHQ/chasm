"""The panes themselves.

Each takes a `FrameSnapshot` (plus, where it needs more, data the app fetched from the VM
thread) and renders it. Panes never touch the session: they are pure render functions with
a widget wrapped around them.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from rich.syntax import Syntax
from rich.text import Text
from textual.content import Content
from textual.widgets import Static

from ..frames import FrameSnapshot
from .layout import (
    _cell,
    _hex_compact,
    _is_zero,
    _page,
    _row,
    local_stack_labels,
    memory_region,
)
from .opcodes import OPCODE_HINTS, operand_count, operand_name
from .pane import Pane
from .theme import (
    C_DIM,
    C_ERROR,
    C_FLOW,
    C_GAS,
    C_MEMORY_TEXT,
    C_MEMORY_ZERO,
    C_SOURCE,
    C_STACK,
    C_STORAGE,
    SYNTAX_THEME,
)


class SourcePane(Pane):
    """Syntax-highlighted Solidity with a gdb-style gutter.

    Rich's `Syntax` widget can't render a custom gutter, so the file is highlighted once
    and re-laid-out line by line, which buys the breakpoint column, current-line arrow,
    and per-line gas column.

    The whole file is laid out, not a window around the current line, so you can scroll
    ahead and set a breakpoint in code that hasn't run yet.
    """

    TITLE = "SOURCE"
    FOLLOWS_PC = True

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
    ANCHOR_HINT = "back to top"

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
    ANCHOR_HINT = "back to top"

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
    ANCHOR_HINT = "back to top"

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
    FOLLOWS_PC = True

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

    The EVM works in 32-byte words, so grouping in 8-byte giants (matching `x/4xg`)
    reads as four columns instead of thirty-two loose bytes. How many fit per row is
    decided by pane width, and a giant costs its 16 hex digits plus the 8 characters of
    ASCII beside it, so four need 108 columns and the pane rarely has them.
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
        # A giant is packed together with the 8 ASCII characters that belong to it,
        # rather than the preview taking whatever the hex leaves: a string is what that
        # column is read for, and `flag{hereyoug...` cut two characters short of its `}`
        # is worse than one giant less per row. Only a pane too narrow for even one
        # giant and its preview falls back to packing hex first, ellipsising the rest.
        with_note = giant_width + 1 + self.GIANT
        if budget >= with_note:
            per_row = min(4, budget // with_note)
            note_width = per_row * self.GIANT
            show_notes = True
        else:
            per_row = max(1, min(4, (budget + 1) // (giant_width + 1)))
            note_width = budget - per_row * giant_width - per_row
            show_notes = note_width >= 6
        hex_width = per_row * giant_width + (per_row - 1)
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

    `RichLog` stores its scrollback as rendered Rich strips, which Textual can't select
    (dragging over it did nothing). Keeping lines as `Content` makes it selectable like
    the rest, which matters here most: `p` prints whole addresses while the other panes
    ellipsise them.
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
        if self._user_scrolled:
            # Scrolled back to read something: new output must not yank you to the end,
            # any more than it does in a terminal's scrollback.
            self._update_anchor_marker()
        else:
            self.scroll_to_anchor()

    def anchor_target(self) -> int:
        # The newest line means the bottom, not a centred row.
        return self.max_scroll_y

    def scroll_to_anchor(self) -> None:
        # `scroll_end` rather than the base scroll: the end is only known once the new
        # content has laid out, which is why it defers to after the refresh. The move
        # therefore lands outside `_anchoring`, and is recognised as ours by arriving
        # exactly on `anchor_target`.
        self._user_scrolled = False
        self.scroll_end(animate=False)
        self.call_after_refresh(self._update_anchor_marker)

    def clear(self, message: str = "") -> None:
        self._lines = []
        self.show(Content.styled(message or "", "dim"))
        self._anchor_line = None
        self._user_scrolled = False
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
