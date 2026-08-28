# Examples

Everything the [README](../README.md) shows is runnable from here.

```
examples/
├── bank/              a Foundry project: src/Bank.sol + test/Bank.t.sol
├── standalone/        a lone .t.sol with no project, to drive the auto-install
└── debug_bank.py      a web3.py script that deposits into Bank
```

```bash
sevm run examples/bank/test/Bank.t.sol                        # a Foundry test
sevm run -y examples/standalone/Vault.t.sol                   # clones forge-std first
sevm run --contracts examples/bank/src examples/debug_bank.py # a web3.py script
```

Running the first two writes `lib/`, `out/`, `cache/` and `remappings.txt` next to the
target. All of it is git-ignored, and `forge clean` removes it.
