"""Turning values into the Rich markup both frontends render.

Escaping is two-tier and the difference matters: `_escape` protects user text spliced into
markup we built, `escape_markup` is for a string that is wholly the user's.
"""

from __future__ import annotations

import re
from collections.abc import Sequence

from rich.markup import escape as rich_escape
from rich.text import Text

_ESCAPE = re.compile(r"\[(/?[a-zA-Z#][^\]]*)\]")


def _addr(raw: bytes | None) -> str:
    if not raw:
        return "0x0"
    return "0x" + bytes(raw).hex()


# Cycled over the argument words; two is enough to show where each one ends.
_ARG_COLOURS = ("magenta", "cyan")
WORD_HEX = 64


def _calldata(raw: bytes | None, limit: int = 128) -> str:
    """Calldata with the selector and each 32-byte argument word coloured apart.

    `limit` counts hex digits of arguments; what is cut is reported, never dropped.
    """
    data = bytes(raw or b"")
    if not data:
        return "[dim]0x (empty)[/dim]"
    out = [f"[bold yellow]0x{data[:4].hex()}[/bold yellow]"]
    args = data[4:].hex()
    for start in range(0, min(len(args), limit), WORD_HEX):
        colour = _ARG_COLOURS[start // WORD_HEX % len(_ARG_COLOURS)]
        word = args[start : min(start + WORD_HEX, limit)]
        out.append(f"[{colour}]{word}[/{colour}]")
    cut = (len(args) - limit) // 2
    if cut > 0:
        out.append(f" [dim]...(+{cut} bytes)[/dim]")
    return "".join(out)


def _short(raw: bytes | None, keep: int = 4) -> str:
    text = _addr(raw)
    if len(text) <= 2 + keep * 2 + 2:
        return text
    return f"{text[: 2 + keep]}..{text[-keep:]}"


def _wei(value: int) -> str:
    if value == 0:
        return "0"
    if 10**15 <= value < 10**27:
        whole, frac = divmod(value, 10**18)
        if frac == 0:
            return f"{value} ({whole} ether)"
        return f"{value} ({value / 10**18:.6f} ether)"
    return str(value)


def describe_amount(text: str) -> str:
    """ "20 characters" or "3 lines", whichever describes the copy better."""
    lines = text.count("\n") + 1
    return f"{lines} lines" if lines > 1 else f"{len(text)} characters"


def _plain(markup: str) -> str:
    """Strip console markup so the clipboard gets text, not `[bold]tags[/bold]`."""
    try:
        return Text.from_markup(markup).plain
    except Exception:
        return markup


def _escape(text: str) -> str:
    """Keep user text and source code from being read as Rich markup."""
    return _ESCAPE.sub(lambda m: "\\[" + m.group(1) + "]", str(text))


def escape_markup(text: str) -> str:
    """Escape *every* bracket, for strings that are wholly the user's.

    `_escape` above is selective because it protects markup we built with user text
    spliced in. An error or notice is the whole string as quoted input, and a stray
    `[/]` in it is a MarkupError waiting to escape a frontend's render loop.
    """
    return rich_escape(str(text))


def _word(value: int) -> str:
    """A 256-bit result, in hex plus decimal while decimal still means anything."""
    text = f"0x{value:x}"
    if value < 2**64:
        return f"{text} ({value:,})"
    return text


def _memory_region(offset: int) -> str:
    """Solidity's fixed memory layout, annotated so beginners can orient themselves."""
    if offset < 0x40:
        return "scratch space"
    if offset < 0x60:
        return "free memory pointer"
    if offset < 0x80:
        return "zero slot"
    return ""


def _event_name(abi: Sequence[dict], topics: Sequence[int]) -> str:
    if not topics:
        return "<anonymous event>"
    from eth_utils import event_abi_to_log_topic

    topic0 = topics[0].to_bytes(32, "big")
    for entry in abi:
        if entry.get("type") != "event":
            continue
        try:
            if event_abi_to_log_topic(entry) == topic0:
                types = ",".join(i["type"] for i in entry.get("inputs", []))
                return f"{entry['name']}({types})"
        except Exception:
            continue
    return f"<unknown event 0x{topics[0]:064x}>"
