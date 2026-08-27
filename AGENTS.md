# AGENTS.md — sevm

Context for any agent working in this directory. Read before editing.

## What this is

`sevm` is a fullscreen, gdb-compatible interactive debugger for Solidity, running on
Py-EVM. It stops *inside* a running transaction with the frame still alive: read
uncommitted state, call view functions, rewrite a stack operand before the opcode
consumes it, mutate storage through Solidity, and force out-of-gas at an exact
instruction. See `README.md` for user docs and `PLAN.md` for the design and its
feasibility research.

This is a self-contained, uv-managed Python project. It grew out of the article demo in
the parent directory (the Py-EVM `apply_computation` monkeypatch trick) but stands on its
own here.

## Layout (src layout)

```
sevm/
├── pyproject.toml        # metadata, deps, `sevm` entry point, pytest + hatch config
├── uv.lock               # pinned resolution
├── README.md, PLAN.md    # user docs, design
├── src/sevm/             # the package (import as `sevm`)
│   ├── cli.py            # arg parsing + `main()`; `.py` vs `.sol` dispatch in `sevm run`
│   ├── compile.py        # solc via py-solc-x → artifacts; `compile_foundry_project`
│   ├── session.py        # the debug session / VM control (largest module)
│   ├── cheatcodes.py     # Foundry cheatcode engine (VM/console intercept, registry)
│   ├── assembly.py       # Yul/inline-assembly parser + executor over the live frame
│   ├── foundry.py        # Foundry test runner: resolve project, discover, driver
│   ├── commands.py       # gdb-style command dispatch (+ `vm.*` cheats, Yul at the prompt)
│   ├── evaluate.py       # Solidity expression evaluation
│   ├── frames.py, srcmap.py, locals.py, decode.py, disasm.py, breakpoints.py
│   ├── console.py        # plain-text frontend (`--console`)
│   ├── vendor/forge-std/ # bundled minimal forge-std (Vm/Test/console.sol), package data
│   └── tui/              # Textual fullscreen frontend (app.py, widgets.py, sevm.tcss)
├── tests/                # test_sevm.py + test_foundry.py + harness.py + fixtures/
└── examples/debug_bank.py
```

`sevm run` dispatches by extension: `.py` attaches to a web3 driver (below); `.sol`
compiles + runs a Foundry test. With no `-m` filter it debugs every test in the file: a
fresh deploy + `setUp` + test per function, a breakpoint on each body, opening at the first
and stepping to each on `continue`. Function/line breakpoints are contract-scoped
(`Breakpoint.contract`), so a shared pc in another same-file contract (e.g. a helper's
creation code, where the running artifact is unrecognised) does not mis-fire them.
Cheatcodes are intercepted in the patched loop right after the precompile check (calls to
the VM / console addresses); `session.foundry_mode` etches a byte at those addresses so
Solidity's `extcodesize` guard on `vm.*` calls does not revert before dispatch. A prank is
applied at the calling opcode (`_exec_opcode` swaps the caller's `storage_address`), so
value, gas and `msg.sender` all follow it as in forge. See README "Foundry tests and
cheatcodes" for the user-facing surface and v1 limits.

A Yul builtin typed at the prompt (`mstore(0x80, 1)`, or the explicit `asm ...`) is parsed
by `assembly.py` and executed by Py-EVM's own opcode functions against the paused
computation: arguments pushed in EVM order, `opcode_fn(computation=...)` called, result
read off the top, then the stack restored by slice-assignment (never rebinding
`Stack.values`, whose `append`/`pop` are cached bound to that list object). Gas is metered,
reported and then handed back; memory expansion is kept. Yul's own exclusions (`jump`,
`pc`, `push*`, `dup*`, `swap*`) plus the frame terminators are refused with a reason.

`help assembly` and `help cheatcodes` are generated at import time from the builtin table
and the cheat registry, so adding a builtin, or a `@_cheat(..., doc="...")`, documents
itself. A cheat left without a `doc` fails the suite rather than quietly vanishing from
the help.

Any command that writes to the VM sets `CommandResult.mutated`, and `execute()` then calls
`DebugSession.refresh_snapshot()`. A `FrameSnapshot` is a copy taken at the pause, so
without that step a write to memory, the stack or a local is invisible to the panes until
the next stop. `_live_view` is the single source of the mutable fields, shared by
`_build_snapshot` and the `resnapshot` inspect op so the two cannot drift.

`CommandProcessor.execute()` must never raise: the console frontend calls it from its read
loop, and the TUI calls it from the worker thread that owns `busy`, where an escaping
exception wedges the prompt for the rest of the run. Everything after the initial strip
lives inside `_dispatch`, behind that one error boundary. The same applies one layer out:
`result.error` and `result.notice` are wholly user text, so both frontends run them
through `escape_markup` before rendering, or an unmatched `[/...]` raises a Rich
MarkupError out of the render itself.

The package uses relative imports internally (`from .compile import ...`). Keep it that
way; do not add `sys.path` hacks.

