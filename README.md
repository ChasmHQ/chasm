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

Contracts here get the same treatment as a Foundry test: an import of
`forge-std/console.sol` or `@openzeppelin/contracts/...` is installed and remapped, a
`foundry.toml` above them is honoured, and `vm.*` cheatcodes work at the prompt.

## Foundry tests and cheatcodes

`sevm run` dispatches on the extension: a `.py` argument is the web3 driver above, a `.sol`
argument is a Foundry test.

```bash
sevm run test/Counter.t.sol                     # fullscreen TUI, opens in the first test
sevm run --console -m testDeposit Vault.t.sol   # plain text, one test by name
```

Each test gets a fresh deploy + `setUp()` + the test call, as forge isolates them, and the
debugger opens on the first line of the first test. `continue` runs to the next test body.
`-m/--match` narrows to test functions matching a substring, `--match-contract` to a
contract.

### A lone .t.sol with nothing installed

```solidity
// /tmp/scratch/Vault.t.sol
import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract VaultTest is Test {
    address alice = address(0xA11CE);

    function testDeal() public {
        vm.deal(alice, 3 ether);
        assertEq(alice.balance, 3 ether);
    }
}
```

```console
$ sevm run --console -y /tmp/scratch/Vault.t.sol
standalone test at /tmp/scratch; compiling ...
installing forge-std from https://github.com/foundry-rs/forge-std @ v1.16.2
installing @openzeppelin/contracts from https://github.com/OpenZeppelin/openzeppelin-contracts @ v5.7.0
wrote 2 remapping(s) to remappings.txt
debugging 1 test(s): VaultTest.testDeal
Breakpoint 1, VaultTest.testDeal() at Vault.t.sol:11
  11          vm.deal(alice, 3 ether);
(sevm)
```

That run leaves a directory `forge` also understands:

```
/tmp/scratch
├── Vault.t.sol
├── foundry.toml        [profile.default] with libs = ["lib"], no src/test
├── remappings.txt      forge-std/=lib/forge-std/src/
│                       @openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/
└── lib
    ├── forge-std                 v1.16.2
    └── openzeppelin-contracts    v5.7.0
```

Drop `-y` and sevm asks once before writing anything:

```console
$ sevm run --console /tmp/scratch/Vault.t.sol
sevm will:
  - create /tmp/scratch/foundry.toml
  - install forge-std, @openzeppelin/contracts into /tmp/scratch/lib
[y/N]
```

Answer `n` and sevm writes nothing, compiling against what is already there. `--no-install`
skips the prompt and refuses the same way:

```console
$ sevm run --console --no-install /tmp/scratch/Vault.t.sol
standalone test at /tmp/scratch; compiling ...
compile failed: unresolved import 'forge-std/Test.sol' in Vault.t.sol, and sevm was told
not to install it. Run again with -y to let it, or install it yourself:
  forge install <org>/<repo>
  echo 'forge-std/=lib/<repo>/src/' >> remappings.txt
```

### Inside a Foundry project

```console
$ sevm run --console test/Token.t.sol
foundry project at /work/mytoken; compiling ...
debugging 2 test(s): TokenTest.testMintAsOwner, TokenTest.testMintPrankRevertsForNonOwner
Breakpoint 1, TokenTest.testMintAsOwner() at test/Token.t.sol:18
  18          token.mint(bob, 100);
```

Nothing is fetched: the project's `foundry.toml`, `remappings.txt` and `lib/` are used as
they are, including its `solc` pin and `evm_version`. forge-std is cloned only when `lib/`
has none.

### How an import is resolved

sevm looks up the prefix in its own table (`forge-std`, `ds-test`, `solmate`, `solady`,
openzeppelin), then in npm's registry metadata for anything else, and clones the repository
it finds at the newest release tag. Prereleases are skipped. The remapping comes from where
the imported file actually landed in the clone, so `src/`, `contracts/` and flat layouts
all work.

A clone is a pin. sevm never updates it; to move a version, delete `lib/<name>` or run
`forge install <org>/<repo>@<tag>` yourself. git is required, the `forge` binary is not,
and only the first run for a given library needs the network.

### Build cache

The first run compiles. The next one does not.

```console
$ time sevm compile .
wrote 48 artifact(s) to out/sevm
solc 0.8.36, optimizer off
real  1.88
$ time sevm compile .
cache hit (40 sources)
real  0.61
$ $EDITOR src/Setup.sol
$ time sevm compile .
recompiled 3 of 40 sources
wrote 48 artifact(s) to out/sevm
real  1.04
```

