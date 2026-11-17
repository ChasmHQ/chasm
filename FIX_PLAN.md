# Fix plan, review round 2 (2026-11-17)

Status: planned, not implemented. Each item lists the cause found in the code.

## 1. Request field loses focus while typing

What happens: after one keystroke the text appears outside the field's focus, and the user must click again.

Cause: `native_contract::presentation` builds a new `fields` model on every paint (`super::model(...)` in the `fields:` entry). Slint recreates every `LineEdit` in `for field in root.view.fields`, so focus is dropped after each edit. This is the same bug fixed for the constructor form in `9c1abaa` (`keep_field_rows`).

Fix:
- Reuse the shown model when the field schema (name + type list) is unchanged. Update only the rows that differ, like `panels::keep_field_rows`. Do this for all four zone projections and `contract_view`.
- Stop writing `text:` back into a focused field while typing: only push `value` when it differs from what the field shows.
- Same check for the macro builder fields (`MacroData.form-fields`) and the value amount field.
- Test: type several characters through `ContractLogic.field-edited` and assert the model handle is unchanged (`ModelRc` pointer equality), as `constructor_callbacks_keep_fields_stable...` does.

## 2. "Missing RPC quantity" on Send

What happens: `withdraw(50000)` fails with "Missing RPC quantity".

Cause: `withdraw` requires `_amount >= 1 ether`, so `eth_estimateGas` reverts. The node answers `{"error":{"code":3,"message":"execution reverted: Minimum Withdrawal is 1 Ether"}}`. `submit_transaction` reads `estimate["result"]` without checking `error`, and `parse_quantity` reports the missing field instead of the revert.

Fix:
- In `submit_transaction` (`contract_service.rs` around line 411), check `error` before `result` for `eth_estimateGas`, `eth_getTransactionCount` and `eth_gasPrice`.
- For estimate errors, decode the revert data: `Error(string)`, `Panic(uint256)`, or custom errors from the contract ABI. Report "Reverted before sending: Minimum Withdrawal is 1 Ether". Nothing is broadcast.
- Offer "Debug this call" from that error. It opens the step debugger on `debug_traceCall` for the same request, so the user lands on the failing `require`.
- Units help: show a hint under uint fields whose name contains `amount`/`value`, and add a "× 1 ether" quick button that fills `1000000000000000000`.
- Test: a mocked estimate error yields the decoded reason and no broadcast; a real Bank `withdraw(50000)` against Anvil yields the message.

## 3. Debugger layout like the sevm TUI

What the user wants: a resizable debugger with small panes like the CLI (`chasm_debugger.tui`), not a fixed three-column page.

sevm TUI panes (`debugger/src/chasm_debugger/tui/panes.py`): Source, Disassembly, Stack, Memory, Storage, Variables, CallStack, CommandLog (console), plus StatusBar.

Plan:
- Make every trace pane a docking pane, so it can be dragged, resized, floated, hidden and stacked with the existing docking (`layout.rs`): `TraceSource`, `TraceOpcodes`, `TraceStack`, `TraceMemory`, `TraceStorage`, `TraceCalls`, `TraceStep`, `TraceConsole`.
- New `Preset::Debugging` default, as in the TUI: Source (center, large), Disassembly (right of source), Stack + Memory stacked (right), Storage + Calls (bottom left), Console (bottom).
- Splitters between every pane; the existing `Splitter` and saved split sizes are reused. Each pane gets compact headers (one line), monospace 11 to 12 px, no padding, like the TUI.
- Console pane: a sevm-style command line over the recorded trace. Commands: `s`/`step`, `n`/`next`, `si`/`stepi`, `fin`/`finish`, `c`/`continue`, `rc` (reverse continue), `b <line>`, `d <n>`, `info breakpoints`, `x/<n> <offset>` (memory), `p stack[<i>]`, `sload <slot>`, `goto <step>`, `find <OPCODE>`. History with Up/Down.
- Toolbar collapses to icons with tooltips when narrow.
- A "Compact" toggle in the debugger header reduces font and row height for small windows.

## 4. Step commands: add single step and document them

Behaviour today (`trace_debug.rs`) and what each name means:

| Button | Key | Meaning |
|---|---|---|
| Step into | F11 | Next step that maps to a different source line, entering calls and internal functions. |
| Step over | F10 | Next different source line at the same call depth or shallower. Calls made by this line run to completion. |
| Step out | Shift+F11 | Run until the current call frame returns to its caller. |
| Opcode | F7 | Exactly one EVM instruction forward. |
| Opcode back | F8 | Exactly one instruction back. |
| Continue / Back | F5 / Shift+F5 | Run to the next / previous breakpoint, or the end / start. |

