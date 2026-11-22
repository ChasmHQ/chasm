# Debugger

In the **Debugger** sidebar, choose **Last transaction or call** or **Choose a transaction** for the native opcode tracer. It lets you step backward through recorded state without rerunning the transaction. Solidity lines appear only where the debugger can match running code to a source map. **Captures and Foundry tests** opens a separate worker; native traces do not need it.

## Open something to debug

- **After a Send or Simulate:** in the contract's Response pane, press
  **Debug steps**.
- **After a Send that reverted before broadcast:** under the request, press
  **Debug this call**. The call is replayed as the same sender, and the
  debugger opens on the failing `require`.
- **A mined transaction:** paste its hash in the Debug bar and press **Debug**,
  or pick it from *Recent transactions on this node*.
- **The newest action:** leave the Debug bar empty and press **Debug**.

## When a trace reverted

The status line names where the failure started:

- the deepest frame that reverted on its own, not the outer frames that only passed the revert up;
- its function, from the running contract's ABI or any workspace or verified ABI;
- the reason: `Error("…")`, `Panic(code)` with its meaning, or a custom error decoded from any known ABI.

The cursor opens on that step. A deploy that fails its gas estimate is traced as a creation call. Its message names the rule when it is one:
- creation code over 49,152 bytes (EIP-3860);
- runtime code over 24,576 bytes (EIP-170);
- out of gas.

## Where recordings are saved

Every transaction or call you debug is saved in the project:

- `.chasm/traces/<network>-<transaction hash>.json` for a mined transaction.
- `.chasm/traces/<network>-<unix time>-<title>.json` for a call (Simulate, or a Send that reverted before broadcast).

`<network>` is the profile slug (`local`, `mainnet-21000000`); a local chain adds a short session tag, so two recordings never overwrite each other even when Anvil repeats a hash after a restart. An existing file is never clobbered: a collision gets a numbered suffix. A trace records the network it was made on, reopens with that chain's Sourcify cache, and says so in the status line when the network that runs now is a different one. `compare` accepts a saved file name, or a bare transaction hash when exactly one recording matches it.

The status line names the file after each recording. The file holds the node's opcode trace and the code at each address it entered, so it reopens later without the node, after a restart, or after the chain was reset. Source lines are read again from the workspace, so edit nothing between recording and review if you want the lines to match.

The Debugger start screen lists them under **Saved traces**, newest first, with **Show in folder** and **Delete**. `.chasm/` is ignored by git. A `contracts.attach` ABI is used to name a frame on the network the trace came from; on another network it is used only when the code the frame ran matches the contract's own runtime.

The **offline engine** (*Captures and Foundry tests*) understands Ethereum mainnet only. On another chain use the native trace, or export a capture and inspect it here with that limitation in mind.

The offline engine (*Captures and Foundry tests*) saves a paused replay with **Save capture** to `.chasm/captures/<unix time>-<label>.json` (a `chasm-fork-capture` bundle: plan, baseline and the remote-state witness, without RPC credentials). With *Offline capture* selected, the saved captures are listed; **Open** uploads one to the local gateway and **Start offline capture** replays it without the network. *Synthetic* uses `debugger/examples/mainnet/synthetic-replay.json`. *Withdrawal lab* uses the capture and build info in `debugger/examples/web-debugger/`. Both ship with the offline worker. Synthetic and Foundry test runs have no replay baseline, so there is nothing to save; a Foundry test run is debugged live and is gone when the session closes. API: `debugger.capture.save`, `debugger.capture.open {name}`; `debugger.state` lists `captures`.

The source pane matches running code against workspace builds, added Foundry builds and available Sourcify programs. If there is no matching program or source location for a recorded step, inspect the opcodes instead.

## Transactions from a forked chain

With the project forked (Workspace > Compiler and libraries > Network), paste a hash from the real chain.

