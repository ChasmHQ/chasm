# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What Chasm is

A native desktop workbench (Rust + Slint) for smart contract development and
security work. It compiles a Solidity workspace, runs a local Anvil node it
owns, and lets you deploy, call, trace, step-debug, script (macros) and
inspect contracts. It can also fork a live chain and debug real transactions.

There is no web UI any more. The React/Vite app, `ui/`, the Axum API server
and the file watcher were removed. The only HTTP servers are:
- the debugger gateway on `127.0.0.1:3000`, used internally by the offline sevm engine;
- the opt-in local control API for AI clients.

## Build and run

Prerequisites: Rust, Foundry (`anvil`, `cast`, `forge`) in PATH, and Git (to install Solidity libraries).

```bash
cargo build              # debug build
cargo run -- ./contracts # open a workspace folder
cargo test               # unit and UI tests (no network)
cargo build --release
```

**Memory:**
- The generated Slint code for `ui-slint/main.slint` is one very large module.
- `[profile.dev] debug = "line-tables-only"` keeps a debug build around 7–9 GB.
- Full debug info needed over 11 GB.
- On a 16 GB machine, build with a few jobs (`-j 6`), and test with `-j 1` when memory is tight.

A running `chasm.exe` locks `target/debug/chasm.exe`; close it before building.

## Layout

```
src/
  main.rs            CLI (path, --debugger-python, --debugger-example,
                     --control-token-file in debug builds)
  anvil.rs           Anvil process: local or fork, contained in a Windows job
  compiler.rs        foundry-compilers wrapper; storage layout; locked solc
  config.rs          .chasm/chasm.toml: remappings, [compiler], [network]
  debugger/          gateway + Python sevm worker for the offline engine
  desktop/           the application (state, services, native views)
ui-slint/            Slint UI, one file per area (main, contract, trace, …)
contracts/           sample workspace (Bank.sol)
```

### `src/desktop`, by area

| Area | Files |
|---|---|
| App shell, render loop, project boot | `mod.rs` (`launch`, `boot`, `wire`, `render`, `paint`), `session.rs` (`DesktopState`) |
| Docking panes and layout | `layout.rs`, `panels.rs` |
| Contracts: requests, results, storage | `native_contract.rs`, `contract_service.rs` (signing, preflight, revert decoding), `call_result.rs`, `request_form.rs`, `storage_layout.rs`, `deployment.rs` |
| Explorer, History, Timeline | `chain_read.rs`, `history.rs` + `native_history.rs`, `chain_tools.rs` + `native_chain.rs`, `address_book.rs` |
| Step debugger (the main one) | `trace_debug.rs` (session, stepping, slot naming), `native_trace.rs` (record/save/render), `trace_console.rs` |
| Offline sevm engine | `native_debugger.rs`, `debug_session.rs`, `debug_transport.rs`, `console.rs` |
| Macros | `macro_model.rs`, `macro_form.rs`, `macro_runner.rs`, `macro_backend.rs`, `macro_adapter.rs`, `native_macro.rs` |
| Compiler, libraries, fork | `toolchain.rs` + `native_toolchain.rs` |
| Palette, notices | `native_palette.rs`, `notice_action.rs` |
| AI control API | `control.rs` (tiers, hub), `control_http.rs` (HTTP + MCP), `application.rs` (commands, queries, catalog), `native_control.rs` (runs commands on the UI thread) |
| Wallet | `keystore.rs`, `owned_node.rs`, `mutation_gate.rs` |

## Project files Chasm writes (inside the opened workspace)

All under `.chasm/`, which `.gitignore` excludes:
- `chasm.toml`: `remappings`, `[compiler] solc`, `[network] fork_url / fork_block`.
- `history.jsonl`: every Send/Simulate (public addresses only).
- `traces/*.json`: recorded step-debugger sessions; they reopen without the node.
- `desktop-settings.json`: the layout. Macro libraries live alongside it.

Libraries install once per machine into `~/.chasm/lib/<id>@<tag>` (git
shallow clone). solc binaries are managed by svm.

## Rules that must not break

- **Private keys never leave the wallet code.** No control-API command or query may return or accept a key, mnemonic, password or signed payload. `control.rs` has tests that every query is free of the Anvil key. Keep them passing.
- **Signing only against the owned node, chain id 31337.** Forks keep `--chain-id 31337` so a signed transaction is never valid on a real network.
- **Chain mutations hold the mutation gate** (`state.mutation_gate.try_acquire()`): deploy, send, snapshot, revert, cheats, macro runs.
- **Revert/Timeline:** a revert must call `apply_revert` / `revert_to_point` so deployments made after the snapshot are dropped and the network epoch advances.
- Provider error messages from RPC are untrusted; `contract_service::validate_and_redact_rpc` keeps only ABI-shaped revert data.

## Slint gotchas met in this codebase

- A property named `row` conflicts with `ActionControl`; use another name.
- An element cannot set both `height` and `min-height` (or `width` and `min-width`).
- Ids inside an `if` cannot be referenced from outside it.
- Rebuilding a `ModelRc` every paint recreates its `LineEdit`s and drops keyboard focus. Reuse the model with `mod.rs::keep_rows`.
- Feed widths that depend on the viewport from Rust (`PanelData.viewport-width`) to avoid binding loops.
- Read-only text uses `SelectableText` (`text-cursor-width: 0`).

## Testing through the control API

Debug builds accept `--control-token-file <file>`. It enables the local API,
pairs one Full-tier client and writes the endpoint and token to the file:

```bash
./target/debug/chasm.exe ./contracts --control-token-file /tmp/token.txt
python tests/live/run.py /tmp/token.txt
```

- `GET /api/v1/catalog` lists every command and query, with schemas.
- Commands are POSTed to `/api/v1/command` with the current `context` from `app.state`.
- Useful queries:
  - `app.state`, `contracts.view`;
  - `debugger.state` (step debugger under `debugger.trace`);
  - `history.list`, `toolchain.state`;
  - `ui.screenshot` (PNG).
- Chain work is asynchronous; poll `app.state.busy` until it settles.

See `AI_CONTROL.md` for the tier table and `DEBUGGER.md` for the step
debugger's commands and keys.

## Workflow

- Branch `UI` holds the native redesign. Do not push or merge without asking.
- Plans and progress: `REDESIGN_PLAN.md`, `FIX_PLAN.md` (done), `NEXT_PLAN.md` (current).
- Each change gets its tests and a live check through the API, then its own commit.
