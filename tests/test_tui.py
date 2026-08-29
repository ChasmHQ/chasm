"""A headless render of the fullscreen frontend."""

from __future__ import annotations

import asyncio
import io

from harness import (
    TIMEOUT,
    line_of,
    locals_debugger,
    project,
    stop_at,
)
from tui_harness import run_tui, screen_text, stop_at_credit, tui_app

from sevm.commands import CommandResult
from sevm.evaluate import Evaluator, make_eval_hook
from sevm.session import DebugSession


def test_tui_renders_every_pane(bank):
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    from sevm.tui.app import SevmApp

    session = DebugSession(proj_)
    evaluator = Evaluator(proj_)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.start(txfn)
    first = session.wait(timeout=TIMEOUT)
    app = SevmApp(session, evaluator, first_event=first)

    async def drive():
        async with app.run_test(size=(150, 46)) as pilot:
            await pilot.pause()
            await asyncio.sleep(1.0)
            line = line_of(proj_, "balances[who] += amount - fee;")
            app.run_command(f"b Bank.sol:{line}")
            await asyncio.sleep(1.0)
            app.run_command("continue")
            await asyncio.sleep(2.5)
            await pilot.pause()
            return screen_text(app)

    try:
        screen = asyncio.run(drive())
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()

    for title in (
        "SOURCE",
        "CALL STACK",
        "VARIABLES",
        "STORAGE",
        "DISASSEMBLY",
        "STACK",
        "MEMORY",
    ):
        assert title in screen, f"{title} pane missing"
    assert "Bank._credit" in screen
    assert "*>" in screen, "breakpoint + current-line marker missing from the gutter"
    assert "=>" in screen, "disassembly current-instruction marker missing"
    assert "totalDeposits" in screen
    assert "free mem ptr" in screen or "scratch" in screen
    assert "0x0000:" in screen, "memory must be addressed in the x/g shape"

    # The VARIABLES pane names the frame's locals instead of the old <unavailable> row.
    assert "local" in screen and "fee" in screen
    assert "solc emits no locations" not in screen


def test_tui_startup_commands_run_in_sequence(bank):
    """`-x` startup commands must all run, not just the first.

    Each command executes on an exclusive VM worker that clears `busy` only when it
    finishes, so the startup commands have to be dispatched one at a time. A regression
    here (dispatching them in a loop) drops every command after the first to the `busy`
    guard, leaving the session parked at the constructor instead of inside `deposit`.
    """
    w3, proj_, contract, _callee, alice = bank

    def txfn():
        tx = contract.functions.deposit().transact(
            {"from": alice.address, "value": w3.to_wei(2, "ether"), "gas": 300_000}
        )
        w3.eth.wait_for_transaction_receipt(tx)

    from sevm.tui.app import SevmApp

    session = DebugSession(proj_)
    evaluator = Evaluator(proj_)
    session.set_eval_hook(make_eval_hook(evaluator))
    session.start(txfn)
    first = session.wait(timeout=TIMEOUT)
    app = SevmApp(
        session,
        evaluator,
        first_event=first,
        startup_commands=["tbreak deposit", "continue"],
    )

    def in_deposit() -> bool:
        snap = app.session.last_snapshot
        return (
            snap is not None
            and snap.function is not None
            and (snap.function.display_name == "Bank.deposit")
        )

    async def drive():
        async with app.run_test(size=(150, 46)) as pilot:
            await pilot.pause()
            # Poll rather than sleep a fixed time: the startup `continue` runs on a
            # background worker, so wait for it to land instead of guessing a duration.
            for _ in range(60):
                if in_deposit():
                    break
                await asyncio.sleep(0.1)
            await pilot.pause()
            return app.session.last_snapshot

    try:
        snap = asyncio.run(drive())
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()

    assert snap is not None and snap.function is not None
    assert snap.function.display_name == "Bank.deposit", (
        "startup `continue` did not run: session parked outside deposit"
    )


def test_tui_pane_helpers():
    from sevm.tui.layout import _hex_compact, memory_region
    from sevm.tui.opcodes import operand_count, operand_name

    assert _hex_compact(0xFF) == "0xff"
    assert ".." in _hex_compact(2**255, budget=12)
    assert memory_region(0) == "scratch"
    assert memory_region(0x40) == "free mem ptr"
    assert memory_region(0x100) == ""
    assert operand_count("SSTORE") == 2
    assert operand_name("SSTORE", 0) == "slot"
    assert operand_name("SSTORE", 1) == "value"
    assert operand_name("ADD", 0) == ""


