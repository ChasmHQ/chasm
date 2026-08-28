# Inline assembly (Yul) at the prompt

Type a Yul builtin at the prompt and it runs on the frame you are stopped in, using
Py-EVM's real opcode implementations. This is the low-level twin of `set var`. `p` answers
a question on a state snapshot that is then thrown away, assembly writes to the machine
that is actually running.

[back to README](../README.md)

```bash
(sevm) mload(0x40)
mload(0x40) -> $1 = 0x80 (128)  (gas 3)
(sevm) mstore(0x80, 0xdeadbeef)
mstore(0x80, 0xdeadbeef) -> ok  (gas 9)
(sevm) sstore(3, add(sload(3), 1))
sstore(3, add(sload(3), 1)) -> ok  (gas 22,103)
(sevm) asm mstore(0x80, 1); mstore8(0xa0, 0x61); mload(0x80)
mstore(0x80, 1) -> ok  (gas 3)
mstore8(0xa0, 0x61) -> ok  (gas 6)
mload(0x80) -> $2 = 0x1 (1)  (gas 3)
(sevm) keccak256(0x80, 32)
keccak256(0x80, 32) -> $3 = 0xb10e2d52...b7fa0cf6  (gas 36)
```

Calls nest exactly as in `assembly { }`. Reads print their value and enter the value
history as `$N`. `asm` (also `assembly`, `yul`) is the explicit form and takes several
statements separated by `;`. A bare `mstore(...)` line is recognised on its own.

Arguments are decimal or hex literals, `1 ether`, `true` or `false`, a 32-byte string
literal (`"hi"`, right-padded as Yul pads it), a nested call, or any convenience variable:
`mstore(0x80, $storage[1])`, `sstore(0, $stack[0])`.

## Two deliberate departures from a real execution

* **Gas is metered, reported, and then handed back.** Poking at the machine must not be
  able to turn a transaction that succeeds into one that runs out of gas.
* **Memory expansion an op causes is kept**, because the op really did write there.

## What is refused

Refused, with the reason:

```bash
(sevm) jump(0x10)
error: `jump`: Yul has no `jump`; use the debugger's `jump 0xPC` or `set $pc = 0xPC`
```

`jump` `jumpi` `pc` `push*` `dup*` `swap*` are how the compiler implements control flow, so
Yul excludes them itself. `stop` `return` `revert` `invalid` `selfdestruct` would end the
frame under you, so `finish` runs it to its end instead.

Everything else in the EVM Yul dialect is available, including `keccak256`, `mcopy`,
`tload` and `tstore`, the `log*` family and the call opcodes. `help assembly` lists every
builtin with its arguments, generated from the builtin table at import time.

`mstore(...)` at the prompt is assembly, but `p mstore(...)` is still Solidity, so a
contract with a function of the same name stays reachable.
