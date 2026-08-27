# Inline assembly (Yul) at the prompt

Run EVM operations against the live frame with sevm's prompt-level Yul dialect.

[Back to README](../README.md)

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
