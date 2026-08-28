"""Copying command output and pane selections to the system clipboard."""

from __future__ import annotations

import asyncio

import pytest
from harness import (
    stop_at,
)
from tui_harness import run_tui, stop_at_credit, tui_app


def test_copy_puts_a_commands_output_on_the_clipboard(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p amount - fee")
    assert result.ok, result.error
    assert result.notice and "copied" in result.notice
    assert fake_clipboard == ["$1 = 1995000000000000000 (1.995000 ether)  (uint256)"]


def test_copy_strips_console_markup(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("copy info locals")
    text = fake_clipboard[-1]
    assert "[cyan]" not in text and "[/dim]" not in text
    assert "fee" in text and "amount" in text
    # Indentation survives, so a pasted table still lines up.
    assert text.splitlines()[0].startswith("  ")


def test_bare_copy_takes_the_last_output(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("p who")
    dbg.run("copy")
    assert fake_clipboard and fake_clipboard[-1].startswith("$1 = 0x")


def test_bare_copy_does_not_copy_its_own_message(deposit_debugger, fake_clipboard):
    """A bound method is a fresh object each lookup, so `is` cannot exclude `copy`."""
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    dbg.run("p fee")
    dbg.run("copy")
    first = fake_clipboard[-1]
    dbg.run("copy")
    assert fake_clipboard[-1] == first, "a second copy must not copy the first's message"
    assert "copied" not in first


def test_copy_reports_a_failing_command_instead_of_copying(
    deposit_debugger, fake_clipboard
):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p nosuchthing")
    assert not result.ok
    assert fake_clipboard == []


def test_copy_with_nothing_to_copy_says_so(deposit_debugger, fake_clipboard):
    dbg = deposit_debugger
    dbg.commands._last_output = []
    result = dbg.run("copy")
    assert not result.ok
    assert "nothing to copy" in (result.error or "")
    assert fake_clipboard == []


def test_copy_surfaces_a_missing_clipboard_tool(deposit_debugger, monkeypatch):
    from sevm import clipboard

    def boom(text: str) -> str:
        raise clipboard.ClipboardError("no system clipboard command found")

    monkeypatch.setattr(clipboard, "copy", boom)
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p owner")
    assert not result.ok
    assert "clipboard" in (result.error or "")


def test_clipboard_module_picks_a_tool_or_says_none(monkeypatch):
    from sevm import clipboard

    monkeypatch.setattr(clipboard.shutil, "which", lambda name: None)
    assert clipboard.available_tool() is None
    with pytest.raises(clipboard.ClipboardError):
        clipboard.copy("hello")

    monkeypatch.setattr(clipboard.shutil, "which", lambda name: name == "pbcopy")
    assert clipboard.available_tool() == "pbcopy"


def test_every_pane_renders_content_so_textual_can_select_it(bank):
    """The whole point of building panes from Content rather than a Rich table.

    Textual implements selection on the visual layer: a Content is character-addressable
    and gets a blended highlight, while a Rich renderable is opaque to both.
    """
    from textual.content import Content

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        from sevm.tui.pane import Pane

        out = {}
        for pane in app.query(Pane):
            if not pane.display:
                continue
            out[pane.id] = isinstance(pane._body._render(), Content)
        return out

    kinds = run_tui(session, app, size, body)
    assert kinds, "no panes found"
    for name, is_content in kinds.items():
        assert is_content, f"{name} does not render Content, so it cannot be selected"


def test_dragging_a_pane_selects_characters_and_ctrl_c_copies(bank):
    session, proj_, app, size = tui_app(bank)
    copied: list = []

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy
        clipboard.copy = lambda text: (copied.append(text), "pbcopy")[1]
        try:
            await stop_at_credit(app, pilot, proj_)
            storage = app.query_one("#storage")
            await pilot.mouse_down(storage, offset=(9, 2))
            await pilot.hover(storage, offset=(30, 2))
            await pilot.pause()
            selection = app.screen.selections.get(storage._body)
            text = app.screen.get_selected_text()
            await pilot.mouse_up(storage, offset=(30, 2))
            await pilot.pause()
            await pilot.press("ctrl+c")
            await pilot.pause()
            return selection, text, app.is_running
        finally:
            clipboard.copy = monkey

    selection, text, still_running = run_tui(session, app, size, body)
    # Character offsets, not the whole-widget fallback Rich renderables collapse to.
    assert selection is not None and selection.start is not None
    assert selection.start.x != selection.end.x
    assert text and text.strip()
    assert copied and copied[-1] == text, "ctrl+c must copy the selection"
    assert still_running, "ctrl+c with a selection copies rather than quitting"


def test_ctrl_c_without_a_selection_still_quits(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        app.screen.clear_selection()
        await pilot.press("ctrl+c")
        await pilot.pause()
        return app.is_running

    assert run_tui(session, app, size, body) is False


def test_the_command_log_is_selectable(bank):
    """RichLog stores rendered strips and cannot be selected; the log is Content."""
    from textual.selection import Selection

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        app.run_command("info storage")
        await asyncio.sleep(1.5)
        await pilot.pause()
        log = app.query_one("#log")
        extracted = log._body.get_selection(Selection(None, None))
        return (extracted[0] if extracted else None), log.text

    selected, text = run_tui(session, app, size, body)
    assert selected and "totalDeposits" in selected
    assert "totalDeposits" in text
    # Markup is parsed, not copied literally.
    assert "[cyan]" not in selected and "[/dim]" not in selected


def test_clipboard_falls_back_to_osc52_when_no_tool_exists(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy

        def boom(text):
            raise clipboard.ClipboardError("no system clipboard command found")

        clipboard.copy = boom
        fallback: list = []
        original = type(app).__mro__[1].copy_to_clipboard
        try:
            await stop_at_credit(app, pilot, proj_)
            app.screen.clear_selection()
            # Textual's own OSC 52 path must still run, so the copy is not simply lost.
            import textual.app as textual_app

            textual_app.App.copy_to_clipboard = lambda self, text: fallback.append(text)
            app.copy_to_clipboard("some text")
            await pilot.pause()
            return list(fallback), [n.message for n in app._notifications]
        finally:
            clipboard.copy = monkey
            import textual.app as textual_app

            textual_app.App.copy_to_clipboard = original

    fallback, toasts = run_tui(session, app, size, body)
    assert fallback == ["some text"], "must fall back to the terminal, not drop the copy"
    assert toasts and "OSC 52" in toasts[-1], "the fallback must say it happened"


def test_every_colour_constant_parses_as_a_textual_style():
    """Rich colour names are not Textual colour names, and a bad one fails silently.

    `bright_yellow` and `grey23` are Rich spellings that Textual rejects; a rejected
    style is dropped rather than raised, so the affected text just loses its colour with
    nothing to show for it. This is the guard that turns that into a test failure.
    """
    from textual.style import Style

    from sevm.tui import theme

    bad = []
    for name in dir(theme):
        if not name.startswith("C_"):
            continue
        value = getattr(theme, name)
        try:
            Style.parse(value)
        except Exception as exc:
            bad.append(f"{name}={value!r} ({exc})")
    assert not bad, "colour constants Textual cannot parse: " + "; ".join(bad)


def test_inline_styles_used_in_panes_parse(bank):
    """The same trap, for styles written inline rather than as constants."""
    from textual.style import Style

    for style in ("on #3a3a3a", "bold red", "dim", "white", "bold white", "dim yellow"):
        Style.parse(style)


def test_memory_dims_zero_words_and_brightens_real_data(bank):
    """A memory dump is mostly zeros; the bytes that hold something must stand out."""
    session, proj_, app, size = tui_app(bank, size=(230, 46))

    async def body(pilot):
        # setNickname writes a string into memory, so there is non-zero data to see.
        app.run_command("b Bank.sol:58")
        await asyncio.sleep(1.0)
        app.run_command("continue")
        await asyncio.sleep(2.5)
        await pilot.pause()
        memory = app.query_one("#memory")
        seen = {"zero": set(), "data": set()}
        for y in range(memory._body.size.height):
            for segment in memory._body.render_line(y)._segments:
                word = segment.text.strip()
                if len(word) != 16 or not all(c in "0123456789abcdef" for c in word):
                    continue
                colour = (
                    segment.style.color.name
                    if segment.style and segment.style.color
                    else None
                )
                seen["zero" if set(word) == {"0"} else "data"].add(colour)
        return seen

    seen = run_tui(session, app, size, body)
    from textual.style import Style

    from sevm.tui.theme import C_MEMORY_TEXT, C_MEMORY_ZERO

    def rendered(style: str) -> str:
        # ANSI names resolve to palette indices by render time, so compare like for like.
        return Style.parse(style).rich_style.color.name

    assert seen["data"], "no non-zero memory words rendered"
    assert seen["zero"], "no zero memory words rendered"
    assert seen["data"] == {rendered(C_MEMORY_TEXT)}
    assert seen["zero"] == {rendered(C_MEMORY_ZERO)}
    assert seen["data"] != seen["zero"], "zeros must not look like real data"


def test_copying_raises_a_toast_and_leaves_the_log_alone(bank):
    """Feedback like "copied 20 characters" is transient, not transcript."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy
        clipboard.copy = lambda text: "pbcopy"
        try:
            await stop_at_credit(app, pilot, proj_)
            before = app.query_one("#log").text
            storage = app.query_one("#storage")
            await pilot.mouse_down(storage, offset=(9, 2))
            await pilot.hover(storage, offset=(30, 2))
            await pilot.mouse_up(storage, offset=(30, 2))
            await pilot.pause()
            toasts = [n.message for n in app._notifications]
            return toasts, before, app.query_one("#log").text
        finally:
            clipboard.copy = monkey

    toasts, before, after = run_tui(session, app, size, body)
    assert toasts, "releasing a drag must raise a toast"
    assert "copied" in toasts[-1] and "pbcopy" in toasts[-1]
    assert after == before, "the copy notice must not land in the transcript"


def test_copy_command_reports_through_a_notice_not_the_log(
    deposit_debugger, fake_clipboard
):
    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    result = dbg.run("copy p owner")
    assert result.ok
    assert result.notice and "copied" in result.notice
    assert result.lines == [], "the notice replaces log output, it does not add to it"


def test_cmd_c_copies_on_macos_and_ctrl_c_still_quits(bank):
    """Cmd+C is `super+c`; both chords copy a live selection."""
    session, proj_, app, size = tui_app(bank)
    copied: list = []

    async def body(pilot):
        from sevm import clipboard

        monkey = clipboard.copy
        clipboard.copy = lambda text: (copied.append(text), "pbcopy")[1]
        try:
            await stop_at_credit(app, pilot, proj_)
            storage = app.query_one("#storage")
            await pilot.mouse_down(storage, offset=(9, 2))
            await pilot.hover(storage, offset=(30, 2))
            await pilot.mouse_up(storage, offset=(30, 2))
            await pilot.pause()
            after_release = len(copied)
            await pilot.press("super+c")
            await pilot.pause()
            after_cmd_c = len(copied)
            app.screen.clear_selection()
            await pilot.pause()
            await pilot.press("super+c")
            await pilot.pause()
            return after_release, after_cmd_c, app.is_running
        finally:
            clipboard.copy = monkey

    after_release, after_cmd_c, running = body and run_tui(session, app, size, body)
    assert after_release == 1, "the drag itself must copy"
    assert after_cmd_c == 2, "cmd+c must copy again"
    assert running, "cmd+c with no selection must not quit"


def test_the_anchor_marker_is_clickable(bank):
    """The marker is the affordance, so it should also be the control."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        source.scroll_to(y=0, animate=False)
        await pilot.pause()
        assert not source.at_anchor
        marker = source.border_subtitle or ""
        # The subtitle carries a click action, not just text.
        assert "@click=jump_to_anchor" in marker
        y = source.region.bottom - 1
        for x in range(source.region.right - 20, source.region.right - 1):
            await pilot.click(offset=(x, y))
            await pilot.pause()
            if source.at_anchor:
                return True, app.focused.id
        return False, app.focused.id

    clicked, focused = run_tui(session, app, size, body)
    assert clicked, "clicking the marker must return the pane to the current state"
    assert focused == "prompt", "clicking a pane must not steal focus from the prompt"


def test_panes_scroll_with_the_wheel_without_taking_focus(bank):
    """Panes are not focusable, so a click cannot send your keystrokes nowhere."""
    from textual.events import MouseScrollDown
    from textual.geometry import Offset

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        moved = {}
        for name in ("#source", "#disasm", "#memory"):
            pane = app.query_one(name)
            before = pane.scroll_offset.y
            for _ in range(4):
                await pilot._post_mouse_events(
                    [MouseScrollDown], offset=Offset(pane.region.x + 3, pane.region.y + 2)
                )
            await pilot.pause()
            moved[name] = (before, pane.scroll_offset.y, pane.can_focus)
        return moved, app.focused.id

    moved, focused = run_tui(session, app, size, body)
    for name, (before, after, can_focus) in moved.items():
        assert after != before, f"{name} did not scroll with the wheel"
        assert not can_focus, f"{name} is focusable and would steal keystrokes"
    assert focused == "prompt"


def test_no_footer_and_no_pane_focus_binding(bank):
    """Both dropped: the key bar was noise, and the wheel replaced F3."""
    from textual.widgets import Footer

    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        return len(app.query(Footer)), {b.action for b in app.BINDINGS}

    footers, actions = run_tui(session, app, size, body)
    assert footers == 0, "the key bar below the prompt is gone"
    assert "focus_pane" not in actions


def test_super_key_bindings_exist_for_mac(bank):
    """Cmd is `super` in Textual; the mac chords are bound alongside the ctrl ones."""
    from sevm.tui.app import SevmApp

    keys = {b.key: b.action for b in SevmApp.BINDINGS}
    assert keys.get("super+c") == "copy_selection"
    assert keys.get("super+p") == "command_palette"
    assert keys.get("super+q") == "quit"
    assert keys.get("super+l") == "clear_log"
    # The ctrl equivalents stay, for everyone else.
    assert keys.get("ctrl+c") == "copy_or_quit"
    assert keys.get("ctrl+q") == "quit"
    assert keys.get("ctrl+l") == "clear_log"


def test_theme_follows_the_terminal(bank):
    """ANSI colours mean the debugger wears whatever palette the terminal is set to."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        from textual.theme import BUILTIN_THEMES

        theme = BUILTIN_THEMES[app.theme]
        return app.theme, theme.background, theme.primary

    name, background, primary = run_tui(session, app, size, body)
    assert name.startswith("ansi-"), f"theme is {name}, not the terminal's"
    assert background == "ansi_default", "the terminal must paint its own background"
    assert primary.startswith("ansi_")


def test_pane_colours_are_ansi_so_they_match_the_terminal():
    from sevm.tui import theme

    accents = {
        name: getattr(theme, name)
        for name in dir(theme)
        if name.startswith("C_") and name != "C_DIM"
    }
    assert accents
    for name, value in accents.items():
        assert "ansi_" in value, f"{name}={value!r} is a fixed colour, not the terminal's"