def test_stack_labels_name_the_frames_locals(deposit_debugger):
    from sevm.tui.layout import local_stack_labels

    dbg = deposit_debugger
    stop_at(dbg, 46, "Bank.sol")
    labels = local_stack_labels(dbg.snap)
    named = {name for name, _kind in labels.values()}
    assert {"who", "amount", "fee"} <= named
    # The labels point at the slots that actually hold those values.
    by_name = {name: index for index, (name, _k) in labels.items()}
    assert dbg.snap.stack[by_name["amount"]].value == 2 * 10**18
    assert dbg.snap.stack[by_name["fee"]].value == 5 * 10**15


def test_stack_labels_carry_the_declaration_kind(locals_contract):
    from sevm.tui.layout import local_stack_labels

    dbg = locals_debugger(*locals_contract, "loop", 3)
    try:
        stop_at(dbg, 56)
        kinds = dict(local_stack_labels(dbg.snap).values())
        assert kinds["rounds"] == "param"
        assert kinds["sum"] == "return"
        assert kinds["i"] == "local"
    finally:
        dbg.close()


def test_stack_labels_span_a_multi_word_local(locals_contract):
    """A calldata reference is offset plus length; neither word is anonymous."""
    from sevm.tui.layout import local_stack_labels

    dbg = locals_debugger(*locals_contract, "calldataTypes", b"\xaa\xbb\xcc")
    try:
        stop_at(dbg, 95)
        names = {name for name, _kind in local_stack_labels(dbg.snap).values()}
        assert "payload.ptr" in names and "payload.len" in names
    finally:
        dbg.close()


def test_stack_labels_are_empty_without_locals(deposit_debugger):
    from sevm.tui.layout import local_stack_labels

    assert local_stack_labels(None) == {}


def test_panes_do_not_overflow_their_border(bank):
    """Every pane must clip to its box; DISASSEMBLY used to spill past the bottom."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        from sevm.tui.pane import Pane

        return {
            pane.id: (
                pane.size.height,
                pane.virtual_size.height,
                pane.region.bottom,
                pane.allow_vertical_scroll,
            )
            for pane in app.query(Pane)
            if pane.display
        }

    panes = run_tui(session, app, size, body)
    assert panes, "no panes found"
    for name, (height, virtual, bottom, scrollable) in panes.items():
        assert height > 0, f"{name} has no height"
        assert bottom <= size[1], f"{name} extends past the bottom of the screen"
        # Taller content than the box is the point; it must scroll, not overflow.
        if virtual > height:
            assert scrollable, f"{name} overflows its border instead of scrolling"


def test_low_level_row_fills_to_the_bottom(bank):
    """The low-level panes share the row and reach its bottom edge."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        row = app.query_one("#lowlevel")
        return row.region, {
            name: app.query_one(name).region for name in ("#disasm", "#stack", "#memory")
        }

    row_region, panes = run_tui(session, app, size, body)
    for name, region in panes.items():
        assert region.height == row_region.height, f"{name} does not fill the row"
        assert region.bottom == row_region.bottom, f"{name} stops short of the bottom"


