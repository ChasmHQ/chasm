# Native workspace

The Rust/Slint desktop uses one application state and one owned debugger session. Floating panes are independent native windows, not copies of a worker or contract request.

## Arrange the workspace

Open **Panels** or **Layout** for Contracts, Debugging and Investigation presets, hidden-pane recovery, and **Reset layout / Bring panels back**. A preset replaces the panel arrangement without discarding document drafts or debugger state. Contracts starts with the output group hidden to reserve room for the contract; open Console, Logs or Jobs when needed. Debugging hides the launch Navigation pane and reserves more output height so code, memory and the console have useful reading space. Navigation is still available from Panels.

Each panel header offers Undock, position and Hide. The position menu supports left, center, right and bottom groups, tab order, and panel maximize/restore. Header or tab grips can move a pane into an existing visible group; the menu also works when a destination group is empty. Splitters support pointer dragging and keyboard arrows. Native floating windows use the operating system's resize, maximize and close controls. Closing a floating window returns it to its remembered dock; Hide is separate.

Panel positions and sizes are saved after a short idle delay and at normal shutdown. The versioned layout contains geometry and opaque pane identities, not passwords, transaction keys, drafts or console history. Missing document owners from an earlier workspace are discarded. Reset restores a recoverable arrangement. Logical client size, physical position, OS display scale and application zoom are distinct.

## Bank and other contract documents

Deployment names remain the primary labels. The original address is not rewritten: each Bank document binds its contract, deployment identity, network epoch and address. Multiple instances use Bank, Bank 2 and similar tab labels; inspect **Details** to verify the full address and copy it. **Attach** explicitly changes only that document's target.

A Bank document contains **Interact, Storage and Details**. At compact sizes its Result view gets a separate tab so request fields and results are not squeezed into simultaneous unreadable strips. Wider and taller layouts use a resizable request/result split. Per-document arguments, value, slot, selection, results and scroll positions survive view changes and panel moves for the current session.

**Open storage** navigates to Bank's Storage section; it does not read a slot automatically. **Read slot** performs the explicit read and retains pinned block provenance. This is a raw slot reader, not a decoded storage-layout explorer.

### Simple, Raw JSON and Trace

- **Simple** is the default after a completed call. Function outputs use the full captured ABI, including exact uint256/signed values, arrays, tuples and supported revert data. A read-only function uses **Read / Simulate**, not Send.
- A **Send** result shows the transaction receipt and supported decoded events. A receipt is not a function return value. No second call is silently made to invent one.
- **Raw JSON** contains captured public context, request and response. Signed payloads are omitted; provider error messages and unsafe extra error fields are explicitly redacted. Redaction metadata distinguishes this from an unmodified wire dump.
- **Trace** loads only on demand for the captured result. The selected node must support `debug_traceCall` or `debug_traceTransaction` with `callTracer`. Requests use the captured call/block or transaction hash, with source/receipt checks before and after. The call tree is formatted locally; only the captured ABI for the known target supplies names. Unknown selectors remain unknown. Raw event topics/data remain available in the tree.

Trace RPC responses are capped at 1 MiB with a 10-second request timeout. Rendering is capped at 32 levels, 4096 nodes and 1 MiB text. Unsupported tracing is reported without falling back to latest, a different endpoint, an external signature service or another transaction. This read-only call tree is not a claim of verified historical debugger replay.

Copy and Save follow the active result view. Separate result/storage panes retain their owning Bank. **Return to contract** embeds that pane back into the correct document; the general header Dock control instead docks the standalone pane into a group.

Native signing is restricted to Chasm's owned local Anvil process and listener on chain 31337. Keys are used in-process, not passed in a `cast --private-key` command line. Unknown submission/receipt outcomes retain the public transaction hash and are labeled unknown; inspect the transaction before considering a retry.

## Internal debugger console

The Console controls the existing debugger session through the same typed operation gate as the toolbar. It is not cmd, PowerShell, Python or a general CLI shell, and opening it does not start a second worker.

Supported examples:

```text
help
si
step 2
next
continue
finish
until WorkshopVault.sol:20
p feeBps
ptype feeBps
bt
info locals
info args
info registers
info gas
info logs
info storage
info breakpoints
list 10
disas *32
x/32xb 0
break WorkshopVault.sol:20
breakop SSTORE
watch feeBps
delete 1
history
clear
cancel
```

`run`/restart requires the session capability. Evaluation is discard-only. The memory command requests a bounded snapshot window; it is not complete GDB formatting. Selected-frame commands, nexti, `$N` history, file/environment/FFI commands, Keep, Yul and vm mutations are not silently mapped to other operations.

Enter executes only the focused prompt. Multiline input is rejected rather than batch-executed. Up/Down navigate command history; Ctrl+Space completes a unique supported prefix and Tab remains available for focus navigation. Follow output can be turned off for scrollback. Transcript copying is explicit. History and transcript are session-local and bounded (50 history commands, 100 entries, 64 KiB stored text and 16 KiB per result).

## Investigation and local security

Investigation, Repeater and Cases are nonmodal workspace content and can use the same panel hosts. Their controller, drafts, selection and async results are shared, including when a request finishes after a floating host redocks. Offline cases remain usable from Welcome; a chain read still requires a selected workspace node.

Project replacement, keystore deletion and AI approvals remain controller confirmations. Pending confirmation or native file-dialog work blocks relevant actions in all hosts. A dedicated Cancel worker action remains independent. Wallet password/private-key drafts are cleared, not copied into panel state, when the Wallet pane moves.

The production API/MCP does not expose a free-form console, signing, approval acceptance, generic widget actions or the new pane placement controls. Existing semantic `ui.layout` still means sidebar/zoom settings. See [AI_CONTROL.md](AI_CONTROL.md) for the versioned remote capability boundary.

## Qualification

This document describes the implementation, not a claim that every layout has been accepted. Build/tests and native interaction evidence are recorded separately. In particular, software-render tests do not prove OS pointer, focus or window-manager behavior. See `.zcode/general-manager/contract-docking-contract-console.md` for the comfort acceptance criteria and `NATIVE_PARITY.md` for the remaining roadmap.
