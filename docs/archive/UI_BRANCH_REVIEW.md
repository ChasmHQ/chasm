# UI branch adaptation

Reviewed2026-11-13. Chasm remains the Rust/Slint native application. The upstream UI branch is a reference for selective UX work, not a replacement for the application's Cargo workspace or a WebView migration.

## Compared revisions

| Reference | Commit |
|---|---|
| Local UI and integration/web-debugger before fetch | `6e69106870edf011a4a498c532290a9a52cd3ca1` |
| Fetched origin/UI | `4ca4d68d31ed2dd909d33d540914fc5584e46363` |
| Upstream web UI addition | `0b403f09d653daa3df177f4dd37e9883b93301bc` |

The cached local refs initially matched; fetching revealed the newer upstream changes. The fetched delta contains a Rust/Python debugger workspace, CLI/engine/TUI/tests and new root manifest in addition to UI.html, web.html and web.py. A wholesale merge would conflict with the native application and its existing uncommitted integration. No checkout, merge, reset or upstream instruction adoption was performed.

## What is reused

| Reference behavior | Native adaptation |
|---|---|
| Source/disassembly with current PC | Native code viewport, current row and Back to PC |
| Frames, locals, storage and command result panes | Separate native inspector and low-level inspection tabs |
| Continue, opcode/source step, next, finish and restart | Typed existing gateway navigation, capability/lifecycle gated |
| Command log | Separate evaluation result and application Logs/Jobs dock |
| Textual code-following and manual inspection | Follow execution only in code; other inspectors keep their independent selection |
| Browser command prompt | Not copied; native exposes bounded typed operations and discard-only Solidity evaluation |

### 2026-11-14 port from `0b403f0`

The fetched `origin/UI` delta is 119 commits. 117 of them are the debugger engine history (REVM migration, stepping fixes, checkpoints, MCP) and already exist in the sibling `debugger` repository, so they are not re-applied here. Only `0b403f0` (`UI.html`, `web.py`) is new UI work. It is not merged. These behaviors were ported into the native workbench:

| `0b403f0` behavior | Native adaptation |
|---|---|
| F5/F7/F8/F9 debugger keys with key hints | F5 Continue, F7 Step opcode, F10 Next line, F11 Step line, Shift+F11 Finish, F9 Breakpoint here. Hints are shown on the toolbar when it is wide enough. Keys act only in the Debugger activity while a paused session accepts commands. |
| Breakpoint button at the current location | **Breakpoint here** adds a breakpoint at the paused `File.sol:LINE` through the existing typed `breakpoint add` operation. It is disabled on bytecode-only stops. |
| EXECUTION card (PC, gas, depth, opcode, step) | Execution card above the Inspector tabs. It wraps to more rows when narrow. |

The browser server, the free-form command prompt and the `--web` CLI flags were not copied. The native console and typed operations already cover them within the existing control boundary.

The existing React debugger is also a behavior reference for session-envelope ordering and for separating baseline receipt agreement, current EVM outcome, worker completion and divergence. These indicators are not combined into a single verified flag. Offline captures may be synthetic.

## Native scope

The workbench uses the existing owned loopback gateway and isolated REVM worker. Cookie/CSRF, generation/revision/sequence, command IDs and worker termination remain the control boundary. The production AI facade does not automatically gain debugger command access.

The first native launch remains the bundled synthetic WorkshopVault preset. Complete launch forms for capture upload, historical replay, fork call and trusted Foundry targets remain separate roadmap work. Frames are read-only inspection, not selected-frame evaluation context. The last-stop diff is not a full trace, reverse navigation or restorable checkpoint.

Runtime capabilities decide source stepping, Solidity evaluation and Restart. Unsupported controls stay disabled. Evaluations use `keep=false`; no arbitrary console or mutating local-write controls are added by this increment.

Historical engine limits remain Ethereum mainnet Cancun (`1710338135 <= timestamp < 1746612311`). Arbitrary Anvil31337 reconstruction and current post-Cancun replay are not delivered. Anvil workspace state and debugger worker state are separate.

## Repository skill

The canonical [chasm-bugbounty package](.agents/skills/chasm-bugbounty/README.md) is versioned with the repository. Its dated schema/source snapshot describes the API checkpoint, not every future UI revision. Runtime schemas take precedence. Installing the skill neither configures MCP nor grants target authorization. Existing personal installations are not overwritten automatically.

## Qualification

Native integration is verified using synthetic local gateway data, not external chain providers or real keys. The final suite passed169 tests and the normal default build succeeded. Tests cover stale responses, cancellation, malformed launch recovery, real gateway stepping and discard evaluation. Actual desktop views were exercised at1360×820 and100/110/125/150% zoom; the repaired150% source, inspector, calldata and evaluation captures passed designated visual review. A separate40-image software-testing render matrix covers1180×720 and1360×820 but does not establish native input. Minimum-size horizontal-control reachability and exhaustive scroll continuity remain partially qualified. Build success alone is not visual acceptance or complete R1-R6 delivery.
