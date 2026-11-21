# Network profiles: fork another network and switch safely

**Status:** approved 2026-11-21; N0 first. **Branch:** `UI`, never merged into `main`, pushed only on request.

## Why

Audits and CTFs need a fork of mainnet (or Sepolia) to test the real impact of an issue: real contracts, real balances, real proxies, at a chosen block. Chasm today keeps one `[network]` entry (`fork_url`, `fork_block`) in `.chasm/chasm.toml`. Changing it saves the setting and reopens the whole project: Anvil, the debugger worker, the runtime and all in-memory state are rebuilt.

What breaks or goes stale when switching today:

- **Lost:** deployments, bound contract tabs, the Timeline and its snapshots, and the chosen signer live only in memory. They are gone after every switch, including a switch back.
- **Kept but from the wrong network:** `history.jsonl`, `traces/*.json` and macro definitions record no network. Repeat or Debug from History can hit an address that only happens to match on another network. Local Anvil transaction hashes repeat across restarts, so a saved trace can be overwritten. `imports.json` is reloaded for every chain (`contract_import::reload` ignores `fork_chain_id`). The same address on two chains collides because the name is `Name@4digits`.
- **Bugs that switching exposes:**
  - `native_trace::RUNTIME` is a `OnceLock`, so after a reopen `load_call` and `load_scenario` spawn on the dropped runtime.
  - `fork_chain_id` is fetched once with no retry, so one 429 at boot disables import, Sourcify and the chain name for the session.
  - A broken `chasm.toml` silently boots a local chain.
  - Trace completions check `workspace_epoch` but not `network_epoch`.
- **Chain behaviour:** the workspace node always runs with `--chain-id 31337`. That is safe (a signed transaction is never valid on the real network), but code that reads `block.chainid` (EIP-712 domains, permit, bridges) behaves differently from the real chain. The offline REVM worker accepts Ethereum mainnet (Cancun) only.

**Goal:** named network profiles that switch safely. Every component keeps working after a switch, data is tagged with its network, and each profile's state can be resumed.

## Decisions

1. **Chain id per profile.** Default stays 31337. A profile can use the **source chain id**. In that mode only Anvil dev keys and *Act as an address* may send; personal keystores are blocked, because their signatures would be valid on the real network.
2. **State is kept per profile.** Switching back to A resumes it. *Start from a clean fork* is available.
3. **First-class networks: Ethereum mainnet and Sepolia**, through any RPC (public presets, a provider URL with a key, an archive node). Other EVM chains still fork with a capability warning and no special handling.
4. **The offline worker stays as it is.** It is labelled "Ethereum mainnet only"; multi-chain support for it is a separate plan.

## Phases

### N0 — Foundations and bugs (small, first)

- Replace `static RUNTIME: OnceLock` in `src/desktop/native_trace.rs` with a handle that every `wire()` replaces. Test: a second `wire` uses the new handle.
- `boot` (`src/desktop/mod.rs`): when `chasm.toml` exists but does not parse, refuse to boot with a clear message instead of starting a local chain.
- Fetch the source chain id through `retry::send`. If it still fails, boot continues, but the notice and status bar say "source chain unknown" and offer **Retry**. Never silently `None`.
- Trace completions in `native_trace.rs` also check `network_epoch`, like `accepts_request` in `session.rs`.
- Remove the unused `trace_input` (or use it in N3).

### N1 — Profiles and a safe switch (medium)

- **Config** (`src/config.rs`): `NetworkProfile { id, name, fork_url | fork_url_env, fork_block, chain_id_mode: Local | Source }` as `[[networks]]`, plus `network.active`. `local` is built in and cannot be removed. The old `[network]` migrates to an active profile `fork-1` through `ChasmConfig::update` (tested). `fork_url_env` keeps provider keys out of the file; UI and API only ever show the host (`fork_host`).
- **Identity** (new `src/desktop/network.rs`): `NetworkIdentity { profile_id, source_chain_id, fork_block (the block actually forked), anchor_hash, execution_chain_id }`, computed at boot by reusing `chain_read::load_identity` / `contract_service::identity_at` and the `anvil_nodeInfo` anchor fallback. Its `key()` (`profile:chain:block`) tags data in N2.
- **Anvil** (`src/anvil.rs`): `start_fork` takes the profile; `apply_fork_args` sets `--chain-id` by mode. `FORK_RETRY_ARGS` stays. Argument-vector tests per combination.
- **Switch** (`native_toolchain.rs` → `switch_network`):
  1. Preflight with `toolchain::test_rpc` before anything is torn down. On failure nothing happens and the current network keeps running.
  2. Save the current profile's state (N3).
  3. Write `network.active` and reopen, reusing `next_workspace`.
  4. If the target node does not become ready, boot the previous profile again and show the error.
