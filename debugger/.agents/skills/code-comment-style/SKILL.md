---
name: code-comment-style
description: Write and review code comments, docstrings, CLI help strings, and in-app help text in a terse senior-engineer style. Use when writing new code, reviewing existing comments, or asked to clean up/tighten comments or command help.
metadata:
  trigger: Writing or reviewing code comments, docstrings, argparse/CLI help text, in-app help output
---

# Code comment and help-text style

Comments and help text exist to carry the non-obvious "why": an invariant, a gotcha, a
workaround, a constraint that would otherwise cost someone a debugging session. They are
not a place to restate what the code already says.

## Rules

1. **State the why, not the what.** Well-named code already says what it does. A comment
   earns its place only when removing it would leave a future reader confused about a
   hidden constraint or surprising behavior.

2. **As short as the fact allows.** One line, almost always. Two if the invariant is
   genuinely dense. Three is the ceiling — if a comment needs more, the fact is probably
   better expressed as a short comment plus a link/reference, or the code needs
   restructuring, not more prose.

3. **No essay style.** Cut scene-setting, rhetorical questions, dramatic phrasing
   ("is the thing a trace cannot give you"), restating the same point twice in different
   words, and quotable one-liners. Say the fact once, plainly.

4. **Leave terse comments alone.** A field annotation (`kind: str  # storage | memory`),
   a section divider (`# -- lookup --`), or a one-line docstring is already correct.
   Don't touch it, don't expand it, don't add a comment near it that repeats it.

5. **No comments on the obvious.** Don't add a comment explaining a call, an assignment,
   or a loop when the code already reads clearly. This applies to new code too, not just
   cleanup: default to writing no comment.

6. **Same discipline for help text.** argparse `help=` strings, in-app `help` output,
   `--help` epilogs: say what's needed to use the thing, then stop. Prefer a compact
   table of examples over a paragraph of explanation. A help topic is reference material,
   not documentation prose.

## Calibration example

BEFORE:
```
# `eth_estimateGas` binary-searches the gas limit by *running the transaction* many times,
# starting from the intrinsic gas, so the first probes fail with OutOfGas by design. Left
# untouched, the debugger stops inside those probes and the user sees a bogus out-of-gas
# in a transaction that succeeds. Estimation therefore runs with the hook suspended.
```

AFTER:
```
# eth_estimateGas binary-searches the gas limit by re-running the tx, so early probes fail
# with OutOfGas by design. Run estimation with the hook suspended, or the debugger stops on
# a bogus failure mid-search.
```

BEFORE:
```
# One accent per data class, in ANSI colours so the debugger wears your terminal's own
# palette: a solarized or gruvbox terminal gets a solarized or gruvbox sevm, and the
# background is whatever your terminal already draws.
```

AFTER:
```
# One accent per data class, in ANSI colours, so the debugger follows the terminal's own palette.
```

## When reviewing/cleaning up existing comments

This is a compression pass, not a deletion pass. Keep every load-bearing fact; cut the
prose around it. Never remove a comment that states a real invariant just to hit a line
count, and never leave a comment that becomes misleading after trimming. Comments that
are already terse and factual need no change — don't churn a diff by rewording something
that was already fine.
