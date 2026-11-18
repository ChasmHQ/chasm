# Native parity delivery checklist

Approved roadmap M0-M12, implementation started 2026-11-13. This tracks actual acceptance, not a percentage migration claim.

| Milestone | Service | Native UI | Persistence | Verification / next gate |
|---|---|---|---|---|
| M0 Baseline | Existing | Existing | Existing | Baseline82 Rust tests pass; snapshot below |
| M1 Jobs / daily UX / converters | Implemented | Converter/modes/drag/logs/save/tab/job UI exercised | Sidebar atomic settings implemented/tested | 105 full tests pass; converter/tooltip render verified; clipboard contents, GUI cancel/overwrite decisions and all minimum/zoom layouts remain unverified |
| M2 Shared ABI / request / signer | Full ABI captured; typed calls and in-process owned-local signing | Bank read/send and Simple/Raw/Trace implemented | Nonsecret per-document session state | Local synthetic flows exercised; remote signer/live networks still unavailable |
| M3 Workspace / compiler / state | Existing compile and switch; expansion planned | Partial | Macros/zoom only | Artifact IDs, watcher lifecycle, atomic schema migration |
| M4 RPC / Live-Local / fork | Local scratch existing | Network selection absent | Planned, secret references only | No Local-to-Live fallback or dev-key live sending |
| M5 Contracts / request workspace | Exact constructor/call encoding, structured receipt/ABI output | Owner-bound Bank documents; combined positional function input; compact request/result sections | Drafts/results/scroll retained in session | Native Bank read/storage and float/redock exercised; raw editable request parity remains unfinished |
| M6 Storage | Raw read existing | Raw slot only | Cache planned | Qualified layout, exact packed/composite decoding |
| M7 Explorer / Trace | Typed pinned historical reads; native callTracer adapter with bounds/provenance; no indexed address history | Explorer plus nonmodal Investigation/Repeater and local contract Trace | Versioned cases/evidence, separate notes/bookmarks | Six production trace transport tests pass; local captured Bank call tree exercised; unsupported provider capability explicit |
| M8 Local tools / snapshots | Basic callbacks | Not exposed | Node-epoch scoped | Read-back mutations and actual local revert |
| M9 Macros | Notes only | Placeholder notes | Existing notes file | Actual typed sequential runner with bounded cancellation |
| M10 Debugger | Real Manager/gateway/engine, typed shared native envelope and operation gate | WorkshopVault preset; independent code/inspector/inspection/session panes and bounded command console | Resource/session scoped; cancelled inspection and nonsecret drafts retained | Shared console float/evaluate/step/redock exercised; current compact debugger comfort qualification recorded separately. Other launch forms/full remote control deferred |
| M11 OZ / dependencies | Auto detection and web operations | Native manager absent | Remapping config existing | Non-mutating preflight and approved installs |
| M12 Qualification / packaging | Planned | Preserve style/zoom | Preferences | No React build dependency only after acceptance |

## 2026-11-13 dock / contract / console checkpoint

Stable document and pane identities now drive real native floating hosts and four dock zones, with menu/drag regrouping, splitters, hide/recover, maximize, geometry checks and nonsecret layout autosave. Contract results use captured full ABI and immutable source context; native signing is in-process and bound to the owned local node. Investigation is nonmodal and shares its controller across hosts. The debugger console uses the existing typed session gate and does not expose an OS shell or remote free-form command endpoint.

The current consolidated suite passed **245 tests**, including trace transport and owner/late-result cases. Actual local Bank read/storage/send/receipt/trace flows and shared-console float/evaluate/step/redock were exercised. Bank normal and actual minimum-size captures at 150% were verified; the final normal150 debugger source/memory/console render was also verified. The last-process OS resize retest was blocked by the desktop tool's window-association boundary; minimum renderer checks are separately recorded. This is not full M0-M12 acceptance or an exhaustive keyboard/cross-window/DPI matrix. See [NATIVE_WORKSPACE.md](NATIVE_WORKSPACE.md) and `.zcode/general-manager/verification-docking-contract-console.md` for controls, commands, evidence and limits.

## Baseline

Exact pre-M0/M1 critical sources: C:/Users/bryan/AppData/Local/Temp/chasm-m0-m1-baseline-japtt2vl. Prior uncommitted migration remains outside this task's changes. Baseline execution artifact: C:/Users/bryan/AppData/Local/Temp/zcode-bash-SgoUvz/sess_cb4b548b-8fd1-43d2-93a9-c38d168e1b76/call_2dd1fdcde680417a91feba53ea05a079-stdout.log (82 pass, 0 fail).

## Protected behavior

Typed constructor and draft identity; keystore file confinement/confirmation and signer provenance; native folder chooser/current-window project switch with owned process cleanup; debugger ownership/cancellation; reactive live zoom. Do not rewrite these under unrelated milestones.

## Verification rules

Use disposable local workspaces/keystores, mock RPC and owned Anvil. No real keys, mainnet broadcasts, automatic installs or commits. Record tests actually executed and prerequisites skipped. Headless/build success does not prove native interaction or visual acceptance. Engine historical support remains capability-bound, including current Cancun restrictions; unavailable functionality must be explicit.

SettingsModal and StructBuilder were not mounted in the React app, so they are not counted as lost active features. Legacy note macros must never be inferred into executable actions.

## 2026-11-13 control / investigation checkpoint

Natural borderless selectable text and centralized Wallet retained. Off-by-default local production API/MCP now shares a semantic facade for representative UI actions, queries and typed analysis, with scoped pairing, request deduplication and local approvals. Native Investigation provides pinned reads, eth_call, captures and offline cases. Final158 tests pass and normal default build succeeds. Reused reviewer: PROCEED for this scoped checkpoint only. Native block0/simulation/evidence persistence and access enable/disable exercised; selected render states reviewed. Full callback/API parity, all-layout/modal qualification, indexed history/trace, arbitrary RPC profiles and complete passive source inspection remain unfinished. See AI_CONTROL.md and .zcode/general-manager/verification-ai-investigation-control.md.
