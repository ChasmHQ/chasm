# sevm

`sevm` is an interactive Solidity debugger and EVM playground, built for red-team dynamic analysis and running on Py-EVM.

- **Full control**: reading the machine is only half of it. Here you are `root` on the EVM, free to rewrite state, the stack, memory, storage, and gas while the transaction is still live.
- **Foundry compatible**: if you know Foundry, you know sevm. Same tests, same cheatcodes.
- **See everything**: no source, no problem. sevm maps every public function and the flow through it, wired into the decompiled output, so you can see what storage and memory changed at each step.

![screenshot](./assets/screenshot.png)

**Table of Contents**
- [Install](#install)
- [Usage](#usage)
  - [Debug a Foundry test](#debug-a-foundry-test)
  - [Debug a web3.py script](#debug-a-web3py-script)
  - [Set breakpoints](#set-breakpoints)
  - [Inspect the frame](#inspect-the-frame)
  - [Evaluate Solidity](#evaluate-solidity)
  - [Read local variables](#read-local-variables)
  - [Watch a storage variable](#watch-a-storage-variable)
  - [Change the running transaction](#change-the-running-transaction)
  - [Force an out-of-gas](#force-an-out-of-gas)
  - [Profile gas](#profile-gas)
  - [Debug a failing assertion](#debug-a-failing-assertion)
  - [Use Foundry cheatcodes](#use-foundry-cheatcodes)
  - [Run Yul at the prompt](#run-yul-at-the-prompt)
  - [Work without source](#work-without-source)
  - [Compile a project](#compile-a-project)
  - [Tips](#tips)
- [Reference](#reference)
- [Development](#development)
  - [Test](#test)

## Install

To install sevm, run the following commands:

```bash
git clone https://github.com/ChasmHQ/debugger && cd debugger

# if uv is not installed
pipx install uv

uv tool install .
```

## Usage

sevm has the following subcommands:
- `run`: debug a Foundry `.t.sol` test, or a web3.py driver script.
- `compile`: compile the contracts and report what sevm sees.

Options go **before** the target. Everything after a `.py` target is forwarded to that
script. For detailed information on each command and its options, run:

```
sevm -h
sevm <COMMAND> -h
```

By default `sevm run` opens the fullscreen interface in the screenshot above. Every example
below uses `--console`, the plain-text frontend, because it pastes into a README. The
commands are the same in both.

### Debug a Foundry test

Point `sevm run` at a `.sol` test. A lone test file can import forge-std and other
libraries without an existing Foundry project. sevm resolves the imports, clones what is
missing, writes the remappings, and breaks on the first line of the test body:

```console
$ sevm run --console -y /tmp/scratch/Vault.t.sol
standalone test at /tmp/scratch; compiling ...
installing forge-std from https://github.com/foundry-rs/forge-std @ v1.16.2
installing @openzeppelin/contracts from https://github.com/OpenZeppelin/openzeppelin-contracts @ v5.7.0
wrote 2 remapping(s) to remappings.txt
wrote 25 artifact(s) to out/sevm
debugging 1 test(s): VaultTest.testDeal
Breakpoint 1, VaultTest.testDeal() at Vault.t.sol:11
  11          vm.deal(alice, 3 ether);
```

Each test gets a fresh deploy, a `setUp()` and the test call, as forge isolates them. With
no filter, sevm debugs every test in the file and `continue` runs to the next test body.
`-m/--match` narrows to test functions matching a substring, `--match-contract` to a
contract:

```
sevm run --console -m testDeposit test/Vault.t.sol
```

Inside a project with a `foundry.toml`, nothing is fetched. The project's `remappings.txt`,
`lib/`, solc pin and `evm_version` are used as they are.

### Debug a web3.py script

Your script needs no changes. It drives web3.py against an in-process Py-EVM chain exactly
as it already does, and sevm compiles the contracts, patches Py-EVM, runs the script on a
worker thread, and stops the first time execution enters code it recognises:

```console
$ sevm run --console --contracts tests/contracts examples/debug_bank.py
compiling tests/contracts ...
cache hit (3 sources)
4 contract(s): Bank, Callee, Locals, Vault
Bank.constructor(string) at Bank.sol:31
  31      constructor(string memory _name) payable {
(sevm)
```

Recognition is by bytecode, not by instrumentation, which is why an unmodified script
works. The first stop is usually the constructor. Skip past it with startup commands, which
run before the prompt opens:

```
sevm run -x 'b _credit' -x c --contracts tests/contracts examples/debug_bank.py
```

### Set breakpoints

Break on a function, a source line, a raw program counter, or on every occurrence of an
opcode in any contract:

```
(sevm) b _credit                 # a function, by name or Contract.name
(sevm) b Bank.sol:46             # a source line, snapped to the next line with code
(sevm) b *0x108                  # a raw program counter
(sevm) b SSTORE                  # every SSTORE, anywhere
(sevm) b Bank.sol:46 if amount > 1 ether
```

An opcode breakpoint is the way into code you have no line numbers for:

```console
(sevm) b SSTORE
Breakpoint 1 on every SSTORE
(sevm) c
Breakpoint 1, Bank.constructor(string) at Bank.sol:32
  32          owner = msg.sender;
```

sevm also stops on reverts by default, at the failing instruction, and decodes the reason
as `reverted: "..."`, `panic 0x11`, or a custom error with its arguments.

### Inspect the frame

`info registers` is the machine at a glance:

```console
(sevm) info registers
pc           0x09b5
opcode       PUSH0 (0x5f)
gas          278,719 remaining / 217 used of 278,936
depth        0
sp           5 items
address      0xf2e246bb76df876cef8b38ae84130f4f55de395b
code         0xf2e246bb76df876cef8b38ae84130f4f55de395b
msg.sender   0xd89b2740bf17f3ed51b8c8890207e6c1a03aac6c
msg.value    2000000000000000000 (2 ether)
tx.origin    0xd89b2740bf17f3ed51b8c8890207e6c1a03aac6c
static       no
step         582
```

`info storage` decodes the whole layout, packed slots and all, without you working out a
single slot number:

```console
(sevm) info storage
Bank at 0x4f9da333dcf4e5a53772791b95c161b2fc041859
  slot   0+0  owner            address                = 0xf2e246bb...5b (cold)
  slot   0+20 feeBps           uint96                 = 25 (cold)
  slot   1+0  totalDeposits    uint256                = 0 (cold)
  slot   2+0  balances         mapping(address => uint256) = <mapping: query a key> (cold)
  slot   4+0  history          uint256[]              = [0 items] [] (cold)
  slot   5+0  name             string                 = "bank" (cold)
```

`bt` walks the call stack. Internal Solidity calls get their own frames even though the EVM
never changed depth:

```console
(sevm) bt
-> #0 Bank.deposit() at Test.t.sol:68
   #1 Bank at pc 0x380
   #2 DebugTest.testDeposit() at Test.t.sol:18
   #3 DebugTest at pc 0x193
```

`l` lists source around the program counter, `disas` disassembles around it with the source
line each instruction belongs to, and `x/NFU` examines memory in gdb syntax:

```console
(sevm) l
Bank.sol
 .   44      function _credit(address who, uint256 amount) internal {
 .   45          uint256 fee = _fee(amount);
->   46          balances[who] += amount - fee;
 .   47          totalDeposits += amount - fee;
 .   48          history.push(amount);
     49      }
(sevm) disas
   0092  OR L33
   0093  SWAP1 L33
=> 0094  SSTORE L33
   0095  POP L33
   0096  DUP1 L34
(sevm) x/32xb 0x80
0x0080: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0x0090: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
(memory is 96 bytes; the rest reads as zero)
```

### Evaluate Solidity

`p` compiles the expression as real Solidity against the paused contract, runs it on a
state snapshot, and throws the snapshot away, so it cannot disturb the run:

```console
(sevm) p balances[who] + 100 ether
$1 = 100000000000000000000 (100 ether)  (uint256)
(sevm) p keccak256(abi.encode(who, amount))
$2 = 0x6515e1c6183591491b775874fa6d6c2f905cfd817ab77e8ece0958bcc578bf7b  (bytes32)
(sevm) p accounts[msg.sender].nickname
$3 = "hodler"  (string memory)
(sevm) p type(uint256).max
$4 = 115792089237316195423570985008687907853269984665640564039457584007913129639935  (uint256)
```

Because it *is* Solidity, `internal` and `private` functions are callable too:

```console
(sevm) p _fee(1 ether)
$5 = 2500000000000000 (0.002500 ether)  (uint256)
```

`p` discards its side effects. `call` keeps them:

```console
(sevm) call history.push(7)
done (gas 44484)
(sevm) p history.length
$6 = 1  (uint256)
```

`display` re-evaluates an expression at every stop, and `ptype` reports a type:

```console
(sevm) display fee
1: fee = <EvalError: local `fee` cannot be used in an expression: this instruction
   allocates it; step once to see it>
(sevm) n
Bank._credit(address, uint256) at Bank.sol:46
  46          balances[who] += amount - fee;
1: fee = 5000000000000000 (0.005000 ether)
(sevm) ptype balances[who]
type = uint256
```

### Read local variables

solc emits no location for locals, so sevm reconstructs them from the AST and the run's
stack heights. `info locals` names, types and decodes them:

```console
(sevm) info locals
  who            address            = 0x0278bdd7808aa64dc93c361ae55fc52cf1a918cf (param)
  amount         uint256            = 2000000000000000000 (2 ether) (param)
  fee            uint256            = <unavailable>  this instruction allocates it;
                                      step once to see it
(sevm) n
Bank._credit(address, uint256) at Bank.sol:46
  46          balances[who] += amount - fee;
(sevm) p amount - fee
$1 = 1995000000000000000 (1.995000 ether)  (uint256)
```

A local that cannot be read says why rather than printing a wrong number.

### Watch a storage variable

A watchpoint breaks when a value changes, and reports the old and new values:

```console
(sevm) watch totalDeposits
Watchpoint 2: totalDeposits (slot 0x1 of 0xf2e2..395b)
(sevm) c
Watchpoint 2 totalDeposits: 0xde0b6b3a7640000 -> 0x299060a1be4b8000
  Bank._credit(address, uint256) at Bank.sol:47
  47          totalDeposits += amount - fee;
```

Watchpoints work on state variables, on mapping elements (`watch balances[msg.sender]`) and
on memory (`watch *0x80`). `rwatch` breaks on reads and `awatch` on either.

### Change the running transaction

`set var` writes through Solidity, so mappings, packed slots and structs all encode
correctly. The write lands in the transaction that is still running, and it changes the
outcome:

```console
(sevm) set var fee = 1 ether
fee = 1000000000000000000 (1 ether)
(sevm) set var balances[who] = 5 ether
balances[who] = 5000000000000000000 (5 ether)
(sevm) c
total deposits: 2000000000000000000
alice balance : 6000000000000000000
```

Without those two writes the same script prints `2995000000000000000` and
`1995000000000000000`.

The lower-level writes are there too:

```
(sevm) set $stack[0] = 0xc0ffee     # rewrite an operand before its opcode consumes it
(sevm) set $mem[0x80] = 1
(sevm) set $storage[0] = 0xdead
(sevm) jump 0x108                   # move the program counter, JUMPDESTs only
```

### Force an out-of-gas

Set the gas counter to whatever you need and let the transaction run into the wall:

```console
(sevm) set $gas = 100
$gas = 100
(sevm) c
Stopped on error: OutOfGas: Out of gas: Needed 2100 - Remaining 67 - Reason: SLOAD
  Bank._fee(uint256) at Bank.sol:41
  41          return (amount * feeBps) / 10000;
```

The failure is real, not simulated. The script's own output shows the deposit never
happened:

```
total deposits: 1000000000000000000
alice balance : 0
```

### Profile gas

`info gas` breaks the spend down by source line and by opcode:

```console
(sevm) info gas
limit        2,843,280
used         23,658
remaining    2,819,622
refund       0
this op      SSTORE base cost 0

gas by source line (highest first)
     22,164 L32   owner = msg.sender;
        214 L33   feeBps = 25;
         92 L31   constructor(string memory _name) payable {

gas by opcode
     20,000 SSTORE
      2,200 SLOAD
        160 JUMP
         90 PUSH2
```

### Debug a failing assertion

A failed forge-std assertion stops the debugger where it broke, with the comparison
decoded, and `up` walks back to your test:

```console
(sevm) c
Stopped on error: reverted: "assertion failed: 100 != 120"
  StdAssertions.assertEq(uint256, uint256) at lib/forge-std/src/StdAssertions.sol:121
 121              vm.assertEq(left, right);
(sevm) up
#1  FailTest.testBalance() at Fail.t.sol:9
```

This works because sevm implements the `vm.assert*` family as real comparisons, all 116
overloads that forge-std's own `assertEq` and friends call into.

### Use Foundry cheatcodes

Cheatcodes run against live Py-EVM state, both from inside the test and typed at the prompt
against the frame you are stopped in:

```console
(sevm) vm.warp(12345)
vm.warp -> ok
(sevm) p block.timestamp
$2 = 12345  (uint256)
```

Implemented: `warp roll fee chainId coinbase deal etch store load prank startPrank
stopPrank addr sign assume label`, plus the `vm.assert*` family. `console.log` prints as you
step. Interactive arguments are plain literals, not Solidity expressions.

Not yet supported: `expectRevert`, `expectEmit`, `expectCall`, `mockCall`, `ffi`, forking,
and fuzz or invariant argument generation.

### Run Yul at the prompt

Type a Yul builtin and it runs on the paused frame through Py-EVM's own opcode
implementations. This is the low-level twin of `set var`:

```console
(sevm) mload(0x40)
mload(0x40) -> $1 = 0x80 (128)  (gas 3)
(sevm) sstore(3, add(sload(3), 1))
sstore(3, add(sload(3), 1)) -> ok  (gas 22,103)
(sevm) asm mstore(0x80, 1); mstore8(0xa0, 0x61); mload(0x80)
mstore(0x80, 1) -> ok  (gas 3)
mstore8(0xa0, 0x61) -> ok  (gas 6)
mload(0x80) -> $2 = 0x1 (1)  (gas 3)
```

Gas is metered, reported, and then handed back, so poking at the machine cannot turn a
transaction that succeeds into one that runs out of gas. Control-flow builtins and the
frame terminators are refused, with the reason:

```console
(sevm) jump(0x10)
error: `jump`: Yul has no `jump`; use the debugger's `jump 0xPC` or `set $pc = 0xPC`
```

### Work without source

Convenience variables bypass solc, so they work on contracts you have no source for, and
they still mix into a Solidity expression:

```console
(sevm) p $pc
$1 = 99  (uint256)
(sevm) p $storage[1] + 1 ether
$2 = 1000000000000000000 (1 ether)  (uint256)
(sevm) p $stack[0]
$3 = 0  (uint256)
(sevm) x/32xb $mem[0x40]
0x0120: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
0x0130: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

The full set is `$pc` `$gas` `$gasused` `$depth` `$sp` `$step` `$stack[N]` `$mem[0x40]`
`$storage[1]`, plus the value history `$1` `$2` and so on.

### Compile a project

`sevm compile` runs the same build the debugger does and reports what it produced, which is
the fastest way to check whether a contract will be debuggable at all:

```console
$ sevm compile tests/contracts
cache hit (3 sources)
solc 0.8.36, optimizer off
  Bank.sol id=0 lines=99
  Locals.sol id=1 lines=103
  Vault.sol id=2 lines=39
  Bank.sol:Bank runtime=5302B source-map=yes state-vars=7 fns=15
  Bank.sol:Callee runtime=468B source-map=yes state-vars=1 fns=2
  Locals.sol:Locals runtime=3105B source-map=yes state-vars=3 fns=12
  Vault.sol:Vault runtime=664B source-map=yes state-vars=2 fns=5
```

`source-map=NO` means stepping will not work for that contract. Builds are cached per
compilation unit, so an edit recompiles the file and its importers rather than the project:

```console
$ time sevm compile .
wrote 26 artifact(s) to out/sevm
real  1.44

$ time sevm compile .
cache hit (20 sources)
real  0.51

$ time sevm compile .        # after editing one test
recompiled 1 of 20 sources
real  0.98
```

Artifacts land in `out/sevm/` in forge's layout, and the cache in `cache/sevm/`. Both are
cleared by `forge clean`. A directory with no `foundry.toml` gets nothing written into it.

### Tips

- Debug builds must keep the optimizer and via-IR **off**. Optimized codegen degrades the
  source map and makes stepping unreliable. sevm warns if you override that.
- Transactions do not need an explicit `gas=`. `eth_estimateGas` runs the transaction
  repeatedly and its early probes fail out-of-gas by design, so sevm runs those with the
  hook suspended and reports how many it skipped in `info frame`.
- `help`, `help assembly`, `help cheatcodes` and `help foundry` are generated from the
  registries at import time, so they cannot drift from what is implemented.
- `mstore(...)` at the prompt is assembly, but `p mstore(...)` is still Solidity, so a
  contract with a function of the same name stays reachable.
- Piping commands into `--console` makes a session scriptable:
  `printf 'b _credit\nc\ninfo locals\nq\n' | sevm run --console ...`

## Reference

| Guide | What is in it |
|---|---|
| [docs/commands.md](docs/commands.md) | every command, breakpoints and watchpoints, convenience variables |
| [docs/expressions.md](docs/expressions.md) | evaluating Solidity, reading and writing local variables |
| [docs/assembly.md](docs/assembly.md) | Yul builtins at the prompt, what is refused and why |
| [docs/foundry.md](docs/foundry.md) | projects, library install, the build cache, cheatcodes |

## Development

Set up an editable environment:

```
uv sync
uv run sevm --help
```

Format, lint, type-check, and build:

```
uv run ruff format src tests examples
uv run ruff check src tests examples
uv run mypy src
uv build
```

### Test

The tests are written using pytest and can be found in the `tests` directory.

```
uv run pytest -q                                   
SEVM_NETWORK_TESTS=1 uv run pytest -q -m network   
```

The default run builds a forge-std fixture in `tmp_path`, so it needs no network. The
network tests install the real forge-std and OpenZeppelin repositories, and one of them
fails if forge-std declares an assertion sevm does not implement.

## License

MIT. See [LICENSE](LICENSE).
