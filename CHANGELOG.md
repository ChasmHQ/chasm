# Changelog

## Unreleased: native desktop app (branch `UI`)

Chasm is now a native Rust + Slint desktop app. The Axum + React web UI,
the `ui/` folder and the npm build are gone.

### Workspace
- **Act as an address** (Wallet, `wallet.impersonate {address}`): send, deploy and run macros as any address (a governor, a whale, a contract) without its key. The workspace node impersonates it (`anvil_impersonateAccount` + `eth_sendTransaction`), only on chain 31337 and with the same node checks as signed sends. It needs ETH for gas (Timeline > Deal). `app.state.wallet.impersonated` and History show it. tx.origin becomes the address even for a contract, which a real chain never allows.
- A macro result says what reached the chain: how many transactions were confirmed (each separately, so the run is not atomic), what stays applied when it stops early, unknown submissions, and whether a revert was verified. A dry run says that nothing was sent and that it does not simulate the writes.
- Rate-limited public RPCs: the fork node retries with backoff (`--retries 8 --fork-retry-backoff 1000 --timeout 45000`), and Chasm's own upstream reads (Test RPC, Sourcify, replay block lookup) retry HTTP 429/503 following `Retry-After`, at most 5 attempts and 20 s of waiting. Sends are never retried.
- `toolchain.state.network` states both chain ids: `sourceChainId` (the forked chain) and `executionChainId` (31337, where workspace transactions run and are signed), with a note on `block.chainid`.
- The offline engine's workspace build-info is built with the same build source, settings and **locked solc** as the workspace, and records them (`chasmSettings`). A workspace that needs several solc versions names them instead of failing with a unit count.
- `locals` says why there are none: code that matches no build, Sourcify source without an AST, a step without source mapping, or a via-IR/optimized build.
- **Open verified contract** (Explorer, `contracts.import`): on a fork, any verified address opens as a bound tab with its Sourcify ABI and storage layout, without building it. A proxy uses its implementation's ABI. Imports are reloaded from the cache when the project opens.
- Attach and import detect EIP-1967 (implementation and beacon), legacy OpenZeppelin and EIP-1167 clone proxies; Details names the implementation while calls still go to the attached address.
- Send and Simulate say why they cannot run (`sendBlockReason`), and the button, `contracts.view` and the call itself use one check. Through the API a refused call fails with `not_ready` instead of reporting completed; an accepted one returns its `jobId`, and `jobs.list` gives that job's document, summary and transaction hash.
- Native workspace with an activity rail (Contracts, Explorer, Converter, Macros, Debugger), dockable panes, saved layouts and Chasm-styled scrollbars.
- An undeployed contract tab shows its constructor form and deploys in place; the tab then binds to the new address.
- Request editor with one field per ABI parameter, wei/gwei/ether units and an *ether to wei* helper. Response in a side pane with Simple, raw JSON and trace views.
- The function tree sits in the sidebar; functions are collapsible and highlighted by whole row.
- Fields keep focus while typing (request, constructor and macro forms).
- A Send that would revert is stopped before signing and shows the decoded reason. *Debug this call* opens the failing line.
- Storage lists every state variable with its decoded value, plus mapping-key and raw-slot readers.
- Known addresses (Anvil accounts, signer, deployments) and function selectors are named everywhere.
- Ctrl+K / Ctrl+Shift+P command palette.
- Notices offer the next step, and empty views say what to do.

