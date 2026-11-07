# Chasm MCP server for AI clients

`chasm mcp` runs the REVM debugger as a [Model Context Protocol](https://modelcontextprotocol.io)
server over stdio. Its tools return compact JSON for clients that inspect and step
through transactions.

[back to README](../README.md)

## Why the MCP surface is shaped differently

| A human... | An AI client... | So the tools... |
|---|---|---|
| scrolls the memory pane | gets one fixed response | every windowed read takes `offset`/`words` and returns `{items, total, truncated}`; narrow the window for more |
| watches panes change live | only sees tool results | `chasm_diff_since_last_stop` reports stack pushes/pops, changed memory ranges and gas after each step |
| knows where they are | must be told, every time | every navigation tool returns the same stop report (pc, opcode, gas, sp, location, stack top, stack delta) |
| reads syntax highlighting | pays per token | output is plain JSON: full-width hex words, decimal counts, no markup |
| restarts on a typo | cannot recover silently | failures return `{"error": "..."}` with the fix in the message |

## Running it

```bash
chasm mcp
uv run chasm mcp
chasm mcp --timeout 300
```

Any MCP client that speaks stdio can connect. Generic configuration:

```json
{
  "mcpServers": {
    "chasm": {
      "command": "chasm",
      "args": ["mcp"]
    }
  }
}
```

One debug session is active at a time. Starting a new target replaces it.

## Tool reference

### Lifecycle

| Tool | Meaning |
|---|---|
| `chasm_start_session` | compile and start a web3.py driver script or a `.t.sol` Foundry test. Extra `args` go to scripts, and `match`/`match_contract` select tests. Returns the first stop report |
| `chasm_stop_session` | dispose the session (chain state is discarded) |
| `chasm_restart_session` | re-run from scratch: fresh chain, breakpoints kept, optional new script args |

### Navigation

| Tool | Meaning |
|---|---|
| `chasm_continue` | run to the next breakpoint/watchpoint/error, or program end |
| `chasm_step_opcodes` | `count` opcodes (≤1000) |
| `chasm_step_lines` / `chasm_next_lines` | Solidity lines, into / over internal calls |
| `chasm_finish_frame` | run to the end of the current frame |
| `chasm_run_to` | a pc, or a location like `Bank.sol:46` / `*0x108` |
| `chasm_get_status` | the stop report alone |
| `chasm_diff_since_last_stop` | what the last navigation changed |

### Inspection (windowed, truncation-flagged)

| Tool | Meaning |
|---|---|
| `chasm_read_memory` | 32-byte words with region names. `beyond: true` marks unallocated reads as zero |
| `chasm_read_stack` | operand stack, index 0 = top |
| `chasm_read_storage` | decoded layout by default (names, types, packed slots), or raw slots |
| `chasm_read_calldata` | windowed. The first window includes selector and signature |
| `chasm_get_backtrace` | Solidity and EVM frames interleaved |
| `chasm_get_locals` / `chasm_get_arguments` | decoded locals / ABI-decoded call arguments |
| `chasm_disassemble` | rows `{pc, text, jumpdest, line}` around a pc |
| `chasm_read_source` | numbered source window, current line flagged |
| `chasm_get_gas_profile` | spend by opcode and by source line |
| `chasm_get_logs` | events so far, names decoded |
| `chasm_list_contracts` / `chasm_list_functions` | the compiled surface |

### Search, provenance, and bytecode parity

| Tool | Meaning |
|---|---|
| `chasm_find_bytes` | hex pattern in `code` (gadget hunting: pc, instruction alignment, nearest preceding JUMPDEST), `memory`, or `calldata` |
| `chasm_set_provenance` / `chasm_why_stack(index)` | record opcode steps within a frame and trace stack values to constants, calldata, memory, storage, and frame entry |
| `chasm_export_trace` | the recording as anvil/geth structLog JSON (to a file, or a windowed inline slice) |
| `chasm_check_parity` | compare a compiled runtime against deployed bytecode. `chasm_start_session(reference_runtime_hex=...)` stops on divergence |

**Provenance semantics**: REVM pauses at each opcode while recording is enabled.
The slice follows recorded values and positions within a frame. It skips the
transition into a nested call because the two frames have different stacks.
Hand mutations (`set $stack[i]`) are not recorded, so ask `why` before mutating.

### Breakpoints

| Tool | Meaning |
|---|---|
| `chasm_set_breakpoint` | `File.sol:LINE`, `*0xPC`, or a function name, with an optional Solidity `condition` |
| `chasm_set_opcode_breakpoint` | every occurrence of an opcode (`SSTORE`, `DELEGATECALL`, ...) |
| `chasm_set_watchpoint` | storage value written/read/accessed, or `*0x40` memory |
| `chasm_list_breakpoints` / `chasm_delete_breakpoint` | management |

### Mutation and evaluation

| Tool | Meaning |
|---|---|
| `chasm_evaluate` | real Solidity against the paused frame (state, mappings, locals, `keccak256`, `abi.encode`, units). `keep: true` keeps side effects like gdb's `call` |
| `chasm_set_gas` | overwrite the gas meter. At an out-of-gas stop, continuing retries the failed instruction |
| `chasm_set_stack_slot` | rewrite an operand before its opcode consumes it |
| `chasm_write_memory` / `chasm_write_storage` | raw writes |
| `chasm_set_pc` | move the pc (JUMPDESTs only) |
| `chasm_command` | raw gdb-verb passthrough for everything else: `vm.*` cheatcodes, Yul, `jump`, `reseat`, `x/32xb 0x40` |

## A first session

```
chasm_start_session(target="exploit.py", contracts="contracts/", optimize=true)
chasm_find_bytes(pattern="5b50505050", scope="code")      # gadget hunt
chasm_set_breakpoint(location="*0x1a2")                   # or "deposit", "SSTORE"
chasm_continue()
chasm_read_memory(offset=0x40, words=4)
chasm_evaluate(expression="balances[msg.sender]")
chasm_diff_since_last_stop()
chasm_restart_session(args=["0x<new payload>"])
```