- **UI** (`ui-slint/toolchain.slint` Network): the profile list (name, chain, block, chain id mode, active), **Add / Edit / Duplicate / Delete / Test / Switch**, presets fill a new profile. Status bar: `profile · Ethereum (1) @ block N · signs as 31337`. The switch dialog says what is kept and what is reset.
- **API:** `network.list`, `network.save {profile}`, `network.remove {id}`, `network.switch {id, mode: resume | fresh}`; `toolchain.state.network` gains `profile`, `identity`, `profiles`. Scope `act`; `fresh` (discards saved state) is `sensitive`. `toolchain.action network` stays as an alias that edits the active profile.

### N2 — Tag and filter data by network (medium)

Every artifact saved under `.chasm/` records its network (profile, source chain, block). Untagged old data shows as "unknown network" and is never deleted automatically.

| Component | Change | Files |
|---|---|---|
| History | `Entry.network`. The list shows the active network by default, with *All networks*. Repeat, Debug and To macro are disabled for entries from another network, with the reason. | `history.rs`, `native_history.rs`, `application.rs` |
| Saved traces | `SavedTrace.network`, `replayed`. File names carry the network so they never overwrite each other. Opening uses Sourcify for the **recorded** chain. By-address frame ABIs only when the code matches. | `native_trace.rs` |
| Imports | `reload` loads only `import.chain == source_chain_id`. Names unique per chain. Warn when the node has no code at the address (an older fork block). | `contract_import.rs`, `session.rs` |
| Macros | Optional `network` set when saved on a network; validation warns (does not block) on another network. | `macro_model.rs`, `native_macro.rs` |
| Investigation | Captures are already tagged; the view filters to the active network, with *All*. | `investigation.rs`, `native_investigation.rs` |
| Sourcify cache | Already keyed by chain; unchanged. | `sourcify.rs` |

### N3 — Per-profile state: resume or clean fork (medium, spike first)

- **Spike:** confirm `anvil_dumpState` / `anvil_loadState` on Anvil 1.2.1 restores a fork at the same block (touched accounts, storage, code, nonces). If it is not reliable, resume keeps deployments and the signer only, the chain always starts from a clean fork, and the UI says so.
- `.chasm/networks/<id>/`: `state.json` (Anvil dump), `deployments.json` (name, address, artifact, bound tabs), `signer.json` (the signer *choice*: a dev key index or an impersonated address). Keys and keystores are never written; a keystore must be unlocked again.
- **Resume:** load the state, restore deployments and tabs, add a Timeline point "Restored from the previous session" (old Anvil snapshots cannot carry over).
- **Fresh:** discard the profile's folder (local confirmation), add a Timeline point "Fork at block N".
- Saved state whose identity (chain, fork block) differs from the new node is rejected; the profile starts clean, with a message.

### N4 — Source chain id mode (small to medium, security-sensitive)

- `chain_id_mode = Source` runs Anvil with the source chain id.
- Every 31337 / `0x7a69` check reads `state.network.execution_chain_id` instead: `contract_service.rs` signing guards and `unsigned_transaction`, `macro_backend.rs` source and submission checks, the boot readiness probe, `chain_read`, `macro_executor.rs`. The loopback-8545 and `OwnedNode::authorizes_loopback` checks stay.
- In Source mode only Anvil dev keys #0–#2 and impersonation may send, checked in `call_block_reason`, `deployment_problem` and the macro signer resolver: "Personal keystores are blocked on a network with its real chain id: a signed transaction would be valid on the real network."
- A yellow banner in the status bar and Wallet; `toolchain.state.network.executionChainId` reports the real value.
- Tests: guard unit tests for both modes; a live `ChainIdProbe` fixture returns 31337 by default and the source id in Source mode.