Gaps to fix:
- "Step over" uses call depth, so internal Solidity functions (JUMP, same depth) are entered, not stepped over. Use the source map jump type (`i` into function, `o` out of function) to track internal calls: Step over skips a whole internal call, and Step out leaves an internal function too.
- Add "Step back" (Shift+F11 is used; use Shift+F10 and Shift+F11 as the reverse of Over and Into, Ctrl+Shift+F11 for reverse Out) so every move has a reverse.
- Name the single-instruction buttons "Step instruction" and "Step instruction back" to match sevm `stepi`.
- Hover text on each button states the meaning from the table.
- Document it in `DEBUGGER.md` (new), Help > Keyboard shortcuts, and the AI control catalog descriptions.

## 5. Where captures and Foundry test runs go

What happens: after using "Captures and Foundry tests" there is no visible capture.

Cause:
- The offline engine only loads captures: the bundled synthetic examples (`debugger/examples/web-debugger/source-replay.json` and `build-info.json`) or files uploaded into the gateway's private temp dir (`src/debugger/mod.rs`, `resources_dir`, a `tempfile::TempDir` deleted when Chasm closes).
- A Foundry test run is debugged live and never written as a capture.
- So nothing is saved where the user can find it.

Plan:
- Save every debug session as a capture under the project: `.chasm/captures/<timestamp>-<label>.json` (the sevm capture format from `fork/capture.py` `save_capture`), with the build-info next to it.
- Step debugger sessions save their recorded trace in the same folder (`.chasm/traces/<tx-hash>.json`), so they reopen without the node.
- The Debugger sidebar gets a "Saved" list of those files, with Open, Reveal in folder and Delete.
- After a run, the status line names the exact file path.
- The offline engine's "Synthetic" and "Withdrawal lab" stay, labelled as built-in examples.

## Order

1. Field focus (1) and revert message (2). These are small and the user hits them on every call.
2. Step semantics and docs (4).
3. Capture and trace saving (5).
4. Dockable debugger panes and the console (3), the largest change.

Each item ships with tests, a live run through the control API and screenshots, and is committed separately.

## Progress

| Item | Status |
|---|---|
| 1. Field focus | Done (`c9fbb89`) |
| 2. Revert reason on Send | Done: decoded reason, nothing broadcast, Debug this call, ether to wei button. Also fixed CRLF source files mapping steps to the wrong line |
| 4. Step semantics and docs | Done: Over/Out handle internal functions, every step has a reverse, Step instruction naming, hover help, `DEBUGGER.md`. Lines of solc's generated helpers are no longer shown as your source |
| 5. Capture and trace saving | Done for the step debugger: `.chasm/traces/`, Saved traces list (open, show in folder, delete), path in the status line. Offline engine examples are labelled as built-in; saving its sessions as captures needs a replay baseline and stays open |
| 3. Dockable debugger panes and console | Done inside the Debugger view: resizable Source / Opcodes / State / Console with splitters, pane toggles, Compact mode, sevm-style console with history. Panes are not yet separate docking-system panes (floating/moving to other zones) |

| G1. Address labels | Done: Anvil accounts, the signer and deployments (numbered when repeated) are named in Explorer lookups and lists, call results, storage values and the debugger stack, storage and call frames. Workspace selectors show as function names in Explorer |
| G2. Saved History | Done: every Send and Simulate goes to `.chasm/history.jsonl` (500 kept, public addresses only). Explorer > History lists them with All / Sends / Failures, Repeat (refills the tab), Debug (mined tx, or replays a call) and To macro (raw step in the selected macro). API: `history.list`, `history.action` |
| G3. Decoded variables | Done for arguments and storage: a Variables tab names every SSTORE by variable (mappings, arrays, packed fields, struct members via keccak preimages from the trace) with old → new and an ether hint, plus `args` / `writes` console commands. Not done: Solidity local variables on the stack (needs the compiler's local variable scopes, which solc does not emit in standard JSON) |

## Goals after round 2 (agreed 2026-11-17)

In order of impact. Start after the five items above are done.

1. Address labels everywhere. An automatic address book ("Anvil #0", "Bank", "Bank.owner") used by Explorer, trace, storage and call results instead of raw addresses.
2. Saved, repeatable History. Per-project record of every call, transaction and result, with Repeat, Debug and "Kirim ke macro". This replaces the session-only History planned in Phase 5.
3. Decoded variables in the debugger. Locals, function arguments and per-step storage changes shown by name (`balances[0xf39…]: 0 → 1 ether`). Build it together with the dockable panes (item 3 above), since both touch the same code.
4. Snapshot timeline and cheatcodes (Phase 3). Warp, roll, deal, prank and "Revert ke sini" per transaction. This was lost when the web UI was removed.
5. Solc and library manager (Phase 4). Needed before real repositories that import OpenZeppelin compile reliably.
6. Mainnet fork. Fork at a block and debug a real transaction. The largest item; it also covers the mainnet address history TODO.
7. Consistent UI patterns: a command palette (Ctrl+K) to jump to a contract, function or transaction; one error pattern that always offers the next action; empty states that name the next step.

Suggested order: 1 and 2 first (they improve every view), then 3 with the dockable panes, then 4, 5, 6. Item 7 is applied along the way.
