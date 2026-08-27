# sevm

A fullscreen, gdb-compatible interactive debugger for Solidity, running on Py-EVM.

If you know gdb, you know sevm. `b`, `n`, `s`, `si`, `finish`, `bt`, `p`, `x/32xb`,
`info registers`, `set var` all mean what you expect. The differences: expressions are
Solidity and the machine underneath is the EVM.

sevm stops *inside* a running transaction with the frame still alive. You can read
uncommitted state, call view functions, rewrite a stack operand before the opcode consumes
it, and force an out-of-gas at an exact instruction.

```bash
sevm run --contracts contracts examples/debug_bank.py
```

## Install and run

sevm is packaged with [uv](https://docs.astral.sh/uv/). Install it as a standalone tool:

```bash
uv tool install .        # from a checkout of this directory
# or, once published:    uv tool install sevm
```

Options go **before** the script; everything after it is forwarded to the script:

```bash
sevm run --contracts contracts examples/debug_bank.py            # fullscreen TUI
sevm run --console --contracts contracts examples/debug_bank.py  # plain text
sevm compile contracts                                           # what sevm sees
```

To hack on sevm itself, use an editable dev environment:

```bash
uv sync                  # .venv with sevm installed editable + dev tools
uv run sevm --help
uv run pytest
```

Your script needs no changes. It drives web3.py against an in-process Py-EVM chain exactly
as it already does; sevm compiles the contracts, patches Py-EVM, runs the script on a
worker thread, and stops the first time execution enters code it recognises (matched by
bytecode). The first stop is usually the constructor; use `-x c` to skip past it, or set a
breakpoint first with `-x 'b Bank.sol:46' -x c`.

## Foundry tests and cheatcodes

`sevm run` also debugs a Foundry test, dispatched by extension: a `.py` argument is the
web3 driver above; a `.sol` argument is a Foundry test.

```bash
sevm run path/to/Counter.t.sol                 # fullscreen TUI, first test function
sevm run --console -m testDeposit MyTest.t.sol # plain text, pick a test by name
```

For each test function sevm does a fresh deploy + `setUp()` + the test call (forge's
per-test isolation) and opens at the first line of the first test. `continue` runs to the
next test body. `-m/--match` narrows to functions matching a substring;
`--match-contract` narrows the contract.

A lone `Test.sol` works with no `forge install`: sevm ships a minimal `forge-std` (`Vm`,
`Test`, `console`). Point it inside a real Foundry project and it uses that project's
layout, `solc` version, and remappings (`foundry.toml` + `remappings.txt` + `lib/`);
`-y` accepts a default `foundry.toml` without prompting.

Cheatcodes are interpreted against live Py-EVM state and `console.log` prints as you step.
Supported: `warp roll fee chainId coinbase deal etch store load prank startPrank
stopPrank addr sign assume label`. You can also fire one at the prompt against the current
frame:

```
(sevm) vm.warp(12345)
(sevm) p block.timestamp
$1 = 12345  (uint256)
```

Interactive arguments are simple literals, not full Solidity expressions. `help
cheatcodes` lists the implemented set with a line of help each, generated from the
registry so it cannot drift from what is actually implemented.

**Not yet supported:** `expectRevert`/`expectEmit`/`expectCall`/`mockCall`, `ffi`,
forking, and fuzz/invariant argument generation. In the bundled `forge-std`, assertions
**revert** on failure; point sevm at a real forge-std for non-reverting semantics.

## The screen

```
  Bank._credit(address, uint256)  Bank.sol:46   gas 276,050/278,936   depth 0   pc 0x9c1   DUP1
+- SOURCE -------------------------------------------+- CALL STACK -----------------------+
|   44     1     function _credit(address who, ...   | -> #0 Bank._credit(...) Bank.sol:46 |
|   45    25         uint256 fee = _fee(amount);     |    #1 Bank.deposit()    Bank.sol:52 |
| *>46               balances[who] += amount - fee;  |    #2 Bank  pc 0x380  [tx depth=0]  |
|   47               totalDeposits += amount - fee;  +- VARIABLES ------------------------+
|   48               history.push(amount);           | args  who           0xdb7f98f5...   |
|   49           }                                   |       amount        2000000000...   |
|                                                    | local fee           2000000000...   |
+-----------------------------------------------------+ state owner         0x7e5f4552...  |
+- DISASSEMBLY ------+- STACK ---------+- MEMORY -----+- STORAGE --------------------------+
|    09bf SWAP1      | 0 0x11c37937..  | 0000 00 00.. |   0+0   owner        0x7e5f4552... |
| => 09c1 DUP1     3 | 1 0x1bc16d67..  | 0040 00 00.. |   0+20  feeBps       25            |
+--------------------+-----------------+--------------+   1+0   totalDeposits 1000000000.. |
```

The gutter is gdb's: `=>` current line, `*` breakpoint, `*>` both; the number beside it is
gas spent on that line so far. The current opcode's operands light up in every pane at
once. Colours are ANSI, so the debugger wears your terminal's own palette.

Function keys: **F5** continue, **F7** step in, **F8** next, **F6** finish, **F10** stepi,
**F11** nexti, **F9** toggle breakpoint, **F2** low-level panes, **F4** back to current
state, **F1** help. On macOS **Cmd+C/P/L/Q** work too; Ctrl does the same everywhere.

STACK labels the slots that hold the current frame's locals. Parameters are bold; a
multi-word local labels both words `name.ptr`/`name.len`. When the current opcode is about
to consume a labelled slot, the row turns red.

## Commands

Every gdb verb and abbreviation below behaves as it does in gdb.

### Execution

| Command | Meaning here |
|---|---|
| `c` / `continue` | run until a breakpoint |
| `n` / `next [N]` | next Solidity line, stepping over calls |
| `s` / `step [N]` | next Solidity line, stepping into calls |
| `si` / `ni [N]` | one opcode, into / over `CALL` |
| `finish` | run to the end of the current frame |
| `u` / `until LOC` | run to a line or `*PC` |

`step`/`next` understand *internal* Solidity calls (compiled to `JUMP`, so EVM depth never
changes) by tracking the source map's `i`/`o` jump markers.

### Breakpoints

| Command | Meaning |
|---|---|
| `b Bank.sol:46` | a source line, snapped forward to the next line with code |
| `b deposit` | a function, by name or `Contract.name` |
| `b SSTORE` | every occurrence of an opcode, in any contract |
| `b *0x108` | a raw program counter |
| `b LOC if EXPR` | conditional; `EXPR` is real Solidity |
| `tbreak` | fires once, then deletes itself |
| `delete N` / `disable` / `enable` / `info breakpoints` | management |
| `watch EXPR` | break when a storage value changes, reporting old to new |
| `rwatch EXPR` / `awatch EXPR` | break on read / either |

Watchpoints work on state variables, mapping elements (`watch balances[msg.sender]`), and
memory (`watch *0x80`). sevm also stops on reverts by default, at the failing instruction,
and decodes the reason (`reverted: "..."`, `panic 0x11`, or a custom error with args).

### Inspection

| Command | Meaning |
|---|---|
| `p EXPR` | evaluate a Solidity expression |
| `call EXPR` | evaluate and **keep** the side effects |
| `ptype EXPR` | report the Solidity type |
| `display EXPR` | re-evaluate at every stop |
| `x/NFU ADDR` | examine memory, gdb syntax |
| `bt` / `f N` / `up` / `down` | call stack and frame selection |
| `l` / `list` / `disas` | source listing / disassembly |
| `info registers` | pc, gas, depth, stack height, `msg.*`, `tx.origin`, static flag |
| `info args` / `info locals` / `info storage` | frame args / locals / state vars, decoded |
| `info gas` | limit, used, refund, and a profile by source line and by opcode |
| `info frame` / `info logs` / `info sources` / `info functions` | the rest |

### Mutation

| Command | Meaning |
|---|---|
| `set var owner = msg.sender` | write storage through Solidity |
| `set var balances[alice] = 5 ether` | mappings, packed slots and structs encode correctly |
| `call deposit()` | run a function and keep the effects |
| `set $stack[0] = 0xc0ffee` | rewrite an operand before the opcode consumes it |
| `set $gas = 100` | force an out-of-gas at an exact instruction |
| `set var fee = 1 ether` | write a local's stack slot |
| `set $mem[0x80] = 1` / `set $storage[0] = 0xdead` | raw writes |
| `jump 0x108` | move the program counter (JUMPDESTs only) |
| `mstore(0x80, 1)` / `asm YUL` | run inline assembly; see below |
| `vm.deal(alice, 10 ether)` | fire a Foundry cheatcode at the current frame |

Every one of these re-reads the machine as soon as it lands, so the STACK, MEMORY,
STORAGE and VARIABLES panes show the change without stepping first.

### Convenience variables

`$pc` `$gas` `$gasused` `$depth` `$sp` `$step` `$stack[N]` `$mem[0x40]` `$storage[1]`, and
the value history `$1` `$2` ... They bypass solc, so they work on contracts with no source
and mix into a Solidity expression: `p $storage[1] + 1 ether`.

## Inline assembly (Yul)

Type a Yul builtin at the prompt and it runs on the frame you are stopped in, using
Py-EVM's real opcode implementations. This is the low-level twin of `set var`: `p` answers
a question on a state snapshot that is then thrown away, assembly writes to the machine
that is actually running.

```
(sevm) mload(0x40)
mload(0x40) -> $1 = 0x80 (128)  (gas 3)
(sevm) mstore(0x80, 0xdeadbeef)
mstore(0x80, 0xdeadbeef) -> ok  (gas 9)
(sevm) sstore(3, add(sload(3), 1))
sstore(3, add(sload(3), 1)) -> ok  (gas 203)
(sevm) asm mstore(0x80, 1); mstore8(0xa0, 0x61); mload(0x80)
```

Calls nest exactly as in `assembly { }`. Reads print their value and enter the value
history as `$N`. `asm` (also `assembly`, `yul`) is the explicit form and takes several
`;`-separated statements; a bare `mstore(...)` line is recognised on its own.

Arguments are decimal or hex literals, `1 ether`, `true`/`false`, a 32-byte string literal
(`"hi"`, right-padded as Yul pads it), a nested call, or any convenience variable:
`mstore(0x80, $storage[1])`, `sstore(0, $stack[0])`.

Two things are deliberately not faithful to a real execution:

* **Gas is metered, reported, and then handed back.** Poking at the machine must not be
  able to turn a transaction that succeeds into one that runs out of gas.
* **Memory expansion an op causes is kept**, because the op really did write there.

Refused, with the reason: `jump` `jumpi` `pc` `push*` `dup*` `swap*` (Yul excludes these
itself, since they are how the compiler implements control flow) and `stop` `return`
`revert` `invalid` `selfdestruct` (they would end the frame under you; `finish` runs it to
its end instead). Everything else in the EVM Yul dialect is available, including
`keccak256`, `mcopy`, `tload`/`tstore`, the `log*` family and the call opcodes.

`help assembly` lists every builtin with its arguments.

`mstore(...)` at the prompt is assembly, but `p mstore(...)` is still Solidity, so a
contract with a function of the same name stays reachable.

## Evaluating Solidity

`p` compiles the expression as real Solidity against the paused contract, runs it on a
state snapshot, and throws the snapshot away, so it cannot disturb the run.

```
(sevm) p balances[msg.sender] + 100 ether
$1 = 101000000000000000000 (101 ether)  (uint256)
(sevm) p accounts[owner].nickname
$2 = "hodler"  (string memory)
(sevm) p keccak256(abi.encode(owner, num))
$3 = 0xe49663c38505702a80c082069aa4ea858bb87c2b324bb676c028d21aa819624e  (bytes32)
```

Because it *is* Solidity, operator precedence, checked arithmetic, `ether`/`gwei`/`days`
units, casts, `keccak256`, `abi.encode`, `type(uint256).max`, struct/mapping access, and
calls to `internal`/`private` functions all work. `msg.sender`/`msg.value` report the
paused frame's values. Results are cached per expression, so a `display` costs one compile.

## Local variables

`info locals` names, types and decodes the locals of the frame you are stopped in, and `p`
takes expressions over them:

```
(sevm) info locals
  who            address            = 0x93291240...a6 (param)
  amount         uint256            = 2000000000000000000 (2 ether) (param)
  fee            uint256            = 2000000000000000 (0.002000 ether)
(sevm) p amount - fee
$1 = 1998000000000000000 (1.998000 ether)  (uint256)
(sevm) set var fee = 1 ether
```

solc emits no location for locals, so sevm reconstructs it from the AST and the run's stack
heights (the way Truffle and Remix do). A value shows only when the frame was observed from
entry, the slot is still below the top of the stack, and the current instruction is inside
the declaration's scope; otherwise you get `<unavailable>` with the reason. `set var`
writes the stack slot directly and refuses memory/calldata locals rather than corrupting a
pointer.

Still not readable: `assembly` block variables (`p $stack[N]`), storage-pointer locals
(index through them instead), calldata references (`info args`), a local read on its own
declaration line (`n`, then read it). Optimized builds are unreliable, so debug builds keep
the optimizer off.

## Requirements

Debug builds compile with the optimizer **off** and via-IR **off** (sevm warns if you
override that); optimized codegen degrades the source map and makes stepping unreliable.

Transactions do **not** need an explicit `gas=`. `eth_estimateGas` binary-searches the
limit by running the transaction repeatedly, so its early probes fail out-of-gas by design;
sevm runs those with the hook suspended and reports how many it skipped in `info frame`.

Tested against web3 7.16, py-evm 0.12.1b1, eth-tester 0.13.0b1, solc 0.8.28, Textual 8.2.8,
Python 3.12.

## How it works

`session.py` monkeypatches `BaseComputation.apply_computation`, reimplementing Py-EVM's
opcode loop with a blocking hook. The debugged program runs on a worker thread; the
controller drives it over two queues and the threads strictly alternate, so exactly one is
ever runnable. Only the VM thread touches Py-EVM objects: the controller gets immutable
`FrameSnapshot`s and asks for anything else via an inspect command the VM thread services
while parked in the hook. Nested calls need no special handling (`CALL` re-enters
`apply_computation`), and speculative execution is safe because `state.snapshot()`/`revert()`
are real journal checkpoints.

| Module | Responsibility |
|---|---|
| `compile.py` | solc standard-JSON, the `Artifact` model, bytecode identity |
| `srcmap.py` | source map parsing, pc to line, internal jump markers |
| `disasm.py` | PUSH-aware disassembly, opcode table derived from Py-EVM |
| `frames.py` | EVM and internal frame model, AST function index, backtrace rows |
| `locals.py` | local-variable declarations, scope rules, stack-slot decoding |
| `breakpoints.py` | breakpoints and watchpoints, thread-safe |
| `session.py` | the patch, the stop policy, inspect and mutation operations |
| `evaluate.py` | Solidity expression evaluation by source injection |
| `decode.py` | storage layout decoding, calldata and revert decoding |
| `foundry.py` / `cheatcodes.py` | Foundry test driver / cheatcode interpreter |
| `commands.py` | the gdb command surface, shared by both frontends |
| `console.py` / `tui/` | plain-text and Textual frontends |
| `cli.py` | `sevm run` and `sevm compile` |

## Tests

```bash
python3 -m pytest tests/test_sevm.py -q
```

Covers artifacts and source maps, the stop policy for every step mode, breakpoints and
watchpoints, cross-contract frames, reverts and panics, expression evaluation, storage
decoding, local variables across scoping, the whole command surface, mutation, and a
headless render of the TUI.
