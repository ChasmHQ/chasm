# Solidity expressions and local variables

Use Solidity syntax to inspect the paused frame and work with reconstructed local values.

[Back to README](../README.md)

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
