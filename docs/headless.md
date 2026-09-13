# Headless REVM protocol

The experimental REVM engine can run without the sevm console or fullscreen interface.
Any frontend that can launch a process and exchange newline-delimited JSON can control the
same live debugger session. This includes editor extensions, browser backends, terminal
interfaces, and clients written in languages other than Python.

The transport is JSON-RPC 2.0 over standard input and standard output. Each input line is
one request and each output line is its response. The server writes protocol data only to
standard output. Process diagnostics go to standard error.

## Run the server

Rust 1.91 or newer is required for the pinned REVM version. This repository ships a
complete request stream that pauses before `SSTORE`, changes its value operand from `1`
to `9`, resumes, and reads the committed storage from the finish event:

```bash
$ uv run sevm-engine < examples/headless-session.jsonl
{"id":1,"jsonrpc":"2.0","result":{"methods":["hello","open","start","transact","wait_event","snapshot","set_breakpoints","set_stack","write_memory","set_gas","set_pc","step","read_storage","write_storage","evaluate","resume","close","shutdown"],"protocol":"sevm-debugger/1","transport":"jsonl-stdio"}}
{"id":2,"jsonrpc":"2.0","result":{"started":true}}
{"id":3,"jsonrpc":"2.0","result":{"snapshot":{"address":"0x1000000000000000000000000000000000000001","calldata":"0x","caller":"0x2000000000000000000000000000000000000002","code_address":"0x1000000000000000000000000000000000000001","depth":0,"frames":[{"address":"0x1000000000000000000000000000000000000001","calldata":"0x","caller":"0x2000000000000000000000000000000000000002","code":"0x600160005500","code_address":"0x1000000000000000000000000000000000000001","depth":0,"gas_limit":79000,"gas_remaining":78994,"is_static":false,"kind":"call","opcode":85,"pc":4,"value":"0x0"}],"gas_limit":79000,"gas_refund":0,"gas_remaining":78994,"gas_used":6,"is_static":false,"memory":"0x","memory_size":0,"mnemonic":"SSTORE","opcode":85,"origin":"0x2000000000000000000000000000000000000002","pc":4,"reason":"breakpoint","stack":["0x0","0x1"],"value":"0x0"},"type":"paused"}}
{"id":4,"jsonrpc":"2.0","result":"0x9"}
{"id":5,"jsonrpc":"2.0","result":null}
{"id":6,"jsonrpc":"2.0","result":{"created_address":null,"gas_used":43106,"output":"0x","storage":[{"address":"0x1000000000000000000000000000000000000001","key":"0x0","value":"0x9"}],"success":true,"type":"finished"}}
{"id":7,"jsonrpc":"2.0","result":null}
```

An installed package exposes the same process as `sevm-engine`. A frontend launches that
command and keeps its standard input and standard output pipes open. A Rust application
can instead depend on `sevm-revm-headless` and call `serve` with any buffered reader and
writer.

Call `hello` first and require `sevm-debugger/1`. A client should stop if it receives a
protocol version it does not support.

## Requests

Every request has a string or numeric `id`, the JSON-RPC version, a method, and an object
of named parameters:

```json
{"jsonrpc":"2.0","id":1,"method":"hello","params":{}}
```

The process owns one session at a time. `start` creates a compatible one-transaction
session. `open` creates a persistent chain and `transact` submits CALL or CREATE
transactions against its committed state. `wait_event` waits for the next pause or
terminal event, and `close` discards the chain. `shutdown` closes it and exits the process
after sending its response.