### Debugger
- The desktop and offline engine now share this repository. The gateway loads the worker from `debugger/` in development or the installed payload; it no longer needs an adjacent checkout. Native transaction tracing still works without Python.
- Step debugger for any transaction or call on the project node, mapped to source through solc source maps.
- Step into, over and out, and single instructions, each with a reverse step; internal function calls nest correctly.
- Breakpoints, opcode search, and a Variables tab with decoded arguments and named storage writes.
- Resizable, hideable and compact panes that also float or dock like any other pane, a sevm-style layout preset, and a sevm-style console (`s`, `n`, `si`, `fin`, `c`, `rc`, `b`, `p`, `x`, `sload`, `find`, `args`, `writes`).
- Call tree in the Calls tab and console (`calls`, `frame n`). Arguments decode per frame with matched, attached or unique workspace ABIs. Breakpoints by pc, opcode and frame entry.
- A reverted trace names the frame where the failure started and decodes its custom error from any known ABI. Failed deploys are debuggable and name the EIP-170/3860 size rules. `debugger.trace` traces unsent calls.
- Solidity locals of the function at the cursor (Variables tab, console `locals`).
- **Scenarios**: several transactions (a list of hashes or a macro run) open as one session with storage carried over; `txs` and `tx <n>` move between them.
- Conditional breakpoints (`b op SSTORE if $1 > 100`, `b if depth == 2`), watchpoints on variables and slots, `nexti`, and `compare` against a saved trace (first divergence, then named storage and balance differences).
- **Foundry tests** in the offline engine take breakpoints before the first step and can run straight to the first one. An Output tab shows compile errors, which breakpoints were set, and why a run ended without stopping.
- **Foundry builds from another folder** map in the debugger without recompiling: Compiler and libraries > *Foundry builds for the debugger* reads a project's `out/` artifacts (code, source map, ABI, AST, storage layout).
- State variables by name at any step: `p balanceOf[0x…]`, nested mappings, array elements and struct members. The state before the execution is read when the trace is recorded and saved with it. `diff` lists every named slot and balance the execution changed. Writes undone by a reverting frame are marked, and DELEGATECALL writes land in the caller's storage.
- Gas profile per call frame and per source line (Gas tab, console `gas`).
- Offline-engine replays are saved as captures in `.chasm/captures/` and reopen offline.
- Traces are saved under `.chasm/traces/` and reopen without the node.

### Chain
- Explorer lists the 100 latest blocks and transactions; Investigation moved inside it.
- Project History (`.chasm/history.jsonl`) of every Send and Simulate, with Repeat, Debug and To macro.
- Timeline: a snapshot before every Send, Deploy and cheat, with *Revert to here*.
- Local chain cheats: time, blocks, balance, nonce, storage slot and code.
- Fork a live chain at the latest or a chosen block (chain id stays 31337), from public RPC presets or any URL. The status bar names the forked chain.
- Verified contracts on a fork show their source in the debugger, from Sourcify, cached per project.
- **Test RPC** in Network checks a fork URL before it is saved: chain id, head, the fork block, block 0 and state at the fork block, one line each. Only the host is shown.
- An RPC that prunes block 0 still forks: the identity pins to the fork block instead of the genesis block.
- A real transaction from the forked chain replays on a separate node at its parent block, with its block context and the real chain id, without reopening the project.

### Project
- Build source: Auto uses `foundry.toml` (remappings and solc settings via `forge config --json`) when present, else `.chasm/chasm.toml`; never both. A duplicate-library compile error names both copies and their remappings.
- Compiler: auto per-file pragma, or one solc version locked in `.chasm/chasm.toml`, plus optimizer, runs, via-IR, EVM version and metadata hash. The debugger matches deployed code across immutables, linked libraries and builds without a metadata hash.
- Library manager for OpenZeppelin, OpenZeppelin Upgradeable, forge-std, Solmate and Solady, installed once into `~/.chasm/lib`. Missing imports are flagged.
- New contract from an ERC20, ERC721 or Ownable template (OpenZeppelin v5).
- Macros can be built from a form as well as JSON.

### AI control
- Opt-in local API and MCP server with Limited, Partial and Full tiers.
- No tier can read private keys, mnemonics, passwords or signed payloads. Sensitive actions need local approval.

### Fixes
- Local chain, config, keystore and layout persistence bugs.
- Macro, panel and debugger ownership bugs in the native workspace.
- Clipped UI, em dashes, edge contrast and the grip glyph.

### Development
- `CLAUDE.md` rewritten for the native app. Finished plans moved to `docs/archive/`.
- `[profile.dev] debug = "line-tables-only"` lowers the peak build memory.
- Builds and tests have no warnings.
- End-to-end flows in `tests/live/` (`python tests/live/run.py`), and a stress flow (`run.py stress`) with timing budgets.