## Environment and commands

Everything runs through uv. Do not call bare `pip`/`python` against a system interpreter.

```bash
uv sync                 # editable install of sevm + dev group into .venv
uv run sevm --help      # run the CLI
uv run sevm run --contracts tests/contracts examples/debug_bank.py   # fullscreen TUI
uv run sevm run --console --contracts tests/contracts examples/debug_bank.py
uv run sevm compile tests/contracts                                  # what sevm sees
uv run pytest -q        # test suite (219 tests; ~2 min, solc compile is the slow part)
uv run ruff check src tests examples   # lint (config in pyproject [tool.ruff])
uv run ruff format src tests examples  # format (line length 90)
uv run mypy src         # type check (pragmatic config, not strict)
uv build                # wheel + sdist
uv tool install .       # install the `sevm` command globally
```

Style is enforced by ruff, not by hand: run `ruff format` and `ruff check --fix` before
committing. `src/` is expected to be lint-clean. Line length is 90 and owned by the
formatter (`E501` is ignored); two rules are deliberately off (`RUF012` fights Textual's
`BINDINGS`/`CSS` class idiom, `RUF059` is relaxed in `tests/` where unpacking a shared
fixture tuple and using only some fields is normal). Every module carries
`from __future__ import annotations`, so annotations use builtin generics (`dict`,
`list`, `X | None`), not `typing.Dict`/`Optional`.

CLI shape: `sevm {run,compile}`. Options go **before** the target script; everything
after the script is forwarded to it. Recognition of user contracts is by bytecode
(runtime code matched against compiled artifacts, metadata hash stripped, immutables
masked), so an unmodified web3 script works as a `run` target.

## Dependencies

Declared in `pyproject.toml`. Rule: every third-party package imported under `src/sevm`
is a direct dependency, not left to transitive resolution. `eth` (py-evm), `eth_abi`,
`eth_utils` and `eth_account` ship with `web3[tester]` but are imported directly, so they
are named explicitly. Dev-only tools (`pytest`, `textual-dev`) live in the PEP 735
`[dependency-groups] dev`, not in the runtime deps, and are not shipped in the wheel.

The Textual stylesheet `src/sevm/tui/sevm.tcss` is package data loaded relative to
`app.py`; hatchling includes it in the wheel automatically. If you move or rename it,
update `CSS_PATH` in `tui/app.py` and re-check `uv build` still bundles it. The bundled
forge-std under `src/sevm/vendor/forge-std/src/*.sol` is package data the same way (located
at runtime via `BUNDLED_FORGE_STD_SRC` in `compile.py`); `uv build` bundles it too.

solc is fetched on demand by py-solc-x into `~/.solcx` (not vendored). Tests and examples
use solc 0.8.28.

## Tests

`tests/` is a parallel test dir. `harness.py` compiles `tests/contracts/` once per process
and deploys over an in-process Py-EVM chain; `test_sevm.py` imports fixtures with
`from harness import ...`. This works because `pyproject.toml` sets
`[tool.pytest.ini_options] pythonpath = ["tests"]`; the `sevm` package itself is imported
from the editable install. Fixtures are self-contained under `tests/contracts/`
(`Bank.sol`, `Locals.sol`, `Vault.sol`) — do not reach outside the project for a contract.

`test_foundry.py` covers the Foundry path and the cheatcode engine. Its fixtures are two
layouts: `tests/foundry_solo/` (a standalone `.t.sol` that leans on the bundled forge-std,
no `lib/`) and `tests/foundry_project/` (a real project with `foundry.toml`,
`remappings.txt`, `src/`, `test/`, and its own vendored `lib/forge-std/src/` to prove the
project's lib wins over the bundled one). Cheats are asserted end to end by running each
test to completion and requiring no revert (the test's own `assertEq`s prove the effect).

## Invariants — do NOT reintroduce these bugs (each cost a debugging session; detail in PLAN.md §12)

1. The `next` exemption for a function entering its own body.
2. Inject eval code at the artifact's own `source_range`, not an arbitrary offset.
3. Probe expression types with an unreachable struct, not `uint256`.
4. Choose the snapped breakpoint line across *all* artifacts before collecting pcs.
5. Truncate TUI cells with Rich `no_wrap` columns, not by hand.
6. Run the unknown-verb fallback (`_not_a_command`) inside `execute()`'s error boundary.

Underlying Py-EVM monkeypatch gotchas (inherited from the tracer, still apply): restore
the raw classmethod descriptor not the bound method; stack items are `int` or `bytes`;
pass explicit `gas=` so web3 does not re-run the tx during estimation.

## Verified environment

web3 7.16.0, py-evm 0.12.1b1, eth-tester 0.13.0b1, py-solc-x 2.0.5, solc 0.8.28,
CPython 3.12. `requires-python = ">=3.10"`. All 219 tests pass as of 2026-08-26 (incl. Foundry multi-test + cheatcode coverage, the
Yul assembly surface, and the snapshot refresh after a mutation).