| Method | Parameters | Result |
|---|---|---|
| `hello` | `{}` | Protocol version, transport, and method names |
| `open` | Initial `accounts` and `breakpoints` | `{"opened":true}` |
| `start` | Session configuration below | `{"started":true}` |
| `transact` | `caller`, optional `to`, `data`, `value`, and `gas_limit` | `{"started":true}` |
| `wait_event` | `{"timeout_ms":5000}` | A paused, finished, or failed event |
| `snapshot` | `{}` | The current paused frame |
| `set_breakpoints` | `{"breakpoints":[{"address":"0x...","pc":4}]}` | Number installed |
| `set_stack` | `{"index":1,"value":"0x9"}` | Written word |
| `write_memory` | `{"offset":64,"data":"0x1234"}` | Bytes written |
| `set_gas` | `{"gas":50000}` | New remaining gas |
| `set_pc` | `{"pc":16}` | New program counter, which must be a `JUMPDEST` |
| `step` | `{"count":1}` | `null`; execution pauses before the next opcode |
| `read_storage` | `{"key":"0x0"}` | Storage word |
| `write_storage` | `{"key":"0x0","value":"0x9"}` | Written word |
| `evaluate` | `{"bytecode":"0x...","keep":false}` | Returned bytes |
| `resume` | `{}` | `null` |
| `close` | `{}` | `null` |
| `shutdown` | `{}` | `null`, followed by process exit |

Omitting `to` from `transact` executes a CREATE transaction and reports the new address as
`created_address` in the finish event. Supplying `to` executes a CALL. Persistent chains
retain account and storage changes between transactions.

`wait_event` is a bounded long poll, not an unsolicited server message. Its default
timeout is 5 seconds. A graphical frontend can use a shorter timeout when it needs to
refresh or cancel its own task promptly.

## Start parameters

`entry` must match one account in `accounts`. The call uses a default funded caller and a
gas limit of `100000` unless those values are supplied. Account balance and storage are
optional:

```json
{
  "entry": "0x1000000000000000000000000000000000000001",
  "caller": "0x2000000000000000000000000000000000000002",
  "gas_limit": 100000,
  "accounts": [
    {
      "address": "0x1000000000000000000000000000000000000001",
      "code": "0x600160005500",
      "balance": "0x0",
      "storage": [{"key": "0x1", "value": "0x2"}]
    }
  ],
  "breakpoints": [
    {"address": "0x1000000000000000000000000000000000000001", "pc": 4}
  ]
}
```

Addresses are 20-byte hex strings. Input EVM words may be decimal or `0x`-prefixed hex.
Output words are hex. Byte strings are hex with an even number of digits.

## Events and errors

A pause event contains a snapshot of the frame that is still executing:

```json
{
  "type": "paused",
  "snapshot": {
    "reason": "breakpoint",
    "address": "0x1000000000000000000000000000000000000001",
    "code_address": "0x1000000000000000000000000000000000000001",
    "caller": "0x2000000000000000000000000000000000000002",
    "origin": "0x2000000000000000000000000000000000000002",
    "value": "0x0",
    "calldata": "0x",
    "is_static": false,
    "depth": 0,
    "pc": 4,
    "opcode": 85,
    "mnemonic": "SSTORE",
    "gas_limit": 79000,
    "gas_remaining": 78994,
    "gas_used": 6,
    "gas_refund": 0,
    "stack": ["0x0", "0x1"],
    "memory_size": 0,
    "memory": "0x",
    "frames": [
      {
        "depth": 0,
        "kind": "call",
        "address": "0x1000000000000000000000000000000000000001",
        "code_address": "0x1000000000000000000000000000000000000001",
        "caller": "0x2000000000000000000000000000000000000002",
        "value": "0x0",
        "calldata": "0x",
        "is_static": false,
        "pc": 4,
        "opcode": 85,
        "gas_limit": 79000,
        "gas_remaining": 78994,
        "code": "0x600160005500"
      }
    ]
  }
}
```

`reason` is `breakpoint`, `step`, or `out_of_gas`. A finish event contains `success`,
`gas_used`, `output`, and the changed storage slots. The `frames` array runs from the
outermost frame to the paused frame and retains each caller's last program counter. A
failed event contains a `message`.

Errors use the JSON-RPC error shape. The server stays alive after parse, request, method,
parameter, engine, and session-state errors:

| Code | Meaning |
|---:|---|
| `-32700` | Invalid JSON |
| `-32600` | Invalid JSON-RPC request |
| `-32601` | Unknown method |
| `-32602` | Invalid parameters |
| `-32000` | REVM engine error or timeout |
| `-32001` | No active session, or a session is already active |

This transport is additive. The Python bridge still links directly to the same Rust core,
and the existing Py-EVM debugger remains the default application engine while the REVM
migration is developed and checked for parity.
