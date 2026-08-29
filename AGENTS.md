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

Every package `__init__.py` carries a map of its own modules, and `src/sevm/__init__.py`
maps the whole tree. Read those first; `tests/test_layout.py` fails if one goes stale.

```
sevm/
├── pyproject.toml        # metadata, deps, `sevm` entry point, pytest + hatch config
├── uv.lock               # pinned resolution
├── README.md, PLAN.md    # landing page, design
├── docs/                 # commands, expressions, assembly, Foundry reference
├── src/sevm/             # the package (import as `sevm`)
│   ├── cli.py            # arg parsing + `main()`; `.py` vs `.sol` dispatch in `sevm run`
│   ├── session/          # the stepping engine: core, patch, stepping, snapshots,
│   │                     #   framelocals, inspect_ops, code, events
│   ├── commands/         # the gdb command layer: processor + one module per verb group
│   ├── compile/          # model, solc, versions, foundry_config, build
│   ├── evaluate/         # bindings, injection, evaluator
│   ├── assembly/         # builtins, parser, execute
│   ├── cheatcodes/       # registry, cheats, assertions, args, console
│   ├── locals/           # layout, index, values
│   ├── tui/              # app, pane, panes, layout, opcodes, theme, sevm.tcss
│   ├── cache.py          # on-disk build cache: unit hashing, partial rebuilds
│   ├── artifacts.py      # forge-shaped `out/sevm/<File.sol>/<Contract>.json`
│   ├── libs.py           # dependency resolution: imports -> repo -> clone -> remapping
│   ├── foundry.py        # Foundry test runner: resolve project, discover, driver
│   ├── frames.py, srcmap.py, decode.py, disasm.py, breakpoints.py, clipboard.py
│   └── console.py        # plain-text frontend (`--console`)
├── tests/                # one file per layer + conftest/harness/tui_harness
└── examples/             # runnable, self-contained projects
```

