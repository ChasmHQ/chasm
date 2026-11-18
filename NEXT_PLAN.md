# Next plan, after goals G1–G7 (2026-11-18)

Status: proposed, not started. FIX_PLAN.md (round 2 and goals G1–G7) is
finished. This plan covers what is left, ordered by what blocks real use
first.

Facts the plan is based on:
- The `UI` branch has 32 commits on top of `main` (171 files, +65.8k / −15.3k lines). Nothing is pushed or merged.
- 397 tests pass. A build reports 68 warnings, mostly dead code from the removed web UI, e.g. `MacroRun`, `ResolvedArtifact`, `ResourceInfo`, and three unused Slint components in `macro.slint`.
- `CLAUDE.md` still describes the Axum + React web app, the `ui/` folder and `npm` builds, all removed in Phase 0.
- `main.slint` is 1486 lines; all Slint files total 5531. The generated Rust for it is what made one rustc need 7–11 GB.
- 51 one-off scripts in `temp/.review-scripts/` drive the live checks. They are not in the repo and not run by `cargo test`.

## P1. Make the branch mergeable

The work is only useful once it can land on `main`. Nothing here is a feature.

1. **Rewrite `CLAUDE.md`** for the native app. Cover:
   - the build (`cargo build`, the memory note, `debug = "line-tables-only"`);
   - the module map (`src/desktop/*`, `ui-slint/*`);
   - the control API test loop;
   - the safety rules: no private keys over the API, chain id 31337 for signing, and the mutation gate.
2. **Clear the 68 warnings.**
   - Delete dead code left from the web UI and the old macro runner types.
   - Delete the unused Slint components.
   - Run `cargo fix` for the two mechanical fixes.
   - Done when `cargo build` reports 0 warnings.
3. **Move the live checks into the repo** as `tests/live/*.py`, with one runner (`python tests/live/run.py`). It starts Chasm with a token file, runs each flow and prints a pass/fail table.
   - Keep the flows that guard features: bank, revert, step, history, timeline, toolchain, fork, macro form and palette.
   - Drop the one-off `edit_*.py` scripts.
4. **Merge readiness review.**
   - Read the full `main..UI` diff for leftovers: debug prints, TODO stubs, the `.agents/` folder, temp files.
   - Write `CHANGELOG.md` from the commit list.
   - Ask the user before merging or pushing.

## P2. Finish half-done features

Each item was cut short and is recorded as "not done" in FIX_PLAN.md.

1. **Deploy inside the request editor (Phase 1c).**
   - Deploy still opens a separate "Deploy Bank" tab.
   - Show the constructor form at the top of the contract tab when the contract has no deployment, with Deploy beside Send.
   - Target: deploy, call and trace in 5 clicks or fewer, measured through the API.
2. **Debugger panes as real docking panes.**
   - Today the panes resize and hide only inside the Debugger view.
   - Register Source, Opcodes, State and Console as `PaneKind`s so they can float, move to another zone and be saved in the layout.
   - Add a `Preset::Debugging` that follows the sevm TUI.
3. **Save offline-engine sessions as captures.**
   - Call the worker's `export_capture` after a replay and save the result to `.chasm/captures/<time>-<label>.json`.
   - List the captures in the Debugger next to Saved traces, and reopen them through the gateway resource upload.
   - Synthetic and Foundry runs have no replay baseline; say so instead of failing.
4. **ERC20 / ERC721 / Ownable templates.**
   - Add "New contract from template" in the Contracts sidebar. It writes a `.sol` file into the workspace, uses OpenZeppelin when it is mapped (offering Install when not), and recompiles.
5. **Fork usability.**
   - A list of known public RPCs (Ethereum, Base, Arbitrum, Optimism, Polygon) to pick from.
   - Show the fork's real chain name and block next to "31337".
   - Explorer lookups of addresses on a fork read the remote chain.

## P3. Robustness (Phase 7)

The UI was only checked with the small Bank contract.

1. **Stress fixtures:**
   - a 128-step macro;
   - a 64 KiB trace;
   - a function with 20 parameters;
   - a 50-item array argument;
   - 30 deployments;
   - 15 open tabs;
   - 500 History entries.
2. **Sizes:** check each fixture at 1280×720, 1700×900 and 2560×1440, and at zoom 100 %, 125 % and 150 %. Use API screenshots compared by eye, plus `view_capture_tests.rs` for layout assertions such as no clipped buttons and no overlapping text.
3. **Large traces.** A real mainnet transaction can exceed the 1 MiB trace limit and the 200 000-step cap. Stream the trace, or record only a step window, and say clearly when a trace was cut.
4. **Performance budgets.** Opening a 100k-step trace, filtering a 500-row History and opening the palette must each stay under 200 ms. Measure them with the API timings.

## P4. Debugger depth

1. **Solidity locals.**
   - solc gives no scope table, so this needs `ethdebug` output (solc ≥ 0.8.29, experimental), or the sevm debugger's local reconstruction (`debugger/src/chasm_debugger/locals`).
   - Prototype with the sevm code first, since it already exists.
2. **Source for verified mainnet contracts.**
   - With an Etherscan or Sourcify lookup, fork traces would show source instead of opcodes only.
   - Sourcify needs no API key.
   - Fetch the sources into `.chasm/sources/<chain>/<address>/` and map them like workspace programs.
3. **Gas profile.** Per-line and per-function gas, from the recorded steps, as a sortable table in the debugger.

## Order and size

| Step | Size | Why this order |
|---|---|---|
| P1.1–P1.3 docs, warnings, tests in repo | small | Needed before anyone else can work on the branch. |
| P1.4 merge review | small | Then ask the user about merging. |
| P2.1 deploy in the request editor | medium | The original 5-click goal is still not met. |
| P2.5 fork usability | small | Cheap, and makes G6 usable without knowing RPC URLs. |
| P2.4 templates | small | Pairs with the library manager. |
| P3 robustness | medium | Before showing the tool to other users. |
| P2.2 docking debugger panes | medium | UX polish. |
| P4.2 Sourcify source | medium | Biggest gain for mainnet debugging. |
| P2.3, P4.1, P4.3 | large | Depend on the sevm worker or experimental solc output. |

Each step ships with tests and a live check through the control API, gets
its own commit, and is recorded in this file's progress table.

## Progress

| Step | Status |
|---|---|
| P1.1 CLAUDE.md | not started |
| P1.2 warnings | not started |
| P1.3 live tests in repo | not started |
| P1.4 merge review | not started |
| P2.1 deploy in request editor | not started |
| P2.2 docking debugger panes | not started |
| P2.3 offline captures | not started |
| P2.4 templates | not started |
| P2.5 fork usability | not started |
| P3 robustness | not started |
| P4.1 locals | not started |
| P4.2 Sourcify source | not started |
| P4.3 gas profile | not started |
