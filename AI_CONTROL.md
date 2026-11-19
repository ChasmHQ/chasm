# Native control and investigation

Last updated: 2026-11-19. Query `app.catalog` for the exact command list and schemas of the build you run.

## Local access

Open **Tools > Automation / AI Access**, or the same control on Welcome. Access is off on each process start. Enable it locally, choose a client tier and create a token. The token is shown only in the local panel and cleared when it closes. The server stores a digest, not the bearer value. Treat the displayed token as a credential.

The panel announces a random loopback port. The production API is `http://127.0.0.1:<port>/api/v1`; MCP Streamable HTTP is `http://127.0.0.1:<port>/mcp`. Configure the MCP client with `Authorization: Bearer <local token>` on every request.

### Tiers

| Capability | Limited | Partial | Full |
|---|---|---|---|
| Read state, artifacts, jobs, view queries (`contracts.view`, `contracts.deploy_form` (the constructor form of an undeployed tab), `macros.state`, `debugger.state`) | Yes | Yes | Yes |
| Analysis (`/analyze`: pinned chain reads, eth_call, cases) | Yes | Yes | Yes |
| UI control: navigate, select, edit drafts, attach, simulate, trace, storage read, macro edit/validate, panels | No | Yes | Yes |
| Chain and build: deploy, send, compile, dev signer, snapshot, revert, mine, warp, macro run | No | One local approval per action | Yes |
| Debugger: launch, start, step, evaluate, console, cancel; record or navigate a step trace; save/open captures | No | No | Yes |
| Change step-debugger presentation (`debugger.trace.view`) | No | Yes | Yes |
| `ui.screenshot` (PNG of the main window) | No | No | Yes |
| Remove a deployment record | No | Local approval | Local approval |
| Open another project by remote path (`workspace.open`) | No | No | No |

Never available at any tier, because no command exists for them:
- reading or entering private keys, mnemonics or keystore passwords; keystore unlock, create, export or delete (use `wallet.dev_account` for the public Anvil test accounts);
- pairing, tiers, revocation and approval decisions;
- signing against any node other than the workspace-owned local Anvil.

A screenshot is refused while the AI access panel, a pairing token or an approval is on screen. Full clients get 600 requests per minute, other tiers 120.

Commands that act through the native callbacks return `{"status":"completed","effect":"native"}` once the callback ran. Chain work is asynchronous: read `app.state` (`busy` flags) or the matching view query until it settles.

### Local testing

Debug builds accept `--control-token-file <path>`: Chasm enables access at startup, pairs one Full client and writes the endpoint and token (one per line) to that file, then deletes it on exit. Release builds do not have the flag.

- Disable revokes all tokens and cancels queued approvals. Individual clients can be revoked.
- Strict exact Host, no browser Origin, no wildcard CORS, 128 KiB request limit, bounded responses, four in-flight requests, per-client request rate limit, request/body timeouts.
- The host survives workspace switches; workspace Anvil/debugger services do not.
- No remote pairing, enable, approval, clipboard read, password entry, private-key export, arbitrary widget setter, shell command or filesystem endpoint exists.

## HTTP v1

Every route requires the bearer token. Errors are `{ "error": { "code": "...", "message": "..." } }`.

| Method and path | Input | Result |
|---|---|---|
| `POST /query` | `{"query":"app.state"}` | Safe state/context |
| `POST /query` | `{"query":"app.catalog"}` | Available and unavailable semantic actions with schemas/reasons |
| `POST /query` | `{"query":"workspace.artifacts"}` | Artifact/function metadata, scoped IDs |
| `POST /query` | `{"query":"jobs.list"}` | Names/status/namespace-qualified IDs/transaction hashes, not raw job text |
| `POST /command` | Command envelope below | Completed, failed, or pending local approval |
| `GET /requests/<requestId>` | None | This client's previous command result |
| `GET /schema` | None | Versioned query, command and analysis schemas |
| `POST /analyze` | Typed analysis request | Pinned chain data or offline case/history results |

Example UI command (replace context with the exact returned `app.state.context`):

```json
{
  "requestId": "fixture-navigation-1",
  "context": {"workspaceId":"workspace-0000000000000001","networkEpoch":1,"viewRevision":0},
  "command": {"actionId":"ui.navigate","params":{"activity":"explorer"}}
}
```

Implemented semantic commands: navigation, document select/close, artifact/function/deployment selection, converter edits, address/constructor/value drafts and layout. Deployment-record removal requires local approval and does not delete the on-chain contract. IDs are workspace-scoped semantic identities, not row indexes. Legacy duplicate artifact names are rejected, not guessed.

