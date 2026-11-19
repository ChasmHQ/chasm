# Changelog

## Unreleased: native desktop app (branch `UI`)

Chasm is now a native Rust + Slint desktop app. The Axum + React web UI,
the `ui/` folder and the npm build are gone.

### Workspace
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
- Step debugger for any transaction or call on the project node, mapped to source through solc source maps.
- Step into, over and out, and single instructions, each with a reverse step; internal function calls nest correctly.
- Breakpoints, opcode search, and a Variables tab with decoded arguments and named storage writes.
- Resizable, hideable and compact panes that also float or dock like any other pane, a sevm-style layout preset, and a sevm-style console (`s`, `n`, `si`, `fin`, `c`, `rc`, `b`, `p`, `x`, `sload`, `find`, `args`, `writes`).
- A reverted trace names the frame where the failure started and decodes its custom error from any known ABI. Failed deploys are debuggable and name the EIP-170/3860 size rules. `debugger.trace` traces unsent calls.
- Solidity locals of the function at the cursor (Variables tab, console `locals`).
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
- A real transaction from the forked chain replays on a separate node at its parent block, with its block context and the real chain id, without reopening the project.

### Project
- Compiler: auto per-file pragma, or one solc version locked in `.chasm/chasm.toml`.
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
