# Solidity expressions and local variables

`p` compiles the expression as real Solidity against the paused contract, runs it on a
state snapshot, and throws the snapshot away, so it cannot disturb the run. `call` runs the
same thing and keeps the effects.

[back to README](../README.md)

## Evaluating Solidity

```
(sevm) p balances[who] + 100 ether
$1 = 100000000000000000000 (100 ether)  (uint256)
(sevm) p keccak256(abi.encode(who, amount))
$2 = 0x6515e1c6183591491b775874fa6d6c2f905cfd817ab77e8ece0958bcc578bf7b  (bytes32)
(sevm) p type(uint256).max
$3 = 115792089237316195423570985008687907853269984665640564039457584007913129639935  (uint256)
(sevm) p accounts[msg.sender].nickname
$4 = "hodler"  (string memory)
```

Because it *is* Solidity, operator precedence, checked arithmetic, the `ether` / `gwei` /
`days` units, casts, `keccak256`, `abi.encode`, `type(uint256).max`, struct and mapping
access, and calls to `internal` or `private` functions all work. Results are cached per
expression, so a `display` costs one compile.

`msg.*` reads the frame you are stopped in, calldata included:

```
(sevm) p msg.sig
$5 = 0xd0e30db0  (bytes4)
(sevm) p msg.data.length
$6 = 4  (uint256)
```

Calldata slices decode as they would in Solidity, so an argument can be pulled out of a
frame whose signature sevm has no source for:

```
(sevm) p abi.decode(msg.data[36:], (uint256))
```

A mapping or a struct has to be indexed rather than printed whole:

```
(sevm) ptype balances
error: EvalError: cannot display a whole mapping; index it with a key
```

## Local variables

`info locals` names, types and decodes the locals of the frame you are stopped in, and `p`
takes expressions over them:

```
(sevm) info locals
  who            address            = 0x0278bdd7808aa64dc93c361ae55fc52cf1a918cf (param)
  amount         uint256            = 2000000000000000000 (2 ether) (param)
  fee            uint256            = 5000000000000000 (0.005000 ether)
(sevm) p amount - fee
$1 = 1995000000000000000 (1.995000 ether)  (uint256)
(sevm) set var fee = 1 ether
```

solc emits no location for locals, so sevm reconstructs it from the AST and the run's stack
heights, the way Truffle and Remix do. A value shows only when the frame was observed from
entry, when the slot is still below the top of the stack, and when the current instruction
is inside the declaration's scope. Otherwise you get `<unavailable>` with the reason:

```
(sevm) info locals
  fee            uint256            = <unavailable>  this instruction allocates it;
                                      step once to see it
```

`set var` writes the stack slot directly and refuses memory and calldata locals rather than
corrupting a pointer.

Still not readable: `assembly` block variables (use `p $stack[N]`), storage-pointer locals
(index through them instead), calldata references (use `info args`), and a local read on
its own declaration line (run `n` first, then read it). Optimized builds are unreliable, so
debug builds keep the optimizer off.
