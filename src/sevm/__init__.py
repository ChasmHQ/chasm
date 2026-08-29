"""sevm: an interactive EVM playground on Py-EVM, built for red-team dynamic analysis.

Where things live:

  cli.py         argument parsing and `main()`; `.py` vs `.sol` dispatch
  session/       the stepping engine: the Py-EVM patch, the threads, the stop policy
  commands/      the gdb-style command layer both frontends drive
  evaluate/      Solidity expression evaluation at a breakpoint
  assembly/      Yul typed at the prompt, run against the live frame
  cheatcodes/    the Foundry cheatcode and console.log intercept
  compile/       solc, the artifact model, and Foundry project resolution
  locals/        recovering Solidity locals from the EVM stack
  tui/           the Textual fullscreen frontend
  console.py     the plain-text frontend (`--console`)

  artifacts.py   forge-shaped `out/sevm/<File.sol>/<Contract>.json`
  breakpoints.py the breakpoint and watchpoint set
  cache.py       the on-disk build cache
  clipboard.py   copying through the platform's own tool
  decode.py      storage layout, calldata and revert decoding
  disasm.py      the opcode table and disassembler
  dispatch.py    the external dispatcher read back out of runtime bytecode
  foundry.py     the Foundry test runner
  frames.py      EVM and Solidity frames, and the snapshot handed to the UI
  libs.py        dependency resolution: imports -> repo -> clone -> remapping
  srcmap.py      solc source maps: pc <-> source location
"""

__version__ = "0.1.0"