def test_hiding_the_low_level_row_grows_the_source(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        before = source.size.height
        await pilot.press("f2")
        await asyncio.sleep(1.0)
        await pilot.pause()
        return before, source.size.height, app.query_one("#lowlevel").display

    before, after, lowlevel_shown = run_tui(session, app, size, body)
    assert not lowlevel_shown
    assert after > before, f"source stayed {before} lines tall after hiding the row"


def test_panes_scroll_and_flag_when_off_the_current_state(bank):
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        at_stop = (source.at_anchor, source.border_subtitle)
        source.scroll_to(y=0, animate=False)
        await pilot.pause()
        scrolled = (source.at_anchor, source.border_subtitle)
        source.action_jump_to_anchor()
        await pilot.pause()
        return at_stop, scrolled, (source.at_anchor, source.border_subtitle)

    at_stop, scrolled, resynced = run_tui(session, app, size, body)
    assert at_stop[0] is True and not (at_stop[1] or "")
    assert scrolled[0] is False and "back to pc" in scrolled[1], (
        "scrolling away must say so"
    )
    assert resynced[0] is True and not (resynced[1] or "")


def test_source_follows_the_pc_even_after_you_scroll_it(bank):
    """SOURCE and DISASSEMBLY exist to follow execution; stepping re-centres them."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        source.scroll_to(y=0, animate=False)
        await pilot.pause()
        away = (source.scroll_offset.y, source.at_anchor)
        app.run_command("next")
        await asyncio.sleep(2.0)
        await pilot.pause()
        return away, (source.scroll_offset.y, source.at_anchor)

    away, after = run_tui(session, app, size, body)
    assert away[1] is False, "the test must first scroll the pane off the stop"
    assert after[1] is True, "SOURCE must come back to the pc on the next step"
    assert after[0] != away[0]


def test_a_scrolled_pane_stays_put_across_a_step(bank):
    """Scrolling a pane is reading it; stepping must not throw that reading away."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        memory = app.query_one("#memory")
        memory.scroll_to(y=3, animate=False)
        await pilot.pause()
        away = memory.scroll_offset.y
        app.run_command("next")
        await asyncio.sleep(2.0)
        await pilot.pause()
        stayed = memory.scroll_offset.y
        # Scrolling back onto the current state opts back into following it.
        memory.scroll_to(y=0, animate=False)
        await pilot.pause()
        app.run_command("next")
        await asyncio.sleep(2.0)
        await pilot.pause()
        return away, stayed, memory.scroll_offset.y

    away, stayed, following = run_tui(session, app, size, body)
    assert away > 0, "the test must first scroll MEMORY away from the top"
    assert stayed == away, f"MEMORY jumped from {away} back to {stayed} on `next`"
    assert following == 0


def test_the_log_stops_following_while_you_read_back(bank):
    """New output must not yank the log to the end while you are reading scrollback."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        log = app.query_one("#log")
        for n in range(60):
            log.write(f"line {n}")
        await pilot.pause()
        at_end = log.scroll_offset.y
        log.scroll_to(y=at_end - 5, animate=False)
        await pilot.pause()
        away = log.scroll_offset.y
        log.write("more output")
        await pilot.pause()
        held = log.scroll_offset.y
        log.action_jump_to_anchor()
        await pilot.pause()
        return at_end, away, held, log.scroll_offset.y, log.max_scroll_y

    at_end, away, held, resynced, end = run_tui(session, app, size, body)
    assert at_end > 0 and away == at_end - 5
    assert held == away, "writing to the log must not move a log you scrolled back"
    assert resynced == end, "the marker must put the log back on the newest line"


def test_source_pane_holds_the_whole_file(bank):
    """Scrolling is only useful if there is something above and below the stop."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        source = app.query_one("#source")
        return source.virtual_size.height, source.size.height

    rendered, visible = run_tui(session, app, size, body)
    total_lines = len(project().sources["Bank.sol"].text.split("\n"))
    assert rendered >= total_lines - 1, f"only {rendered} of {total_lines} lines rendered"
    assert rendered > visible


def test_history_recall_puts_the_cursor_at_the_end(bank):
    """Arrow-up must leave you ready to edit, not typing in front of the command."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        app.run_command("info storage")
        await asyncio.sleep(1.0)
        from textual.widgets import Input

        prompt = app.query_one("#prompt", Input)
        prompt.focus()
        app._input_history = ["info storage", "p totalDeposits"]
        app._history_pos = 2
        app.action_history_prev()
        await pilot.pause()
        recalled = (prompt.value, prompt.cursor_position)
        app.action_history_prev()
        await pilot.pause()
        return recalled, (prompt.value, prompt.cursor_position)

    first, second = run_tui(session, app, size, body)
    assert first == ("p totalDeposits", len("p totalDeposits"))
    assert second == ("info storage", len("info storage"))


def test_memory_pane_renders_giant_words(bank):
    session, proj_, app, size = tui_app(bank, size=(230, 46))

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        return screen_text(app)

    text = run_tui(session, app, size, body)
    assert "0x0000:" in text and "0x0010:" in text
    # 8-byte giants, two per row at this width, and the layout is still labelled.
    assert "0000000000000000 0000000000000000" in text
    assert "scratch" in text
    assert "free mem ptr" in text


def test_memory_preview_is_never_cut_short(bank):
    """The ASCII column shows every byte of its row or the pane shows fewer bytes.

    A preview ellipsised mid-string is worse than a narrower row: `flag{hereyoug...`
    reads as the whole value and hides where the string actually ends.
    """
    session, proj_, app, size = tui_app(bank, size=(230, 46))

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        app.run_command('mstore(0x50, "flag{hereyougo}")')
        await asyncio.sleep(1.5)
        await pilot.pause()
        return screen_text(app)

    text = run_tui(session, app, size, body)
    row = next(line for line in text.splitlines() if "0x0050:" in line)
    assert "666c61677b686572 65796f75676f7d00" in row
    assert "flag{hereyougo}" in row


def test_mouse_is_on_by_default_and_can_be_turned_off():
    """Panes render Content, which Textual can select, so the mouse is worth having."""
    from sevm.cli import build_parser

    args = build_parser().parse_args(["run", "--contracts", "x", "script.py"])
    assert args.no_mouse is False
    args = build_parser().parse_args(
        ["run", "--no-mouse", "--contracts", "x", "script.py"]
    )
    assert args.no_mouse is True


def test_every_mutation_refreshes_the_snapshot(deposit_debugger):
    """The snapshot is a copy, so a write is invisible to the panes until it is re-read."""
    dbg = deposit_debugger
    assert dbg.run("set $stack[0] = 0xc0ffee").ok
    assert dbg.snap.stack[0].value == 0xC0FFEE
    assert dbg.run("set $mem[0x80] = 0xabc").ok
    assert int.from_bytes(dbg.snap.memory[0x80:0xA0], "big") == 0xABC
    assert dbg.run("set $gas = 90000").ok
    assert dbg.snap.gas_remaining == 90000


def test_mutating_commands_are_flagged_and_reads_are_not(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("mstore(0x80, 1)").mutated
    assert dbg.run("set var totalDeposits = 5").mutated
    assert dbg.run("set $storage[1] = 5").mutated
    assert dbg.run("vm.warp(1735689600)").mutated
    assert not dbg.run("p totalDeposits").mutated
    assert not dbg.run("info registers").mutated


def test_tui_panes_show_an_assembly_write_without_stepping(bank):
    """The MEMORY and STORAGE panes must repaint the moment `mstore`/`sstore` lands."""
    session, proj_, app, size = tui_app(bank)

    async def body(pilot):
        await stop_at_credit(app, pilot, proj_)
        memory = app.query_one("#memory")
        storage = app.query_one("#storage")
        before = (memory.text, storage.text)
        app.run_command("asm mstore(0x80, 0xdeadbeef); sstore(1, 0xc0ffee)")
        await asyncio.sleep(2.0)
        await pilot.pause()
        return before, (memory.text, storage.text)

    before, after = run_tui(session, app, size, body)
    assert "deadbeef" in after[0], "MEMORY pane did not pick up the mstore"
    assert after[0] != before[0]
    assert "c0ffee" in after[1].lower() or after[1] != before[1], (
        "STORAGE pane did not pick up the sstore"
    )


def test_console_escapes_error_text_before_rendering(deposit_debugger):
    """An error quoting the user's own brackets must not be eaten as Rich markup.

    `[nope]` used to be swallowed as a style tag, and an unmatched `[/...]` raised
    MarkupError straight out of the console's read loop, ending the session over a typo.
    """
    from rich.console import Console

    from sevm.console import ConsoleFrontend

    frontend = ConsoleFrontend(deposit_debugger.session, deposit_debugger.evaluator)
    frontend.console = Console(file=io.StringIO(), width=200, highlight=False)
    for error in ("bad [nope] thing", "x [/] y", "closing [/nope] tag"):
        frontend._emit(CommandResult(error=error))
    written = frontend.console.file.getvalue()
    for fragment in ("[nope]", "[/]", "[/nope]"):
        assert fragment in written, f"{fragment} was eaten by the markup parser"


def test_console_survives_a_bad_command_end_to_end(deposit_debugger):
    """The whole path a typo takes: execute -> CommandResult -> console render."""
    from rich.console import Console

    from sevm.console import ConsoleFrontend

    frontend = ConsoleFrontend(deposit_debugger.session, deposit_debugger.evaluator)
    frontend.console = Console(file=io.StringIO(), width=200, highlight=False)
    for line in ("blah", "p balances[nope]", "mstore(0x80)", "delete xyz"):
        result = frontend.commands.execute(line)
        assert not result.ok, line
        frontend._emit(result)
    assert "error:" in frontend.console.file.getvalue()