`sevm run` dispatches by extension: `.py` attaches to a web3 driver (below); `.sol`
compiles + runs a Foundry test. Both paths resolve dependencies the same way and both run
with `foundry_mode` on. With no `-m` filter it debugs every test in the file: a
fresh deploy + `setUp` + test per function, a breakpoint on each body, opening at the first
and stepping to each on `continue`. A test transaction that reverts fails the run
(`foundry.TestFailed`); eth-tester reports it as `status = 0` rather than raising, so
without that check a failing assertion looks like a passing test. Function/line breakpoints
are contract-scoped (`Breakpoint.contract`), so a shared pc in another same-file contract
(e.g. a helper's creation code, where the running artifact is unrecognised) does not
mis-fire them.

Cheatcodes are intercepted in the patched loop right after the precompile check (calls to
the VM / console addresses); `session.foundry_mode` etches a byte at those addresses so
Solidity's `extcodesize` guard on `vm.*` calls does not revert before dispatch. A prank is
applied at the calling opcode (`exec_opcode` swaps the caller's `storage_address`), so
value, gas and `msg.sender` all follow it as in forge. A `delegateCall` prank swaps the
caller's `msg.sender` as well, because that is where a delegated frame's sender comes
from. See README "Foundry tests and cheatcodes" for the user-facing surface and v1 limits.

## Library resolution (libs.py)

sevm ships no forge-std. `compile_foundry_project` walks the import graph
(`libs.import_closure`), and anything unresolved is installed: prefix -> `libs.ALIASES`,
else npm registry metadata (`repository.url`) -> `git clone --depth 1 --branch <newest
stable tag>` into `<root>/lib/<name>`, with the remapping derived from where the imported
file landed inside the clone (so `src/`, `contracts/` and flat layouts all work). New
remappings are appended to `remappings.txt` so `forge` resolves the project identically.
git is required; the `forge` binary is never invoked. `--no-install` resolves from disk
only. `foundry.prepare_project` gates every write behind one prompt (`-y` skips it,
non-tty declines) and writes `STANDALONE_FOUNDRY_TOML` (no `src`/`test` keys) for a lone
`.sol`, `DEFAULT_FOUNDRY_TOML` when the root already has `src/` + `test/`. Without a
foundry.toml the root is the target's own directory, unless a directory a few levels up
holds `src/` + `test/` (`_infer_root`), so `test/Foo.t.sol` in an un-configured project
does not get `test/lib/forge-std`.

Only the project's own sources are collected by directory walk; library sources enter
solely through the import closure. Real forge-std's `assertEq` and friends delegate to
`vm.assertEq`/`assertGt`/`assertApproxEqAbs` (116 overloads in its `Vm.sol`), all
registered programmatically in `cheatcodes/assertions.py` from an op x type matrix, sharing a
`family` so `help cheatcodes` prints one row instead of 116. They implement the comparison
for real: the `*Decimal` and `*ApproxEq*` forms are called even when the assertion holds.

A Yul builtin typed at the prompt (`mstore(0x80, 1)`, or the explicit `asm ...`) is parsed
by `assembly/parser.py` and executed by Py-EVM's own opcode functions against the paused
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

Panes re-centre on their anchor at every stop, but only until you scroll one by hand:
`Pane.watch_scroll_y` sets `_user_scrolled` for any move the pane did not make itself
(`_scroll_ourselves` guards ours), and `anchor_at` then leaves that pane alone. Landing
back on `anchor_target()` clears the flag, which is also how the clickable border marker
works. `FOLLOWS_PC` exempts SOURCE and DISASSEMBLY, whose job is to follow execution.

`CommandProcessor.execute()` must never raise: the console frontend calls it from its read
loop, and the TUI calls it from the worker thread that owns `busy`, where an escaping
exception wedges the prompt for the rest of the run. Everything after the initial strip
lives inside `_dispatch`, behind that one error boundary. The same applies one layer out:
`result.error` and `result.notice` are wholly user text, so both frontends run them
through `escape_markup` before rendering, or an unmatched `[/...]` raises a Rich
MarkupError out of the render itself.

The package uses relative imports internally (`from .compile import ...`). Keep it that
way; do not add `sys.path` hacks.

## Build cache (cache.py)

`compile_foundry_project` caches per compilation unit: sha256 over the schema constant, the
resolved solc version, `optimize`, `evm_version`, the remappings, the output selection and
every source's content hash. A hit loads `<cache>/<unit>.json.gz` (solc's standard-JSON
output, `errors` stripped) and goes straight to `_build_project` without invoking solc. The
cache is `cache/sevm/` inside a Foundry root, else `~/.cache/sevm/projects/<hash>`, so a
plain directory is never written into.

A miss with a usable base (same settings, same source *key set*) builds partially: solc
still receives every source, only `outputSelection` narrows to the dirty files, which are
the edited ones closed over their importers (`libs.Closure.edges`, reversed by
`cache.dependents`). Emitting the ASTs of all sources is ~0.9s of a 1.0s compile on a
40-source project, so the narrowed request is where the win is. Because the source set solc
sees is unchanged, file ids, analysis and source maps come out identical to a full build;
`merge_output` still re-checks every reused id and refuses the merge if one moved, and
`base_for` requires an identical key set because adding or removing a file shifts them.

A build also leaves forge-shaped artifacts (`artifacts.write_out`): one JSON per contract
under `<out>/sevm/<File.sol>/<Contract>.json`, `out` taken from foundry.toml. The `sevm/`
nesting is load-bearing, not tidiness: sevm compiles optimizer-off, and overwriting
`out/<File.sol>/<Contract>.json` would leave forge's own cache calling it fresh, so the
next `forge test` would run sevm's build. Contract names that collide across sources with
the same basename get a `.1` suffix, and `sourceName` says which source each came from.
Artifacts are written when a build happens, or when a hit finds the `out` tree gone; they
carry no `metadata`, which is the one forge field sevm does not request from solc.

Every cache failure is a miss, never an error. Tests must never touch `~/.cache`:
`conftest.isolated_cache` points `XDG_CACHE_HOME` at tmp per test, which is also what lets
the version-resolution tests' mocked release list beat the real one cached on disk.

## Git workflow

Commit after every change, no matter how small. Do not batch multiple unrelated edits
into one commit and do not wait for the user to ask. Each commit should be a single
coherent change with a message describing why. This overrides the general default of
only committing when explicitly asked; in this repo, committing after each change *is*
the standing instruction.

## Environment and commands

Everything runs through uv. Do not call bare `pip`/`python` against a system interpreter.

```bash
uv sync                 # editable install of sevm + dev group into .venv
uv run sevm --help      # run the CLI
uv run sevm run --contracts tests/contracts examples/debug_bank.py   # fullscreen TUI
uv run sevm run --console --contracts tests/contracts examples/debug_bank.py
uv run sevm compile tests/contracts                                  # what sevm sees
uv run pytest -q        # test suite (702 tests; ~2.5 min, solc compile is the slow part)
SEVM_NETWORK_TESTS=1 uv run pytest -q -m network   # 4 more, against the real forge-std/npm
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

## Comment and help-text style

Use the `code-comment-style` skill before writing or reviewing code comments,
docstrings, argparse `help=` strings, or the in-app `help`/`help <topic>` text in
`commands/help.py`. It covers writing new code (default to no comment) as well as cleanup
passes (compress, don't delete load-bearing facts).

## Splitting a module

Use the `module-splitting` skill before breaking up a large file or reorganizing the
package tree. It carries the failure modes this repo has already paid for: deferred
relative imports that only fail at run time, decorators dropped by a naive slice, locals
that shadow a newly imported module, and monkeypatch targets that stop taking effect. It
also prefers explicit collaborators over mixins, which is why `session/` passes the session
in rather than inheriting.

## README and docs style

Use the `readme-writing` skill before creating or editing `README.md`, anything under
`docs/`, or `examples/README.md`. The rule that matters most here: every command must name
a path that ships in `examples/`, and every transcript must be pasted from a real run
(`COLUMNS=110` keeps blocks from wrapping), never hand-edited. Regenerating the transcripts
is also how doc drift gets caught, so treat a mismatch between a run and the prose as a
finding, not something to write around. Terminal blocks are fenced ```bash; bare fences are
for directory trees only.

## Dependencies

Declared in `pyproject.toml`. Rule: every third-party package imported under `src/sevm`
is a direct dependency, not left to transitive resolution. `eth` (py-evm), `eth_abi`,
`eth_utils` and `eth_account` ship with `web3[tester]` but are imported directly, so they
are named explicitly. Dev-only tools (`pytest`, `textual-dev`) live in the PEP 735
`[dependency-groups] dev`, not in the runtime deps, and are not shipped in the wheel.

The Textual stylesheet `src/sevm/tui/sevm.tcss` is package data loaded relative to
`app.py`; hatchling includes it in the wheel automatically. If you move or rename it,
update `CSS_PATH` in `tui/app.py` and re-check `uv build` still bundles it. It is the only
package data left: forge-std is no longer vendored.

solc is fetched on demand by py-solc-x into `~/.solcx` (not vendored). Tests and examples
use solc 0.8.28.

## Tests

`tests/` is a parallel test dir, one file per layer of `src/` (`test_stepping`,
`test_breakpoints`, `test_commands`, `test_locals`, `test_tui`, ...). `harness.py` compiles
`tests/contracts/` once per process, deploys over an in-process Py-EVM chain, and owns the
`Debugger` helper; `conftest.py` holds the fixtures every file shares and `tui_harness.py`
the Textual pilot helpers. Test files import them with `from harness import ...`, which
works because `pyproject.toml` sets
`[tool.pytest.ini_options] pythonpath = ["tests"]`; the `sevm` package itself is imported
from the editable install. Fixtures are self-contained under `tests/contracts/`
(`Bank.sol`, `Locals.sol`, `Vault.sol`) — do not reach outside the project for a contract.

`test_cheatcodes.py` covers the cheat registry itself. Every registered signature is
exercised once and `test_every_cheat_is_exercised` fails if one is added without a case.
The expected values are not invented, each was read off a run of the same call under real
`forge test`, so the file doubles as sevm's differential record of Foundry behaviour. The
generated `vm.assert*` overloads are driven from the signature rather than a hand-written
table, which is the only way 116 of them stay covered. Cheats that touch VM state are
proven a second time end to end in `test_foundry.py`, where they reach a real Py-EVM
transaction, which is where the prank and fee-settlement bugs lived. `test_foundry.py`
also covers the Foundry path itself; `test_libs*.py` cover
dependency resolution, the compile pipeline around it, and the network-only equivalents;
`test_cache.py` covers the build cache, and proves a partial build is field-for-field the
same Project as a full one. `test_layout.py` guards the package tree itself: every module
imports, every relative import names a real sibling (a deferred `from .x import y` inside a
function body survives its module moving a level deeper and fails only at run time), and
every package `__init__` still maps its own modules. The suite never touches
the network: `conftest.py`
builds a git repo from `tests/fixtures/forge_std_fake/` (forge-std-shaped, assertions
delegating to `vm.assert*` like the real thing, plus a `test/` tree with an unlinked
library that must never reach solc) and points `libs.ALIASES["forge-std"]` at it over
`file://`, so the real clone/tag-selection/remapping code runs offline. Fixture layouts:
`tests/foundry_solo/` (standalone `.t.sol`, no `lib/`, drives the install path) and
`tests/foundry_project/` (a project with `foundry.toml`, `remappings.txt`, `src/`, `test/`
and its own `lib/forge-std/src/`, which must be used untouched). Both are copied into
`tmp_path` before use so an install never writes into the repo. Cheats are asserted end to
end by running each test to completion (the test's own `assertEq`s prove the effect).

Tests marked `network` reach the real forge-std, npm and openzeppelin repositories, and
are skipped unless `SEVM_NETWORK_TESTS=1`. One of them asserts sevm implements every
`assert*` the current forge-std declares, so a new overload upstream fails the suite
rather than surfacing as "unimplemented cheatcode" at run time.

## Invariants — do NOT reintroduce these bugs (each cost a debugging session; detail in PLAN.md §12)

1. The `next` exemption for a function entering its own body.
2. Inject eval code at the artifact's own `source_range`, not an arbitrary offset.
3. Probe expression types with an unreachable struct, not `uint256`.
4. Choose the snapped breakpoint line across *all* artifacts before collecting pcs.
5. Truncate TUI cells with Rich `no_wrap` columns, not by hand.
6. Run the unknown-verb fallback (`_not_a_command`) inside `execute()`'s error boundary.
7. Compile library sources only through the import closure. A directory walk drags in a
   library's own `test/`, whose unlinked-library placeholders (`__$...$__`) have no
   debuggable artifact.
8. Implement the `vm.assert*` cheats as real comparisons. forge-std calls the `*Decimal`
   and `*ApproxEq*` forms unconditionally, so a blanket revert fails passing assertions.
9. Bind `msg.data`/`msg.sig` from the frame. The injected `__sevm_eval` is reached by a
   real call, so read directly they report *that* call's calldata (`0x365a2820` plus the
   bound locals), silently and plausibly wrong.
10. Prank a DELEGATECALL by rewriting `msg.sender`, not only `storage_address`. Py-EVM
    sources a delegated frame's sender from the caller's `msg.sender` and its storage
    context from `storage_address`; forge's `delegateCall` flag rewrites both, so swapping
    only the latter moves the storage context and leaves `msg.sender` untouched.
11. Put `base_fee_per_gas` back before the transaction settles. The coinbase is paid
    `gas_used * (max_fee_per_gas - base_fee_per_gas)`, so a `vm.fee` above the
    transaction's own cap pays a negative fee and Py-EVM rejects the negative balance.

Underlying Py-EVM monkeypatch gotchas (inherited from the tracer, still apply): restore
the raw classmethod descriptor not the bound method; stack items are `int` or `bytes`;
pass explicit `gas=` so web3 does not re-run the tx during estimation.

## Verified environment

web3 7.16.0, py-evm 0.12.1b1, eth-tester 0.13.0b1, py-solc-x 2.0.5, solc 0.8.28, git 2.x,
forge-std 1.16.2, CPython 3.12. `requires-python = ">=3.10"`. All 702 tests pass as of
2026-08-29 (4 more with `SEVM_NETWORK_TESTS=1`), covering every registered cheatcode
against values taken from real forge, Foundry multi-test coverage, library install and
remapping derivation, the assertion engine, the Yul assembly surface, the build cache and
its artifacts, the snapshot refresh after a mutation, and the package layout itself.
