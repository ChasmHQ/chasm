# Live tests

End-to-end flows that drive a real Chasm window through the local control
API. `cargo test` does not run them: they need a debug build, Foundry in PATH,
port 8545 free and, for `fork`, network access.

```bash
cargo build
python tests/live/run.py                 # every flow
python tests/live/run.py revert history  # bank always runs first
python tests/live/run.py stress           # large fixtures and timing budgets; slow
CHASM_SKIP_FORK=1 python tests/live/run.py
```

`run.py` copies `contracts/` to a temporary folder, starts
`target/debug/chasm` on it with `--control-token-file`, runs the flows in
`flows.py` in order and prints a table. Your own `.chasm/` folder is never
touched. The private-key check derives Anvil's public test key with `cast`, so no
key is stored here.