Use the local folder picker to switch projects; the API does not accept `workspace.open` paths. For supported deploy/send operations, Chasm signs locally against its owned node. Clients never receive the key or signed payload. Build, chain and debugger commands have the tier requirements listed above; UI-control permission alone does not authorize them.

Command request IDs are unique per client across HTTP and MCP. Same ID plus same payload returns the stored result; a changed payload conflicts. Terminal partial failures are retained too. At capacity, revoke/re-pair or disable access; accepted requests are not silently evicted and rerun. After an ambiguous timeout, check `/requests/<id>` or resend the identical request, never create a new ID blindly.

Approval is bound to the exact client, command, workspace/network/view context and expiry (120 seconds). It is consumed once. Changing context, revocation or expiry prevents execution. The decision is available only in the native confirmation, not as a remote action.

## MCP

The official Rust SDK (`rmcp` 3.9.0) implements the transport and protocol. Tools use the same facade and schemas as HTTP:

- `chasm_query`
- `chasm_command`
- `chasm_request_status`
- `chasm_analyze`

No production prompts or resources are advertised yet. No claim of complete remote control of every legacy callback is made. Query `app.catalog` before assuming an action exists.

## Historical analysis

Example account read:

```json
{
  "op":"chain",
  "context":{"workspaceId":"workspace-0000000000000001","networkEpoch":1,"viewRevision":0},
  "request":{"op":"account","address":"0x0000000000000000000000000000000000000001","block":"0x10"}
}
```

The node is selected locally by the workspace. Clients cannot supply arbitrary endpoints. Read requests cover block, transaction+receipt, account balance/nonce/code, storage, `eth_call` simulation and a bounded page of up to 20 blocks. Moving `latest` is resolved before state reads; number/hash provenance is retained and checked after account/storage/simulation. No historical error falls back to latest.

Transaction absence, pending (null block number), incomplete mined receipt and provider errors are distinct. Full transaction and receipt objects retain calldata and logs. Provider error text is sanitized rather than echoed. Simulation is bounded read-only `eth_call`, not broadcast or an exact replay of the original transaction. Historical data availability depends on the selected provider's archive support. Tracing and indexed address history are not implemented by this reader.

Analysis history is bounded to 50 successful captures and 16 MiB in this process. This is not a complete chain index. Case evidence is separately persisted, versioned and digest-checked; notes/bookmarks are separate from immutable evidence. Imported content is inert: no network, compiler, worker or macro is started. The digest checks bundle integrity, not whether a provider is truthful. Source data, contract names and notes are untrusted content, not instructions.

Case file I/O is confined to the app-controlled repository, uses bounded reads and atomic writes, and refuses links/reparse points during checks. This is not a defense against a hostile local process racing filesystem path replacement; it does not replace the protected-handle keystore deletion implementation. Secret screening is a best-effort additional guard, not a promise that arbitrary free text cannot contain a secret. Never paste credentials into notes/imports or capture raw RPC credential URLs.

## Native workspace commands

Commands added for the native app (all in `app.catalog`):

| Area | Commands and queries |
|---|---|
| Deploy in place | `contracts.deploy.open`, `contracts.constructor.edit`, `contracts.deploy`; `contracts.deploy_form` reads the unbound tab's constructor form |
| Templates | `contracts.template {kind: erc20 \| erc721 \| ownable, name}` |
| Step debugger | `debugger.trace {hash?}`, `debugger.trace.step`, `.seek`, `.breakpoint`, `.console` (`locals`, `gas`, `args`, `writes`, …), `.view {tab: 0-5}` (5 = Gas), `.open_saved`; `debugger.state.trace` has `variables` (locals first) and `gas` |
| Offline captures | `debugger.capture.save`, `debugger.capture.open {name}`; `debugger.state.captures` |
| Panes | `ui.panel {pane, action: dock \| float \| hide \| maximize \| reset \| preset, zone?}`; trace panes are `trace_ops`, `trace_state`, `trace_console`; presets `contracts`, `debugging`, `investigation`, `tracing`; `app.state.layout.panes` |
| History, timeline, cheats | `history.list`, `history.action`, `chain.timeline.revert`, `chain.cheat` |
| Compiler, libraries, fork | `toolchain.state` (incl. `network.forkChain`, `publicRpcs`), `toolchain.action {open, lock_solc, install, use, remove, network}` |
| Palette, notices | `ui.palette`, `ui.notice.action` |

Forked transactions replay on a separate Anvil node with the real chain id; nothing is signed there. Pending controller confirmations block relevant actions across native hosts. See [CLAUDE.md](CLAUDE.md) for the code layout.

## Qualification

Run `cargo test` for authorization, request handling and HTTP/MCP regression tests. The [live test runner](tests/live/README.md) exercises the control API against a debug build in a temporary workspace. Release builds require local pairing; they do not accept the runner's `--control-token-file` flag.
