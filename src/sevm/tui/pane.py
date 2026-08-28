"""The scrolling, anchoring panel every pane is built on.

Panes render *more* than fits on purpose (the whole source file, all of memory), and each
one knows the line it is anchored to. The anchor logic is the subtle part: a pane
re-centres at every stop until you scroll it by hand, and scrolling back onto the anchor
re-arms following.
"""

from __future__ import annotations

from typing import Any

from rich.text import Text
from textual.app import ComposeResult
from textual.containers import VerticalScroll
from textual.content import Content
from textual.widgets import Static


class PaneBody(Static):
    """The rendered content of a pane.

    Selection is deliberately *not* implemented here. Mouse reporting is off, so the
    terminal/tmux handles dragging like any other program (system clipboard, tmux
    copy-mode, macOS Cmd+C). An in-app implementation could only copy via OSC 52, which
    tmux swallows unless `set-clipboard` is on, and could only paste back into sevm.
    """


class Pane(VerticalScroll):
    """A titled panel that renders from a snapshot, and scrolls.

    Panes render *more* than fits on purpose: the whole source file, disassembly around
    the pc, all of memory, so you can look away from where execution is paused.

    Each pane knows the line it's anchored to (current source line, pc, top of stack)
    and re-centres on it on every stop. Scrolling away from that anchor puts a marker in
    the border, so a pane never silently shows stale state.
    """

    TITLE = ""
    ANCHOR_HINT = "back to pc"
    # SOURCE and DISASSEMBLY exist to follow the pc, so they re-centre at every stop.
    # Every other pane holds the position you scrolled it to, until you scroll back onto
    # the anchor (or click the border marker), which re-arms following.
    FOLLOWS_PC = False
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
        self._user_scrolled = False
        self._anchoring = False  # true while *we* are the ones scrolling

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
        self._user_scrolled = False
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
        if line is None or (self._user_scrolled and not self.FOLLOWS_PC):
            self._update_anchor_marker()
            return
        self.scroll_to_anchor()

    def anchor_target(self) -> int:
        """The scroll position that puts the anchor row where the pane wants it."""
        return max(0, (self._anchor_line or 0) - self.visible_rows // 2)

    def scroll_to_anchor(self) -> None:
        if self._anchor_line is None:
            self._update_anchor_marker()
            return
        self._scroll_ourselves(self.anchor_target())
        self.call_after_refresh(self._update_anchor_marker)

    def _scroll_ourselves(self, y: int) -> None:
        """Scroll without it counting as the user having scrolled."""
        self._user_scrolled = False
        self._anchoring = True
        try:
            # `animate=False` matters twice over: an animated scroll lands a frame
            # later, so the marker would flicker on for that frame every time the
            # debugger steps, and the move would land outside this guard and read as
            # the user's.
            self.scroll_to(y=y, animate=False)
        finally:
            self._anchoring = False

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
        # Clickable: the marker doubles as the control.
        hint = "" if self.at_anchor else f"[@click=jump_to_anchor] {self.ANCHOR_HINT} [/]"
        if self._marker != hint:
            self._marker = hint
            self.border_subtitle = hint

    def watch_scroll_y(self, old_value: float, new_value: float) -> None:
        super().watch_scroll_y(old_value, new_value)
        if not self._anchoring and int(old_value) != int(new_value):
            # Watching the scroll position rather than the wheel catches every way a
            # pane moves by hand: wheel, scrollbar drag, scrollbar page click. Landing
            # back on the anchor is how you say "follow along again".
            self._user_scrolled = int(new_value) != self.anchor_target()
        self._update_anchor_marker()


# ==================================================================
# source
# ==================================================================
