# Examples

Everything the [README](../README.md) shows is runnable from here.

```
examples/
├── bank/              a Foundry project: src/Bank.sol + test/Bank.t.sol
├── standalone/        a lone .t.sol with no project, to drive the auto-install
├── mainnet/           a synthetic offline historical-replay capture
└── debug_bank.py      a web3.py script that deposits into Bank
```

```bash
chasm run examples/bank/test/Bank.t.sol                        # a Foundry test
chasm run -y examples/standalone/Vault.t.sol                   # clones forge-std first
chasm run --contracts examples/bank/src examples/debug_bank.py # a web3.py script
```

Running the first two writes `lib/`, `out/`, `cache/` and `remappings.txt` next to the
target. All of it is git-ignored, and `forge clean` removes it.

The [synthetic replay capture](mainnet/synthetic-replay.json) needs neither an RPC nor a
compiler. It uses invented blocks and hand-derived REVERT/STOP receipts, not historical
mainnet evidence:

```bash
uv run chasm replay --offline examples/mainnet/synthetic-replay.json --console -x 'info registers' -x c
```

See [mainnet replay](../docs/fork.md) for the support interval and validation boundaries.
