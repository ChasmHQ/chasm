# Plan: find the cause of any revert in one flow (2026-11-19)

Status: R1–R4 and R6 done; next R5.

This plan comes from a review of a real fork session. The step debugger
works: a contract Chasm compiled itself stepped by source line, through
123 steps. The gaps are everything around it:
- deployed code that Chasm did not build;
- reverts that never become a transaction;
- the Foundry test debugger;
- storage and frame inspection;
- fork setup.

## Facts checked in the code

- `CompilerConfig` (`src/config.rs`) has only `solc`. The workspace build always uses optimizer off and via-IR off. Optimized production code never matches it.
- `trace_debug::same_code` strips only the CBOR tail.
  - Code built with `bytecode_hash = "none"`, with immutables, or with linked libraries never maps.
  - With no match, a frame shows no source, no locals and no arguments.
- `Session::call_arguments` decodes with the ABI of a *matched program*. An address attached with an ABI in a contract tab is not used.
- Only `contracts.send` keeps the reverted call (`ContractView.reverted_call`). A deploy that fails preflight leaves nothing to debug.
- `contract_service::identity_at` requires block 0 from the RPC. Some RPCs do not serve it, so the fork cannot pin its identity.
- The Foundry launch of the offline engine runs to the end. It has no start-paused option, no breakpoints before start, and no worker stdout/stderr in the UI.
- `sload` reads only slots the trace touched. `writes` shows raw slots when the layout cannot name them.

## Goals

Each goal has a live check in `tests/live/`. A goal is done only when that
check passes.

| Goal | Done when |
|---|---|
| G1. A revert always explains itself | A keeper call that reverts on an insufficient USDC balance shows the failing frame, the token call and the decoded reason in Chasm, without Foundry `-vvvv`. |
| G2. Source for code Chasm did not build | A pair deployed from a via-IR, optimizer 200, `bytecode_hash = none` build steps by source line, with arguments decoded. |
| G3. Every frame is inspectable | The trace shows the full call tree. Any frame can be selected; its stack, memory, storage and arguments are shown. |
| G4. Storage by name, before and after | Variables, mapping entries and structs read by name at any step, without `cast index`. Each transaction has a state diff. |
| G5. Foundry tests pause | A Foundry test starts paused at its entry, with breakpoints set before start. A compile error is shown, not swallowed. |
| G6. Fork and compile set up on the first try | An RPC without block 0 still forks. A Foundry project compiles with its own remappings and compiler settings. |
| G7. Scenarios across transactions | borrow → redeem → sync → withdraw is stepped in one session. Two traces are compared to their first divergence. |

## Steps

### R1. Debug every preflight failure (G1)

1. Deploy preflight keeps its `eth_call` (with `data` = creation code) as `reverted_call`, like Send. *Debug this call* opens it.
2. Explain empty reverts before tracing:
   - creation code over 49,152 bytes (EIP-3860);
   - runtime over 24,576 bytes (EIP-170, known after a traced create);
   - out of gas: the gas used reaches the limit;
   - a call to an address with no code;
   - insufficient balance for `value`.

   Each case gets one sentence and the numbers.
3. The trace status names the **deepest frame that reverted first**: contract, function (from any known ABI), revert data decoded as `Error`, `Panic` or a custom error. The custom error is matched against every workspace and attached ABI.
4. `debugger.trace` gets a `call` form, so the API can trace an unsent call directly. Today it only takes a hash or "the newest action".

Live check `revert_explained`:
- a token transfer without balance shows `ERC20InsufficientBalance` from the token frame;
- an oversized deploy shows the size rule.

### R2. Match deployed code to its build (G2)

1. Compiler settings in `.chasm/chasm.toml`:
   - `[compiler]` gets `optimizer`, `optimizer_runs`, `via_ir`, `evm_version` and `bytecode_hash`;
   - these are passed to `foundry-compilers` and shown in Compiler and libraries.
