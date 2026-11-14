# Native control and investigation

Implementation checkpoint: 2026-11-13. This is a staged addition to the native parity roadmap, not complete UI/API parity.

## Local access

Open **Tools > Automation / AI Access**, or the same control on Welcome. Access is off on each process start. Enable it locally, choose client permissions and create a token. The token is shown only in the local panel and cleared when it closes. The server stores a digest, not the bearer value. Treat the displayed token as a credential.

The panel announces a random loopback port. The production API is `http://127.0.0.1:<port>/api/v1`; MCP Streamable HTTP is `http://127.0.0.1:<port>/mcp`. The endpoint is not a browser UI. Configure the MCP client with `Authorization: Bearer <local token>` on every request. This is separate from Slint's optional developer-inspection connector. Default builds do not enable that connector.

- Scopes: application read, semantic UI control, analysis.
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

Remote `workspace.open` is explicitly unavailable until directory identity can be bound safely; use the local native picker. Signing/broadcast, compile/project execution, keystore deletion, chain reset and debugger commands remain local-only. The catalog says why. These are not implicitly authorized by UI-control scope.

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

## Native workspace additions (local-only)

The desktop now has owner-bound contract documents, Simple/Raw JSON/Trace results, Bank-internal Storage, native dock/floating panels and a typed debugger console. These do not imply new production tools. The console and signing remain local-only; `ui.layout` still covers sidebar/zoom, not arbitrary pane placement. Native Trace uses a bounded callTracer request against the captured context, formatted locally with the captured ABI; it is not exposed by the historical analysis reader and is not verified debugger replay. Pending controller confirmations block relevant actions across native hosts. See [NATIVE_WORKSPACE.md](NATIVE_WORKSPACE.md).

## Qualification

The first integrated regression run was 147/150 passing, with three fixture/expectation failures. After fixes, subsequent checkpoints passed 153, 154 and 157 tests. The final post-edit suite passed **158/158**, including real SDK MCP client and HTTP equivalence, loopback authorization, delayed source invalidation before evidence insertion, obsolete native completion rejection, historical RPC/provider validation, case integrity/import and mock-clipboard keyboard/mouse selection. Native block0 read, eth_call simulation, case capture/persistence and access enable/disable were exercised against disposable local fixtures. Native popup visibility, complete zoom/layout coverage and full action parity are not implied by these tests. See the scoped verification record for exact commands and limits.