- Chasm starts a separate replay node forked at the block before the transaction and replays the transaction there as a call. The project node and its deployments are not touched.
- The replay node keeps the real chain id, so contracts that check `block.chainid` (EIP-712 signatures, permits) behave as on chain. Chasm never signs on it.
- If the upstream block lookup succeeds, replay supplies its number, timestamp, base fee, coinbase, gas limit and prevrandao as overrides. Without the block response, replay can proceed without those overrides.
- The parent block supplies the starting state. Replay omits earlier transactions in the same block, so matching the chain ID and block context does not guarantee the original outcome.
- Replay uses the profile's Optimism mode and hardfork override. On OP Stack, earlier deposits (`0x7e`) are reported separately from other omitted transactions, and the trace does not reproduce L1 data fees. On Arbitrum, the block-number override uses the RPC block number, not ArbOS's L1 block number. The trace flags calls to ArbSys, ArbGasInfo and NodeInterface because Anvil does not implement them.
- When Chasm knows the source chain ID, it looks up at most 12 unmatched addresses with runtime code per trace using [Sourcify](https://sourcify.dev). It caches answers, including not-found responses, in `.chasm/sources/<chain>/<address>.json`. Source display needs matching runtime, a usable source map and the declaring file. Steps in imported files can still show only opcodes. Saved traces use the local cache.

## Step commands

A *frame* is either an external call (`CALL`, `STATICCALL`, `DELEGATECALL`)
or an internal Solidity function call. Internal calls are found from the
compiler's source map: a `JUMP` marked `i` enters a function, one marked `o`
returns from it.

| Command | Key | Reverse | Reverse key | What it does |
|---|---|---|---|---|
| Continue | F5 | Back | Shift+F5 | Run to the next breakpoint, or the end. |
| Step into | F11 | Into | Ctrl+F11 | Next source line, entering any call or internal function the line makes. |
| Step over | F10 | Over | Ctrl+F10 (or Shift+F10) | Next line in this frame. Calls and internal functions on the line run to completion. |
| Step out | Shift+F11 | Out | Ctrl+Shift+F11 | Run until this frame returns, then stop in its caller. Reverse: back to the step that entered the frame. |
| Step instruction | F7 | Instruction | F8 | Exactly one EVM instruction. |
| First / last step | Home / End | | | Jump to the start or end of the trace. |

### Step into, over and out on an example

```solidity
function withdraw(uint256 amount) public {
    _check(amount);            // A
    balance[msg.sender] -= amount; // B
}
function _check(uint256 amount) internal view {
    require(amount >= 1 ether); // C
}
```

Starting on line A:

- **Step into** goes to line C, inside `_check`.
- **Step over** goes to line B. `_check` runs, but the debugger does not stop
  in it.
- From line C, **Step out** stops back in `withdraw`, right after the call
  returns.

If `_check` reverts, Step over stops on the reverting line, because the
trace ends there.

## Breakpoints and search

- Click a line number in the source pane to set or clear a breakpoint.
  Continue and Back stop when execution reaches that line.
- The number at the right of a line counts how many steps ran on it.
- **Find** jumps to the next or previous step with an opcode, for example
  `SSTORE`, `CALL`, `LOG2` or `REVERT`.

## Layout

- Drag the bars between Source, Opcodes and State, and above the Console, to resize them. With a bar focused, the arrow keys move it in 20 px steps.
- **Panes** in the second toolbar row hides or shows Opcodes, State and Console.
- **Compact** uses smaller rows and fonts for small windows.
- **Float** and **Dock** on the Opcodes, State and Console headers move a pane out of the view: into its own window, or into a dock zone next to the source. It then moves, stacks and maximizes like any other pane, and the place is saved with the layout. Hiding it (or Panels > Reset layout) puts it back in the view.
- **Panels > Trace debugger layout (sevm)** arranges them like the sevm TUI: opcodes left, source in the middle, machine state right, console below.

## Console

The console at the bottom takes sevm-style commands over the recorded steps. Up and Down recall earlier commands.

| Command | Reverse | What it does |
|---|---|---|
| `s`, `step` | `rs` | Step into |
| `n`, `next` | `rn` | Step over |
| `si`, `stepi` | `rsi` | Step instruction |
| `fin`, `finish` | `rfin` | Step out |
| `c`, `continue` | `rc` | Run to a breakpoint |
| `b <line>` / `d <line>` / `d` | | Set, delete, or delete all breakpoints |
| `info b` | | List breakpoints |
| `goto <step>` | | Jump to a step number (1-based) |
| `find <OP>` | `rfind <OP>` | Next step running an opcode |
| `p stack[i]`, `p $i`, `p stack` | | Stack words, 0 is the top |
| `p <variable>` | | A state variable at the cursor, by Solidity path: `p owner`, `p balanceOf[0xf39F…]`, `p allowance[0xA…][0xB…]`, `p list[2]`, `p position.amount`. A struct lists its members. |
| `x <offset> [bytes]` | | Memory bytes, 32 by default |
| `sload <slot>` | | A raw slot of the running account at the cursor |
| `args` | | Decoded arguments of the call |
| `writes` | | Storage writes up to the cursor, named by the layout; writes a reverting frame undid are marked |
| `diff` | | Net effect of the execution: each changed slot by name, before and after, then balance changes |
| `locals` | | Solidity locals of the function at the cursor |
| `gas`, `gas lines` | | Gas per call frame, or per source line |
| `calls`, `frame <n>` | | The call tree; jump to a frame's first step |
| `b *<pc>`, `b op <OP>`, `b entry` | `d …` | Break on a pc in any contract, on an opcode, or on entering any call. These need no source. |
| `b <line\|*pc\|op OP> if <cond>` | | Stop there only where the condition holds |
| `b if <cond>` | `d if` | Stop where a condition becomes true |
| `watch <variable>`, `watch slot <n>` | `unwatch …` | Stop where a write changes the value (in the account running at the cursor) |
| `ni`, `nexti` | | One instruction, stepping over calls |
| `txs`, `tx <n>` | | The transactions of a scenario; jump to one |
| `compare <file>` | | First divergence and effect differences against a saved trace; `compare` alone lists them |
| `where`, `help` | | Current position, command list |

## State panes

- **Stack:** top first. Changed entries are highlighted.
- **Memory:** 32-byte words.
- **Storage:** slots touched so far in this frame's contract.

### Foundry builds from another folder

Add a built project folder (the one with `foundry.toml`) in Workspace > Compiler and libraries > *Foundry builds for the debugger*, or with `toolchain.action {action: "add_import", folder}`. Chasm saves the folder under `[build] imports` in `.chasm/chasm.toml`. This adds local artifacts for source matching; it does not import a verified address or bind a contract tab.

Chasm scans `[profile.default].out` (default `out`) to depth 3, considers at most 5,000 JSON artifacts per project, skips `build-info` directories and skips files larger than 16 MiB. It needs deployed code, a source map, the compilation target and readable source text. ABI, AST and storage layout are used when present. Rebuild the external project after editing its source. Workspace programs are considered first.

### Conditions

A condition compares numbers, stack words (`$0` is the top, or `stack[i]`), `pc`, `gas`, `depth` and state variables of the code running at the step (`balanceOf[0x…]`, `total.amount`), with `==`, `!=`, `<`, `<=`, `>`, `>=`. Comparisons join with `&&` and `||`, left to right. A condition that cannot be read at a step (no layout, a value not known yet) does not stop there.

### Scenarios: several transactions in one session

`debugger.trace {"hashes": ["0x…", "0x…"]}` records up to 16 mined transactions and opens them as one session. `{"macro_run": true}` uses the available transaction hashes from the selected run result. If the node no longer has a transaction after rollback or restart, the scenario cannot record it. This view follows the supplied order; it does not certify mining order or that all receipts belong to one block. Storage carries over: a slot's value in a later transaction includes the earlier writes, and a transaction that reverted keeps none. Every view works across the boundary. The Calls tab has one `tx n` frame per transaction, `txs` lists them, `tx <n>` jumps to one, and `where` shows `tx i/n`. A scenario is not saved; each transaction debugged on its own is.

### Comparing traces

`compare <file>` compares the open trace with one saved in `.chasm/traces/`. It reports the first step where the two take different paths (by code, pc and call depth), with the source line and frame of each. Then it lists the outcome and gas of both, and every storage slot (by name when the layout knows it) and balance whose net change differs.

The recorded trace is read-only. `set` and Keep belong to the offline engine, which runs its own VM.

### Storage at any step

When a trace is recorded, Chasm reads the state before the execution from the node: every slot the execution wrote, each state variable's base slot, array elements, and mapping entries keyed by an address or argument from the trace. A mined transaction reads the parent block, and its balance changes are also read. These values are saved with the trace, so `p`, `sload` and `diff` work on a reopened trace without the node.

The value at the cursor is the value before the execution, plus every write up to the cursor. A write inside a frame that reverted is undone when that frame ends. A DELEGATECALL writes the caller's storage: the slot belongs to the caller, and the library's layout names it. A slot that was never read, written or fetched shows as unknown, never as zero.
- **Calls:** the external call tree. Each frame shows its kind, `Contract.function`, value, how it ended (returned, reverted, stopped, out of gas) and its steps. The frame holding the cursor is highlighted; click a frame to jump to its first step.
  - Frames are named with the matched contract's ABI, an ABI attached to that address in a contract tab, or a workspace ABI when exactly one declares the selector.
  - If several do, the frame lists the candidates; attach the address to choose.
- **Variables:** the Solidity locals of the function at the cursor (parameters, return variables and locals in scope, with value types, memory strings and arrays, and calldata arrays decoded), then the decoded arguments of the call, then every storage write by variable name with its old and new value, for example `balanceOf[0xf39F… (Anvil #0, signer)]  0 → 2000000000000000000 (2 ether)`. Writes after the cursor are marked *later*; the write at the cursor is highlighted. Names come from the compiler's storage layout: mapping keys and dynamic array indexes are recovered from the hashes computed in the same transaction, packed variables are split, and slots the layout cannot explain show as `slot 0x…`.
- `locals` uses the matched build's AST and stack positions. A source line alone is not enough: Sourcify programs have no AST, compiler-generated steps can lack a source mapping, and optimized or via-IR builds can move variables off the expected stack slots. The console explains why it cannot reconstruct locals at the cursor.
- **Gas:** where the execution gas went, per external call (`Contract.function`, named from the ABI or the selector) and per source line, most expensive first. Each step counts its own cost; a call's forwarded gas is counted in the callee. Intrinsic gas and refunds are not included. The console's `gas` and `gas lines` print the same tables.
- Known addresses are labelled everywhere (Anvil accounts, the signer, workspace deployments).
- **This step:** the opcode, gas left and cost, depth and code address. At the
  last step it also shows the return or revert data.

## Control API

AI clients with the `debug` scope use the same commands:

- `debugger.trace` with `{"hash": "0x…"}`, or `{}` to debug the newest action, or `{"call": {"to", "data", "value", "from", "block"}}` to trace a call that was never sent (omit `to` for a creation). Nothing is signed.
- `debugger.trace.step` with `action` set to one of:
  - `continue`, `back`
  - `step`, `step-back`
  - `next`, `prev-next`
  - `finish`, `finish-back`
  - `op`, `op-back`
- `debugger.trace.seek` with `{"step": n}`.
- `debugger.trace.breakpoint` with `{"line": n}`.
- `debugger.trace.console` with `{"line": "n"}`; output is in `debugger.state` `trace.console`.
- `ui.panel` with `pane` = `trace_ops`, `trace_state` or `trace_console` and `action` = `dock` (with `zone`), `float` or `hide`; `action` = `preset` with `pane` = `tracing` applies the sevm layout. `app.state` lists each pane's placement under `layout.panes`.
- `debugger.state` `trace.calls` lists the call tree (while the Calls tab, `tab: 3`, is shown).
- `debugger.trace.view` with any of `opcodes`, `state`, `console`, `compact` (booleans), `tab` (0 Stack, 1 Memory, 2 Storage, 3 Calls, 4 Variables, 5 Gas) and `opcodes_width`, `state_width`, `console_height` (px). `debugger.state` `trace.variables` and `trace.gas` are filled while their tab is shown.
- `debugger.capture.save` and `debugger.capture.open` with `{"name": "<file>.json"}` for the offline engine's captures (see below).
- `debugger.trace.open_saved` with `{"name": "<file>.json"}`, a name from `debugger.state` `trace.saved`.
- The `debugger.state` query returns the trace position under `trace`.

## Offline engine

*Captures and Foundry tests* in the Debugger sidebar opens the separate sevm
engine. It replays capture files and runs trusted Foundry tests. A binary in its Cargo build profile looks for the worker in the build checkout's `debugger/`. An installed binary looks in `debugger/` beside the executable on Windows or `<prefix>/share/chasm/debugger/` on Unix. It does not search the open Solidity project or a neighboring checkout. Interpreter precedence is `--debugger-python`, then `CHASM_DEBUGGER_PYTHON`, then the worker directory's `.venv` interpreter. If only the worker is missing, use native tracing or install the worker as described in [the README](README.md#build-from-source).

### Foundry tests

*Trusted Foundry* runs a test file inside the workspace (its folder needs a `foundry.toml` with a pinned, installed solc).

- **Breakpoints (optional)**, comma separated: `Vault.sol:42`, `*0x1a4` or a function name. They are set before the test's first step.
- **Pause at the test's first line** (default) or **Run to the first breakpoint**.
- The **Output** tab lists what the worker reported: the test being debugged, each breakpoint it set or could not set, how the run ended (passed, failed, or finished without reaching a breakpoint) and, when the start fails, the compile error. Endpoints are redacted; the worker's raw stdout and stderr are never shown.

API: `debugger.launch {mode: 1, foundry_target, foundry_match, trusted: true, breakpoints, run_to_breakpoint}`, then `debugger.start`; `debugger.state.output` holds the Output text.
