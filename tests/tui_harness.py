"""Driving the Textual frontend headlessly.

A TUI test stands up the real app over a real session and drives it through Textual's
pilot. `stop_at_credit` is the shared starting point: most assertions want the app parked
mid-deposit with every pane populated.
"""

from __future__ import annotations

import asyncio
import re

from harness import TIMEOUT, line_of

from sevm.evaluate import Evaluator, make_eval_hook
from sevm.session import DebugSession

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def strip_ansi(text: str) -> str:
    return _ANSI_RE.sub("", text)


def screen_text(app) -> str:
    """Flatten the composited screen to plain text for assertions."""
    return "\n".join(
        "".join(segment.text for segment in strip)
        for strip in app.screen._compositor.render_strips()
    )


def tui_app(bank, size=(150, 46)):
    """A started TUI over a Bank deposit, paused at the interesting line."""
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
    return session, proj_, SevmApp(session, evaluator, first_event=first), size


def run_tui(session, app, size, body):
    """Drive `body(pilot)` against a running app and tear the session down."""

    async def drive():
        # Notifications are off by default in `run_test`, and toasts are part of the UI.
        async with app.run_test(size=size, notifications=True) as pilot:
            await pilot.pause()
            await asyncio.sleep(1.0)
            return await body(pilot)

    try:
        return asyncio.run(drive())
    finally:
        try:
            session.detach(timeout=TIMEOUT)
        except Exception:
            session.uninstall()


async def stop_at_credit(app, pilot, proj_):
    line = line_of(proj_, "balances[who] += amount - fee;")
    app.run_command(f"b Bank.sol:{line}")
    await asyncio.sleep(1.0)
    app.run_command("continue")
    await asyncio.sleep(2.5)
    await pilot.pause()
    return line
