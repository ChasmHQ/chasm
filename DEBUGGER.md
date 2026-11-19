# Debugger

The Debugger activity steps through a transaction or call that ran on the
workspace's own Anvil node. Every opcode is recorded once, with its stack,
memory and storage, and mapped to your Solidity source. Moving backwards costs
nothing, so every step command has a reverse.

## Open something to debug

- **After a Send or Simulate:** in the contract's Response pane, press
  **Debug steps**.
- **After a Send that reverted before broadcast:** under the request, press
  **Debug this call**. The call is replayed as the same sender, and the
  debugger opens on the failing `require`.
- **A mined transaction:** paste its hash in the Debug bar and press **Debug**,
  or pick it from *Recent transactions on this node*.
- **The newest action:** leave the Debug bar empty and press **Debug**.

## Where recordings are saved

Every transaction or call you debug is saved in the project:

- `.chasm/traces/<transaction hash>.json` for a mined transaction.
- `.chasm/traces/<unix time>-<title>.json` for a call (Simulate, or a Send that reverted before broadcast).

The status line names the file after each recording. The file holds the node's opcode trace and the code at each address it entered, so it reopens later without the node, after a restart, or after the chain was reset. Source lines are read again from the workspace, so edit nothing between recording and review if you want the lines to match.

The Debugger start screen lists them under **Saved traces**, newest first, with **Show in folder** and **Delete**. `.chasm/` is ignored by git.

The offline engine (*Captures and Foundry tests*) saves nothing to the project. *Synthetic* and *Withdrawal lab* are built-in examples shipped with the debugger, in `examples/web-debugger/`. A Foundry test run is debugged live and is gone when the session closes.

The source pane shows the contract whose code is running. Code that is not a
workspace contract (another deployment, a precompile) shows as `no source`,
with opcodes only.

## Transactions from a forked chain

With the project forked (Workspace > Compiler and libraries > Network), paste a hash from the real chain.

- Chasm starts a separate replay node forked at the block before the transaction and replays the transaction there as a call. The project node and its deployments are not touched.
- The replay node keeps the real chain id, so contracts that check `block.chainid` (EIP-712 signatures, permits) behave as on chain. Chasm never signs on it.
- The replay runs with the mined block's context (number, timestamp, base fee, coinbase, gas limit, prevrandao) and the transaction's own fees.
- It is exact for the first transaction of a block. For a later one, the transactions before it in the same block did not run first, and the status line says so.
- Contracts that are not in the workspace get their verified source from [Sourcify](https://sourcify.dev) (no API key), cached in `.chasm/sources/<chain>/<address>.json`. The file that declares the contract is listed; code in its imports and unverified contracts show opcodes only. Saved traces reopen with the cached source.
- Some MEV bot transactions check their position in the block or the builder's balance. Replayed alone they can revert even though they succeeded on chain.

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
| `x <offset> [bytes]` | | Memory bytes, 32 by default |
| `sload <slot>` | | Storage value of a slot touched so far |
| `args` | | Decoded arguments of the call |
| `writes` | | Storage writes up to the cursor, by slot |
| `where`, `help` | | Current position, command list |

## State panes

- **Stack:** top first. Changed entries are highlighted.
- **Memory:** 32-byte words.
- **Storage:** slots touched so far in this frame's contract.
- **Calls:** the external call stack at the cursor.
- **Variables:** the decoded arguments of the call, then every storage write by variable name with its old and new value, for example `balanceOf[0xf39F… (Anvil #0, signer)]  0 → 2000000000000000000 (2 ether)`. Writes after the cursor are marked *later*; the write at the cursor is highlighted. Names come from the compiler's storage layout: mapping keys and dynamic array indexes are recovered from the hashes computed in the same transaction, packed variables are split, and slots the layout cannot explain show as `slot 0x…`.
- Known addresses are labelled everywhere (Anvil accounts, the signer, workspace deployments).
- **This step:** the opcode, gas left and cost, depth and code address. At the
  last step it also shows the return or revert data.

## Control API

AI clients with the `debug` scope use the same commands:

- `debugger.trace` with `{"hash": "0x…"}`, or `null` to debug the newest action.
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
- `debugger.trace.view` with any of `opcodes`, `state`, `console`, `compact` (booleans) and `opcodes_width`, `state_width`, `console_height` (px).
- `debugger.trace.open_saved` with `{"name": "<file>.json"}`, a name from `debugger.state` `trace.saved`.
- The `debugger.state` query returns the trace position under `debugger.trace`.

## Offline engine

*Captures and Foundry tests* in the Debugger sidebar opens the separate sevm
engine. It replays capture files and Foundry test runs without a node.
