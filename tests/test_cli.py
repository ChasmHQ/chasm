"""The `sevm` command line."""

from __future__ import annotations

import os

from tui_harness import strip_ansi


def test_cli_compile_subcommand(capsys):
    from sevm.cli import main

    contracts = os.path.join(os.path.dirname(os.path.abspath(__file__)), "contracts")
    assert main(["compile", contracts]) == 0
    # Rich styles its output when the environment forces colour, which splits words with
    # escape sequences; assert on what the user reads, not on how it is painted.
    out = strip_ansi(capsys.readouterr().out)
    assert "Bank.sol:Bank" in out
    assert "source-map=yes" in out


def test_cli_rejects_a_missing_script(capsys):
    from sevm.cli import main

    assert main(["run", "/nonexistent/script.py"]) == 1
    assert "no such script" in capsys.readouterr().out