### N5 — Ethereum and Sepolia first, others with warnings (small to medium)

- `chain_info(id) -> { name, symbol, kind }` extends `chain_name`. Ethereum (1) and Sepolia (11155111) are first-class. Any other chain id is `other`: it forks, and the Network panel, `test_rpc` and the trace say precompiles, system transactions and fees may differ, without special handling.
- `test_rpc` adds the chain kind and capabilities: archive state, `debug_traceTransaction` support, and a warning when the workspace `evm_version` is newer than the network's.
- The native-token label follows `chain_info` (ETH on both first-class chains; the reported symbol, or "native token", otherwise).
- Offline worker: the Debugger panel and `debugger.state` say "Offline engine: Ethereum mainnet only; use the native trace on other networks." No worker changes.

## What must keep working after a switch

- **Contracts:** the same compile output; imports for the active chain only; deployment tabs restored on resume; `sendBlockReason` correct for a tab whose address has no code on the new network.
- **Wallet:** the signer choice restored without keys; keystores blocked in Source mode; impersonation works in every mode.
- **Timeline, cheats, snapshots:** work on the new network, with a clear starting point.
- **Send / Simulate / jobs:** an old `jobId` resolves as `workspace_changed` through the existing hub (`control.rs`).
- **Macros and bundles:** run on the active network; warn for a definition tagged for another; bundles use `execution_chain_id`, never a 31337 constant.
- **Native debugger:** trace, scenario, compare and the replay node use the active profile's RPC; a saved trace from another network still opens offline with its own chain's Sourcify cache.
- **History, Investigation, Explorer:** filtered or refreshed per network; cross-network actions disabled.
- **Control API:** `app.catalog` reflects availability (for example `contracts.import` only with a known source chain).

### N6 — Verification, documentation and skill

- **Unit tests:** config migration; `NetworkIdentity` keys; history and import filters; unique import names; saved-trace tags; chain id guards in both modes; keystore block in Source mode; Anvil argument vectors; a new runtime handle after a second `wire`.
- **Live flow `networks`:**
  1. profiles `local` and `mainnet`;
  2. deploy Bank and deposit locally;
  3. switch to mainnet: USDC readable, local Bank absent, History filtered;
  4. import USDC;
  5. switch back to local with resume: Bank, its balance and the signer restored, USDC import absent;
  6. switch to mainnet with resume: the USDC import is back;
  7. mainnet profile in Source mode: `ChainIdProbe` returns 1;
  8. a profile with a bad URL: the preflight refuses and the active network is untouched.
- An optional Sepolia check behind `CHASM_SKIP_FORK`. The full live suite must keep passing; the `fork` flow moves to profiles.
- **Docs:** README, AI_CONTROL (`network.*`), DEBUGGER (saved traces across networks, worker mainnet-only), CHANGELOG, `WORKSPACE_RELIABILITY_PLAN.md`, and the `chasm-bugbounty` skill (confirm the network and chain id before judging impact).
- **Release:** build `target/release` (PATH unchanged); one commit per phase on `UI`.

## Order

N0 → N1 → N2 → N3 (spike first) → N4 → N5 → N6. Each phase ships with its own tests and documentation.

## Progress

| Phase | Status |
|---|---|
| N0 | done: replaceable trace runtime, boot refuses a broken config, retried source chain id with a Retry notice, trace completions check `network_epoch`, `trace_input` removed |
| N1 | done: `[[networks]]` profiles with migration, `NetworkIdentity` (`key()` = `profile:chain:block`), Anvil chain id per mode, preflight + fallback switch, profile UI, `network.list/save/remove/switch` |
| N2 | not started |
| N3 | not started |
| N4 | partially plumbed: `chain_id_mode` runs Anvil with the source id and the probe expects it; signing guards and the keystore block are still N4 |
| N5–N6 | not started |

N1 notes: `network.switch` and an active-profile `network.save` return `effect: "reopen_pending"` — the event loop must not quit before the preflight decides to reopen. The fallback marker is `.chasm/network-switch.json`; boot clears it after any ready boot and rewrites `network.active` to the profile it fell back to.
