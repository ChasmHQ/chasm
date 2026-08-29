# Command reference

Every gdb verb and abbreviation below behaves as it does in gdb. The differences are that
expressions are Solidity and the machine underneath is the EVM.

[back to README](../README.md)

## Execution

| Command | Meaning here |
|---|---|
| `c` / `continue` | run until a breakpoint |
| `reset` | re-run the target script from scratch: fresh chain, same breakpoints |
| `run [ARGS]` | re-run, replacing the script's arguments (`run 0x<hex>` re-sends raw calldata) |
| `n` / `next [N]` | next Solidity line, stepping over calls |
| `s` / `step [N]` | next Solidity line, stepping into calls |
| `si` / `ni [N]` | one opcode, into / over `CALL` |
| `finish` | run to the end of the current frame |
| `u` / `until LOC` | run to a line or `*PC` |

`step` and `next` understand *internal* Solidity calls. Those compile to `JUMP`, so EVM
depth never changes, and sevm tracks them through the source map's `i` and `o` jump
markers.

Every stop ends with a one-line machine echo, so opcode-level stepping always shows what
the last step did to the machine — the stack height as old->new when it changed (a `POP`
reads `sp 13->12`), plus pc, opcode, gas remaining and step count:

```bash
(sevm) si
pc 0x0077  POP     sp 13->12  gas 2,843,413  step 5
```

`reset` and `run` work mid-transaction and after the program has finished; the prompt
does not exit when the program ends. Breakpoints, watchpoints and `display` expressions
survive the restart; the chain does not, because a fresh script run builds a fresh
tester chain — a genuine clean slate for iterating on calldata. An argument of the form
`@path` (at launch, or to `run`) is replaced by that file's whitespace-stripped contents:
payload hex runs to thousands of characters, past what a Windows console or command line
will carry. The same `@file` syntax works at launch
(`sevm run ... script.py @payload.hex`), at the prompt (`run @payload.hex`) and anywhere
else the script's arguments are set.

## Breakpoints

| Command | Meaning |
|---|---|
| `b Bank.sol:46` | a source line, snapped forward to the next line with code |
| `b deposit` | a function, by name or `Contract.name` |
| `b SSTORE` | every occurrence of an opcode, in any contract |
| `b *0x108` | a raw program counter |
| `b LOC if EXPR` | conditional, where `EXPR` is real Solidity |
| `tbreak` | fires once, then deletes itself |
| `delete N` / `disable` / `enable` / `info breakpoints` | management |
| `watch EXPR` | break when a storage value changes, reporting old to new |
| `rwatch EXPR` / `awatch EXPR` | break on read / either |

Watchpoints work on state variables, on mapping elements (`watch balances[msg.sender]`),
and on memory (`watch *0x80`).

sevm also stops on reverts by default, at the failing instruction, and decodes the reason
as `reverted: "..."`, `panic 0x11`, or a custom error with its arguments.

## Inspection

| Command | Meaning |
|---|---|
| `p EXPR` | evaluate a Solidity expression |
| `call EXPR` | evaluate and **keep** the side effects |
| `ptype EXPR` | report the Solidity type |
| `display EXPR` | re-evaluate at every stop |
| `x/NFU ADDR` | examine memory, gdb syntax; a bare `x` continues past the last dump, format reused |
| `bt` / `f N` / `up` / `down` | call stack and frame selection |
| `l` / `list` / `disas` | source listing / disassembly |
| `info registers` | pc, gas + refund, depth, stack height, memory and calldata sizes, `msg.*`, `tx.origin`, static flag |
| `info args` / `info locals` / `info storage` | frame args / locals / state vars, decoded |
| `info gas` | limit, used, refund, and a profile by source line and by opcode |
| `info frame` / `info logs` / `info sources` / `info functions` | the rest |

`info storage` decodes the whole layout, packed slots included:

```bash
(sevm) info storage
Bank at 0x4f9da333dcf4e5a53772791b95c161b2fc041859
  slot   0+0  owner            address                = 0xf2e246bb...5b (cold)
  slot   0+20 feeBps           uint96                 = 25 (cold)
  slot   1+0  totalDeposits    uint256                = 0 (cold)
  slot   2+0  balances         mapping(address => uint256) = <mapping: query a key> (cold)
  slot   4+0  history          uint256[]              = [0 items] [] (cold)
  slot   5+0  name             string                 = "bank" (cold)
```

## Mutation

| Command | Meaning |
|---|---|
| `set var owner = msg.sender` | write storage through Solidity |
| `set var balances[alice] = 5 ether` | mappings, packed slots and structs encode correctly |
| `call deposit()` | run a function and keep the effects |
| `set $stack[0] = 0xc0ffee` | rewrite an operand before the opcode consumes it |
| `set $gas = 100` | force an out-of-gas at an exact instruction — and at an out-of-gas stop, refill with `set $gas = N` then `c` to retry the failed instruction and keep going |
| `set var fee = 1 ether` | write a local's stack slot |
| `set $mem[0x80] = 1` / `set $storage[0] = 0xdead` | raw writes |
| `jump 0x108` | move the program counter, JUMPDESTs only |
| `mstore(0x80, 1)` / `asm YUL` | run inline assembly, see [assembly.md](assembly.md) |
| `vm.deal(alice, 10 ether)` | fire a Foundry cheatcode at the current frame |

Every one of these re-reads the machine as soon as it lands, so the STACK, MEMORY, STORAGE
and VARIABLES panes show the change without stepping first.

## Convenience variables

`$pc` `$gas` `$gasused` `$depth` `$sp` `$step` `$stack[N]` `$mem[0x40]` `$storage[1]`, plus
the value history `$1` `$2` and so on.

They bypass solc, so they work on contracts with no source and still mix into a Solidity
expression:

```bash
(sevm) p $storage[1] + 1 ether
```