2. Configuration source: **Auto / Foundry / Chasm** (the design agreed earlier).
   - Auto uses `foundry.toml` when present: profile, remappings and compiler settings.
   - The two configurations are never merged.
   - The UI shows the active source per dependency.
3. Import a build: "Add Foundry build" reads `out/**/*.json` and `out/build-info/*.json` of a project. These programs join the debugger's list without recompiling.
4. Matching that tolerates what differs per deployment:
   - zero the `immutableReferences` ranges;
   - zero `linkReferences`;
   - strip the CBOR tail only when present (`bytecode_hash = none` has none).

   A unit test covers each case.
5. via-IR source maps are coarser. Stepping works, but locals need legacy codegen. The Variables tab says so instead of showing nothing.

Live check `external_build`: a fixture compiled by `forge build` with via-IR, optimizer and immutables is deployed outside Chasm. It then steps by source line and decodes its arguments.

### R3. Frames and ABI decoding (G3)

1. The trace builds a call tree from depth changes. Each frame records:
   - address, code address and kind (CALL, DELEGATECALL, STATICCALL, CREATE);
   - selector, value, gas;
   - return or revert data.
2. A Calls tab lists the tree. Selecting a frame moves the State pane to that frame: its first step, or its step at the cursor.
3. ABI sources for decoding, in order:
   - a matched program;
   - an ABI attached in a contract tab (by address);
   - every workspace ABI by selector.

   Unknown selectors stay unknown; there is no external lookup.
4. Breakpoints by `pc`, by opcode at an address, and on frame entry. The current breakpoint is line-only, which is useless without source.

### R4. Storage by name and state diff (G4)

1. `sload <slot>` at any step:
   - the value before the transaction comes from the pinned block (`eth_getStorageAt`);
   - writes in the trace apply on top.
2. `p balanceOf[0xB…]`, `p totalBorrow.amount`: the storage layout resolves mapping keys, array indexes and struct members to slots. This works for workspace code, an imported build (R2.3) or Sourcify.
3. State diff per transaction, from `prestateTracer` with `diffMode`: per account, balance, nonce, code and slots before and after. Slots are named when a layout is known.
4. `writes` shows names for every slot the layout can explain, including mappings keyed by values that were not hashed in this trace (from 2).

### R5. Foundry tests in the offline engine (G5)

This work is in the sevm worker (`debugger/`) and the gateway.

1. The `start` parameters gain `pause_at_entry` (default on) and `breakpoints` (`File.sol:LINE` or `*pc`), applied before the first step.
2. Worker stdout, stderr and compile errors become an `output` pane in the envelope, bounded and with credentials redacted. A test that finishes without stopping says why.
3. API:
   - `debugger.launch` takes the breakpoints;
   - `debugger.console` accepts `break` while preparing;
   - `debugger.state` returns the output pane.

Live check `foundry_test`: a test in `contracts/test/` pauses at entry, stops at a preset line, and shows a compile error when one is introduced.

### R6. Fork and compile setup (G6)

1. Identity pinning falls back when block 0 is unavailable. It then pins chain id plus the fork block's number and hash. The status says the genesis check was skipped.
2. "Test RPC" in Network, run before saving, checks:
   - chain id;
   - head;
   - the fork block;
   - `debug_traceCall` support on the replay node;
   - block 0.

   Each check reports its own result.
3. A compile failure caused by two copies of one library names both paths and the remapping that brought each in. This is the OpenZeppelin `Initializable` case.

### R7. Multi-transaction scenarios and comparison (G7)

1. A scenario trace records several transactions in one session:
   - a macro run;
   - a Timeline range;
   - a list of hashes.

   The cursor crosses transaction boundaries, the timeline shows each transaction, and state carries over.
2. Conditional breakpoints and watchpoints:
   - stop when a storage slot or named variable changes;
   - stop when an expression over stack words or variables is true;
   - stop on an opcode with a condition.
