# Live tests

End-to-end flows that drive a real Chasm window through the local control
API. `cargo test` does not run them: they need a debug build, Foundry in PATH,
port 8545 free and, for `fork`, network access.

```bash
cargo build
python tests/live/run.py                 # default suite, excluding stress
python tests/live/run.py revert history  # bank always runs first
python tests/live/run.py stress           # large fixtures and timing budgets; slow
CHASM_SKIP_FORK=1 python tests/live/run.py
```

`run.py` copies `contracts/` to a temporary folder, starts
`target/debug/chasm` on it with `--control-token-file`, runs the flows in
`flows.py` in order and prints a table. Your own `.chasm/` folder is never
touched. The private-key check derives Anvil's public test key with `cast`, so no
key is stored here.

## Flows

| Flow | Checks |
|---|---|
| `bank` | deploy, deposit, trace, snapshot/revert; no query leaks the Anvil key |
| `revert`, `steps` | revert reason before sending; step into/over/out and their reverses |
| `history`, `timeline` | saved calls, Repeat/Debug/To macro; snapshots, cheats, revert to a point |
| `toolchain`, `template` | lock solc, install/use/remove a library, Test RPC without showing the URL; ERC20 template compiled and deployed |
| `call_jobs` | Send on a tab without a deployment fails with `not_ready` and starts no job; a real Send returns a `jobId` that finishes with a hash and lands in its own document |
| `palette`, `macro` | Ctrl+K; a macro built from form commands |
| `inline_deploy` | deploy from the contract tab, call and debug in five clicks |
| `panes` | trace panes dock, float and hide |
| `gas`, `locals` | gas profile per call and line; Solidity locals (`i`, `result`, calldata array) |
| `scenario` | two deposits as one session (`txs`, `tx 2`, storage carried over, one TX frame each), a watch stopping in each, a conditional opcode break, `compare` naming the differing slot |
| `storage_names` | `p balanceOf[owner]` before and after a deposit, `p owner`, `p luckyNumbers[1]`, named `writes`, `diff` with the balance change |
| `foundry_test` | a test in the workspace pauses at its first line, runs to a launch breakpoint, lists unresolved breakpoints and shows a compile error in Output |
| `captures` | offline replay saved to `.chasm/captures/`, uploaded and replayed |
| `call_tree` | keeper → token frames with outcomes, per-frame args, entry and opcode breaks, an attached ABI naming code Chasm did not build |
| `external_build` | a via-IR, optimizer, no-metadata contract with an immutable, deployed by `cast`, maps to source with matching `[compiler]` settings |
| `foundry_import` | a via-IR build in another folder, added as an import, maps to source with args and `p total` from its layout; a missing folder is refused |
| `build_source` | Auto follows `foundry.toml`; a duplicate library names both copies |
| `revert_explained` | a keeper revert names `MiniToken.transfer` and its custom error; an oversized deploy names EIP-170; unsent calls trace through the API |
| `fork` | mainnet fork, real transaction replay matches the receipt, Sourcify source; USDC imported by address as a proxy with its implementation's ABI, `name()` read through it |
| `stress` (only when named) | 20-field form, 200k-step trace, 30 deployments, 500 History rows, 128-step macro, three zooms |

`CHASM_STRESS_SHOTS=<folder>` saves a screenshot of each view per zoom.
