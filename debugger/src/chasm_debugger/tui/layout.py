"""Cell, row and page construction for the panes.

Wide cells are truncated here by hand, not with Rich's `no_wrap`/`overflow` columns: in a
`Table.grid` an unwrappable ratio column reports a minimum width equal to its longest
line, and Rich then shrinks the *fixed* columns to make room, silently eating the
breakpoint gutter and the program counter.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from rich.text import Text
from textual.content import Content

from ..decode import StorageDecoder
from ..frames import FrameSnapshot


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
# Panes are built from `Content`, not Rich `Table.grid`: Textual's text selection,
# highlighting and ctrl+c copy only work on `Content` (a Rich renderable's
# `render_strips` ignores selection entirely). Columns are laid out by padding here
# instead, which is all `Table.grid` did anyway.


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