3. Trace diff: two saved traces, aligned by frame and source line. It reports the first divergence (step, frame, line), then the storage and balance differences.
4. Console: `nexti` alias, expression history, `p` over variables (R4.2).

The recorded trace stays read-only: `set` and Keep belong to the offline engine, which can mutate its own VM. The console says so.

## Order and size

| Step | Size | Why this order |
|---|---|---|
| R1 preflight and revert explanation | small | Highest daily pain; reuses the existing trace path. |
| R2.4 tolerant matching + R2.1 compiler settings | medium | Unblocks source for most deployed code. |
| R2.2 Auto/Foundry/Chasm config + R6.3 duplicate-library message | medium | Same area; removes the remapping trap. |
| R3 frames and ABI decoding | medium | Needed by R4 and R7 for inner frames. |
| R6.1–6.2 fork identity fallback and Test RPC | small | Removes the first setup failure. |
| R4 storage by name and state diff | medium | Replaces manual `cast index` work. |
| R2.3 import a Foundry build | medium | For contracts built outside the workspace. |
| R5 Foundry test pause | medium | In the worker; separate from the native debugger. |
| R7 scenarios, conditions, diff | large | Builds on R3 and R4. |

Fixtures to add first:
- a via-IR, optimizer, `bytecode_hash = none` contract with immutables;
- an oversized contract;
- a token-insufficient-balance call chain;
- a Foundry test.

Every step ships with unit tests and its live check, gets its own commit on
`UI`, and is recorded in a progress table here.

## Progress

| Step | Status |
|---|---|
| R1 | done: `revert_cause.rs` (first self-reverting frame, reason from all ABIs, oversized creation), deploy preflight keeps the creation call, size/out-of-gas messages via a redacted `chasmKind`, `debugger.trace {call}`; `revert_explained` live flow |
| R2 | R2.1 compiler settings, R2.2 build source (`build_config.rs`: Auto/Foundry/Chasm, `forge config --json`) and R2.4 tolerant matching done; live flows `external_build`, `build_source`. A copy of the review's PoC compiles 123 contracts under Auto. R2.3 done: `foundry_import.rs` reads `out/**/*.json` (deployed code, source map, ABI, AST, storage layout with `extra_output = ["storageLayout"]`) of folders saved under `[build] imports`, on every trace; Compiler and libraries > *Foundry builds for the debugger*, actions `add_import`/`remove_import`; live flow `foundry_import`. Build-info is not needed: each artifact carries what one program needs |
| R3 | done: `call_tree.rs` (frames, kinds, outcomes, ranges), Calls tab with frame jump, `calls` / `frame n`, per-frame `args` via attached or unique ABIs (ambiguous selectors list candidates), `b *pc`, `b op`, `b entry`; `call_tree` live flow |
| R4 | done: `p <path>` resolves variables, mapping keys (nested), array indexes and struct members (`Layout::resolve`/`show`); `sload` and `p` read the value at the cursor from the writes plus the state before the execution, fetched at record time (written slots, base slots, array elements, mapping entries keyed by trace addresses) and saved with the trace; writes follow the storage context (DELEGATECALL writes the caller) and are marked undone by a reverting frame; `diff` lists net named changes and balance changes. Keys never hashed in the trace are named from trace addresses and calldata words. No `prestateTracer`: Anvil returns `{}` for it, so the diff comes from the struct log plus `eth_getStorageAt`. `storage_names` live flow |
| R5 | not started |
| R6 | done: identity pins to the fork block (`anvil_nodeInfo`, marked `chasmAnchor`) when block 0 is pruned; Test RPC (`toolchain::test_rpc`, action `test_rpc`) checks chain id, head, fork block, block 0 and state at the fork block, naming only the host. `debug_traceCall` is not probed: the replay node is a local Anvil, which always serves it. R6.3 duplicate-library message (`duplicate_library_hint`); checks in the `toolchain` live flow |
| R7 | not started |
