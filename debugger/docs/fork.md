# Mainnet forks and historical replay

The Rust REVM engine reads pinned Ethereum state through a read-only RPC client. Transactions, Solidity expressions, Yul and prompt cheatcodes execute locally. No private key, signing, account unlock or transaction broadcast is required.

## Support envelope

| Area | Current support |
|---|---|
| Chain | Ethereum L1, chain ID 1 and mainnet genesis identity |
| Historical rules | Cancun timestamps `1710338135 <= timestamp < 1746612311` |
| Transactions | Legacy, access-list, dynamic-fee and Cancun blob transactions, types 0 through 3 |
| Remote state | Archive account/code/storage reads with EIP-1898 `blockHash` and `requireCanonical` selectors |
| Raw debugging | PC/opcode breakpoints, stepping, frames, stack, memory and storage without source |
| Source debugging | Embedded Solidity compiler build-info with matching runtime body and source maps |
| Evaluation | Supported Solidity, Yul and prompt cheatcode operations on the live local journal |
| Validation | Prefix and target receipt status, transaction gas, ordered logs and created address |
| Offline | Versioned capture containing the base reads used by the captured execution |

Pre-Cancun, Prague and later blocks, pending transactions and L2 execution are rejected or outside this release. A current mainnet transaction will normally be outside this historical support interval. Extending the interval requires implementing and testing the later fork rules, not changing a timestamp check.

A provider must support historical state and block-hash selectors. Unsupported selectors and unavailable archive state fail explicitly. There is no fallback to `latest` or a public RPC endpoint. Provider URLs are supplied by environment-variable name so credentials need not appear in command history.

## Offline example

`examples/mainnet/synthetic-replay.json` is a **synthetic fixture**, not a recorded Ethereum transaction. Its block hashes, height and accounts are invented. The prefix reverts with hand-derived gas `21004`. The target is `STOP` with gas `21000`. A minimal synthetic beacon contract exercises block-start processing but is not the deployed mainnet beacon-root contract.

From the repository root, run:

```bash
uv run chasm replay --offline examples/mainnet/synthetic-replay.json --console -x 'info registers' -x c
```

The command recomputes the baseline from the capture, opens the target at PC zero and uses the ordinary debugger frontend. No compiler or RPC is needed for raw debugging. The baseline report says `matched_receipts`, not state-root validation.

## Online replay

The `replay` command accepts a mined transaction hash, `--rpc-env` naming a configured archive RPC variable, optional `--contracts` selecting one embedded compiler build-info JSON, and optional `--capture` naming a new file. Run `uv run chasm replay --help` for the argument reference. Live provider verification is opt-in and is not part of the default test suite.

Preparation proceeds as follows:

1. Validate chain identity, target transaction, receipt and target/parent header relationships.
2. Pin remote reads to the parent block hash.
3. Apply the Cancun beacon-root block-start system call under the target block environment.
4. Execute and commit every preceding transaction in the block, preserving reverted transactions' nonce and fee effects.
5. Execute the unmodified target and compare its receipt dimensions.
6. Rebuild the same pre-target state and start the interactive target session.

Nonce, fees, access lists, blob fields and block context are not replaced by debugger defaults. Receipt cumulative gas is distinguished from the target's own gas. Withdrawals and other end-of-block processing are not applied before the target.

A mismatch fails closed. Restart reconstructs the pinned base, block-start transition and prefix rather than starting with an empty chain. The baseline is kept separate from subsequent counterfactual results.

## Fork experiments

The `fork` command selects an explicit historical block using `--block`. Its driver requires `--to`, `--from`, `--nonce` and `--gas-price`. Optional `--data`, `--value` and `--gas` control the local transaction. There is no automatic funding or nonce guessing.

This is a sandbox on the selected block's end-state, not a historical replay. Its synthetic next-block timestamp and fee fields are derived locally. Beneficiary and prevrandao are inherited and no unknown next-block beacon root is invented. Reports identify this context as synthetic.

## Source and expression limits

`--contracts` imports one build-info file, or a directory containing exactly one JSON file. Input source contents and compiler output must be embedded. The importer does not fetch source URLs or invoke solc. It preserves compiler settings, source IDs and library links.

A normalized runtime-body match can ignore a validated Solidity CBOR tail and immutable values. Reports separately expose exact byte equality and raw code hashes. Neither comparison is a proof of semantic equivalence. Unmatched code remains raw rather than receiving guessed source attribution.

Optimized/viaIR locals are unavailable in fork sessions because their stack positions are not reliable. Solidity evaluation of viaIR builds and fork artifacts containing immutable references is currently rejected explicitly. An expression may invoke the compiler version recorded in build-info when evaluation is requested.

Nested Solidity evaluation supports ordinary EVM calls and creation through a bounded local frame driver. It does not run interactive nested stops or emulate Foundry VM/console host callbacks inside an expression. Unsupported host callbacks fail without keeping partial work. The driver bounds gas, frame depth and transitions. Time checks occur between transitions and individual RPC requests have timeouts, not a hard interrupt of every opcode.

Supported prompt `vm.*` commands still operate through the live state bridge. Historical execution itself does not intercept special Foundry addresses or seed fake accounts there.

`p` discards evaluation side effects. `call`, `set var`, Yul writes, prompt cheats and raw state edits mark the session diverged. Inspection is non-perturbing and does not warm storage for the resumed historical execution. Discard failures and nested reverts restore the isolated execution context.

## Captures and trust

A capture retains normalized target/parent headers, transaction requests and receipts, baseline comparisons, and the remote accounts/code/slots/block hashes that were read. Endpoint credentials are excluded. Capture creation refuses an existing path by default. New captures use `chasm-fork-capture`. The reader also accepts the previous `sevm-fork-capture` version-one format.

Offline replay disables transport and fails on missing witness data. A baseline capture does not contain every possible slot or callee an arbitrary experiment might request. Programmatic callers can export the extended witness from the interactive chain and save it explicitly for a later offline experiment. Source build-info is supplied separately and is not bundled automatically.

RPC responses and capture contents are trusted inputs subject to shape and consistency checks. Transaction signatures, transaction hashes, header hashes and state proofs are not independently reconstructed or authenticated. Matching receipts establishes agreement with the supplied reference dimensions, not full-block state-root equivalence or independent consensus verification.

## MCP

The same session implementation is exposed by:

- `chasm_start_replay(tx_hash=..., rpc_env=..., contracts=..., capture=...)`, or `chasm_start_replay(offline=...)`.
- `chasm_start_fork(block_number=..., rpc_env=..., to=..., caller=..., nonce=..., gas_price=...)`.
- `chasm_fork_info()` for anchor, baseline, source parity and divergence.

Existing navigation, inspection, evaluation and mutation tools then control that session. Terminal and MCP are equivalent entry points, not simultaneous clients attached to one process.

## Current delivery boundary

The implementation and default tests use local fixtures, including a recording localhost RPC server. A synthetic receipt match is not evidence that a historical mainnet transaction has been reproduced against a live provider. Live validation requires an explicitly supplied archive endpoint and selected in-range transactions.

The Chasm browser UI is not wired to these operations yet. It should consume the same headless engine after the native/terminal/MCP path is validated, rather than implementing a second replay pipeline over Anvil.