A build leaves the same two directories forge does:

```
cache/sevm/     the compilation unit, keyed by solc version, settings and source content
out/sevm/       one JSON per contract, in forge's layout: out/sevm/Token.sol/Token.json
```

An edit invalidates the file you changed and everything that imports it; the rest is
reused, so solc only has to emit the parts that moved. `forge clean` clears both along with
forge's own.

Artifacts are nested under `out/sevm/` rather than written straight into `out/`, because
sevm compiles with the optimizer off: overwriting `out/Token.sol/Token.json` would leave
forge's cache calling it fresh and the next `forge test` running sevm's build. They carry
`abi`, `bytecode`, `deployedBytecode`, `methodIdentifiers`, `storageLayout` and the source
id, with forge's field names; `metadata` is the one thing missing, as sevm never asks solc
for it.

A directory with no `foundry.toml` gets nothing written into it: its cache lives under
`~/.cache/sevm/` and no artifacts are written at all. `--force` recompiles and rewrites the
entry, `--no-cache` (or `SEVM_NO_CACHE=1`) writes nothing anywhere.

### Cheatcodes

Cheatcodes run against live Py-EVM state and `console.log` prints as you step. Implemented:
`warp roll fee chainId coinbase deal etch store load prank startPrank stopPrank addr sign
assume label`, plus the full `vm.assert*` family (`assertEq`, `assertGt`,
`assertApproxEqRel`, the `*Decimal` forms, 116 overloads) that forge-std's own `assertEq`
calls into. A failed assertion stops the debugger where it broke, with the comparison:

```console
(sevm) c
Stopped on error: reverted: "assertion failed: 100 != 120"
  StdAssertions.assertEq(uint256, uint256) at lib/forge-std/src/StdAssertions.sol:121
 121              vm.assertEq(left, right);
(sevm) up
#1  FailTest.testBalance() at Fail.t.sol:9
```

Fire one at the prompt against the frame you are stopped in:

```
(sevm) vm.warp(12345)
(sevm) p block.timestamp
$1 = 12345  (uint256)
```

Interactive arguments are plain literals, not Solidity expressions. `help cheatcodes` lists
the implemented set, generated from the registry so it cannot drift; `help foundry` covers
project resolution and installs.

**Not yet supported:** `expectRevert`/`expectEmit`/`expectCall`/`mockCall`, `ffi`, forking,
and fuzz/invariant argument generation.

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
calls to `internal`/`private` functions all work. Results are cached per expression, so a
`display` costs one compile.

`msg.*` reads the frame you are stopped in, calldata included:

```
(sevm) p msg.sig
$4 = 0x26784590  (bytes4)
(sevm) p msg.data.length
$5 = 68  (uint256)
(sevm) p abi.decode(msg.data[36:], (uint256))
$6 = 64  (uint256)
```

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

`git` must be on PATH: sevm clones missing libraries itself and never shells out to
`forge`. The first run for a given library needs the network; after that the clone under
`lib/` is reused. solc is downloaded on demand into `~/.solcx`, at the version the
pragmas ask for or the one `foundry.toml` pins.

Tested against web3 7.16, py-evm 0.12.1b1, eth-tester 0.13.0b1, solc 0.8.28, forge-std
1.16.2, Textual 8.2.8, Python 3.12.

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
| `libs.py` | import scanning, library lookup, clone, remapping |
| `commands.py` | the gdb command surface, shared by both frontends |
| `console.py` / `tui/` | plain-text and Textual frontends |
| `cli.py` | `sevm run` and `sevm compile` |

## Tests

```bash
uv run pytest -q                                   # 307 tests, no network
SEVM_NETWORK_TESTS=1 uv run pytest -q -m network   # 4 more, against real forge-std and npm
```

Covers artifacts and source maps, the stop policy for every step mode, breakpoints and
watchpoints, cross-contract frames, reverts and panics, expression evaluation, storage
decoding, local variables across scoping, the whole command surface, mutation, library
install and remapping derivation, the assertion engine, and a headless render of the TUI.

The default run clones forge-std from a git repo built in `tmp_path`, so it needs no
network. The `network` tests install the real forge-std and openzeppelin, and one of them
fails if forge-std declares an assertion sevm does not implement.
