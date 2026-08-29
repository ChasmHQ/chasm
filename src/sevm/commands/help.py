"""The `help` text.

`help assembly` and `help cheatcodes` are generated from the builtin table and the cheat
registry at import time, so adding either documents itself. The rest is written by hand.
"""

from __future__ import annotations

import textwrap

from ..assembly import listing as assembly_listing
from ..cheatcodes import all_specs as cheat_specs
from ..cheatcodes import listing as cheat_listing

HELP_SUMMARY = """
[bold]Execution[/bold]
  [cyan]c[/cyan]ontinue               run until a breakpoint
  [cyan]n[/cyan]ext [N]                  next Solidity line, stepping over calls
  [cyan]s[/cyan]tep [N]                  next Solidity line, stepping into calls
  [cyan]si[/cyan] / [cyan]ni[/cyan] [N]               one opcode, into / over calls
  [cyan]finish[/cyan]                 run to the end of this frame
  [cyan]u[/cyan]ntil LOC              run to a line or *PC
  [cyan]reset[/cyan]                 re-run the script: fresh chain, same breakpoints
  [cyan]run[/cyan] [ARGS]             re-run, replacing the script's arguments
                         ([dim]run 0x<hex>[/dim] sends new raw calldata;
                         [dim]@file[/dim] reads an argument from a file)

[bold]Breakpoints[/bold]
  [cyan]b[/cyan] FILE:LINE            break on a source line
  [cyan]b[/cyan] FUNC                 break on a function
  [cyan]b[/cyan] SSTORE               break on every occurrence of an opcode
  [cyan]b[/cyan] *0x108               break on a program counter
  [cyan]b[/cyan] LOC if EXPR          conditional, EXPR is Solidity
  [cyan]tbreak[/cyan] / [cyan]d[/cyan]elete N / [cyan]disable[/cyan] / [cyan]enable[/cyan]
  [cyan]watch[/cyan] EXPR             break when a storage value changes

[bold]Inspection[/bold]
  [cyan]p[/cyan] EXPR                 evaluate Solidity: [dim]p balances[msg.sender] + 100 ether[/dim]
  [cyan]call[/cyan] EXPR              evaluate and KEEP the side effects
  [cyan]ptype[/cyan] EXPR             report the Solidity type
  [cyan]display[/cyan] EXPR           re-evaluate at every stop
  [cyan]x[/cyan]/NFU ADDR             examine memory: [dim]x/32xb 0x40[/dim]
  [cyan]bt[/cyan] / [cyan]f[/cyan] N / [cyan]up[/cyan] / [cyan]down[/cyan]   call stack, EVM and Solidity frames
  [cyan]l[/cyan]ist [LINE]                  source listing
  [cyan]disas[/cyan]semble            disassembly around the pc
  [cyan]copy[/cyan] [CMD]                  put a command's output on the system clipboard
  [cyan]i[/cyan]nfo TOPIC             registers, breakpoints, frame, args, locals,
                         storage, gas, logs, sources, functions [dim](help info)[/dim]

[bold]Mutation[/bold]
  [cyan]set var[/cyan] X = V          write storage through Solidity: [dim]set var balances[a] = 5 ether[/dim]
                         a bare local name writes its stack slot: [dim]set var fee = 1 ether[/dim]
  [cyan]set[/cyan] $pc = 0x108        jump; [cyan]set[/cyan] $gas = N; [cyan]set[/cyan] $stack[0] = V; [cyan]set[/cyan] $storage[1] = V

[bold]Assembly[/bold]
  [cyan]mstore[/cyan](0x80, 1)        type a builtin call straight at the prompt
  [cyan]sload[/cyan](3)               reads print their value and enter the history as $N
  [cyan]sstore[/cyan](3, add(sload(3), 1))    calls nest, exactly as in `assembly { }`
  [cyan]asm[/cyan] YUL                the explicit form; takes `;`-separated statements
                         [dim]every write shows up in the panes at once[/dim]

[bold]Foundry cheatcodes[/bold]
  [cyan]vm.warp[/cyan](1735689600)    block.timestamp; [cyan]vm.roll[/cyan](N) block.number
  [cyan]vm.deal[/cyan](addr, 10 ether)   set a balance
  [cyan]vm.prank[/cyan](addr)         rewrite msg.sender for the next call
  [cyan]vm.store[/cyan] / [cyan]vm.load[/cyan] / [cyan]vm.etch[/cyan] / [cyan]vm.label[/cyan] / [cyan]vm.sign[/cyan] / [cyan]vm.addr[/cyan]
  [cyan]vm.assertEq[/cyan](a, b)      the assertions forge-std calls; see [cyan]help cheatcodes[/cyan]

[bold]Convenience variables[/bold]
  $pc $gas $gasused $depth $sp $step $stack[N] $mem[0x40] $storage[1] $1 $2 ...

[bold]In the TUI[/bold]
  [cyan]f2[/cyan]                    hide the low-level panes; SOURCE takes the space
  [cyan]copy[/cyan] [dim]CMD[/dim]              run CMD and put its output on the system clipboard
  [cyan]copy[/cyan]                  the last output again, untruncated

  STACK labels the slots that hold this frame's locals.
  A pane you scroll stays where you left it; scroll back, or click the marker in
  its border, to have it follow execution again.

[dim]help <topic> for detail. topics: breakpoints, print, memory, mutation, assembly, cheatcodes, foundry, gas, locals, info[/dim]
"""

HELP_TOPICS = {
    "info": """
[bold]info TOPIC[/bold]
  info registers         pc, opcode, gas, depth, stack height, addresses, static, step
                         [dim]abbreviated `info r`[/dim]
  info frame             the EVM frame: depth, kind, artifact, gas, address,
                         code_address, sender, value, calldata, internal call stack
                         [dim]calldata colours the selector and each argument word apart[/dim]
  info args              the arguments of the frame you are in, named and decoded
                         [dim]an internal frame reports its own parameters, not calldata[/dim]
  info locals            the frame's locals; see [cyan]help locals[/cyan]
  info storage [C]       every state variable of C (default: this contract), decoded
                         from the layout, each marked warm or cold
  info gas               limit/used/remaining/refund plus a per-line and per-opcode
                         profile; see [cyan]help gas[/cyan]
  info logs              events emitted so far in this frame, named against the ABI
  info breakpoints       breakpoints and watchpoints, with hit counts
                         [dim]abbreviated `info b`; `info watchpoints` is the same list[/dim]
  info sources           the compiled sources and their file ids
  info functions [PAT]   every function, with visibility and line, filtered by PAT
""",
    "breakpoints": """
[bold]Breakpoints[/bold]
  b Bank.sol:46             a source line (snapped forward to the next line with code)
  b deposit                 a function, by name or Contract.name
  b SSTORE                  every SSTORE, in any contract
  b *0x108                  a raw program counter
  b Bank.sol:46 if totalDeposits > 1 ether
                            the condition is real Solidity, evaluated at the stop

Conditions can read local variables, state variables, msg/tx/block and call view
functions. A condition that fails to evaluate still breaks, as gdb does, and
`info breakpoints` says why.

  watch totalDeposits       stop when the value changes, reporting old -> new
  watch balances[0xabc..]   mapping elements work too
  watch *0x80               a 32-byte window of memory
""",
    "print": """
[bold]print[/bold]
`p EXPR` compiles EXPR as real Solidity against the paused contract and runs it on a state
snapshot that is thrown away afterwards, so it cannot disturb the run.

  p owner                            a state variable
  p balances[msg.sender] + 100 ether units and arithmetic
  p accounts[owner].nickname         structs and mappings
  p _fee(msg.value)                  internal and private functions
  p keccak256(abi.encode(owner))     any builtin
  p address(this).balance
  p $storage[1] + 1 ether            mix in low-level convenience variables

`msg.*` reads the frame you are stopped in, `msg.data` and `msg.sig` included: they carry
the frame's own calldata, not the call the debugger makes to evaluate the expression.

  p msg.sig                          the selector that got you here
  p abi.decode(msg.data[4:], (uint256))   arguments straight out of calldata

Results enter the value history as $1, $2 ... and can be reused in later expressions.
`call EXPR` is the same but KEEPS the effects, which is how you mutate through Solidity.
""",
    "memory": """
[bold]x, examine memory[/bold]
Same syntax as gdb: x/NFU ADDR.
  N  count      F  format x d u o t c s   U  unit b h w g (1, 2, 4, 8 bytes)

  x/32xb 0x40    32 bytes in hex
  x/4xg 0x80     4 eight-byte words
  x/s 0xa0       a string

Solidity's fixed layout is annotated for you: 0x00-0x3f scratch, 0x40 free memory
pointer, 0x60 zero slot.
""",
    "mutation": """
[bold]Changing state mid-execution[/bold]
  set var owner = msg.sender          writes storage through Solidity, so packed slots,
  set var balances[alice] = 5 ether   mappings and structs are all encoded correctly
  call deposit()                      run a function and keep the effects

  set $stack[0] = 0xc0ffee            rewrite an operand before the opcode consumes it
  set $gas = 100                      force an out-of-gas at an exact instruction
  set $mem[0x80] = 1
  set $storage[0] = 0xdead            raw slot write, bypassing the layout
  jump 0x108                          move the program counter (JUMPDESTs only)

  mstore(0x80, 1)                     raw assembly; see `help assembly`
  vm.deal(alice, 10 ether)            Foundry cheatcodes; see `help cheatcodes`

Every one of these re-reads the machine as soon as it lands, so the STACK, MEMORY,
STORAGE and VARIABLES panes show the change without stepping first.
""",
    "gas": """
[bold]info gas[/bold]
Shows the frame's limit, used, remaining and refund, the base cost of the current opcode,
then a profile: gas attributed to each source line and to each opcode, measured as the
real meter delta per instruction rather than from a cost table.
""",
    "locals": """
[bold]Local variables[/bold]
  info locals            the frame's locals, named, typed and decoded
  p amount - fee         expressions over them, in real Solidity
  b LOC if amount > 1     conditions over them
  set var fee = 1 ether  writes the local's stack slot

solc emits no location info for locals; sevm reconstructs it from the AST (name, type,
scope) plus the stack slot at the current pc. A local shadows a state variable of the
same name, as in the contract.

Not readable, reported as <unavailable> rather than guessed at:
  assembly variables     no AST declaration exists
  storage pointers       the slot number is shown; index the state variable instead
  calldata references    use `info args`
  a local on its own declaration line   step once, then read it
""",
}


def _assembly_help() -> str:
    """`help assembly`, generated from the builtin table so the two cannot disagree."""
    rows = [
        f"  {builtin.signature:<44} {builtin.summary}" for builtin in assembly_listing()
    ]
    return (
        """
[bold]Inline assembly (Yul)[/bold]
A Yul builtin typed at the prompt runs for real, on the frame you are stopped in. Unlike
`p`, which evaluates on a throwaway snapshot, assembly writes to the live machine.

  mstore(0x80, 1)                     write memory
  sstore(3, add(sload(3), 1))         read-modify-write a slot, nested as in Yul
  mload(0x40)                         reads print, and enter the value history as $N
  keccak256(0x80, 0x40)               hash a memory range
  asm mstore(0x40, 0xa0); mstore8(0xa0, 0x61)     `asm` also takes several statements

Arguments: decimal or hex literals, `1 ether`, `true`/`false`, a right-padded string
literal ("hi"), a nested call, or a convenience variable, e.g. `mstore(0x80, $storage[1])`.

Gas is metered and refunded, so this can't turn a passing run into an out-of-gas. Memory
expansion sticks, since the write really happened.

Refused: jump/jumpi/pc/push*/dup*/swap* (Yul excludes these itself) and
stop/return/revert/invalid/selfdestruct (would end the frame; use `finish` instead).

`mstore(...)` at the prompt is assembly; `p mstore(...)` is Solidity, so a contract
function of the same name stays reachable.

[bold]Builtins[/bold]
"""
        + "\n".join(rows)
        + "\n"
    )


def _cheatcode_help() -> str:
    """`help cheatcodes`, generated from the registry: documented set == implemented set."""
    rows = [
        f"  vm.{spec.signature:<42} {spec.doc}" for spec in cheat_listing() if spec.doc
    ]
    names = sorted({spec.name for spec in cheat_specs() if spec.family == "assert"})
    asserts = textwrap.wrap(
        "  ".join(names), width=84, initial_indent="  ", subsequent_indent="  "
    )
    return (
        """
[bold]Foundry cheatcodes[/bold]
The same cheatcodes a `.t.sol` calls, available at the prompt against the live state of
the frame you are stopped in. Arguments are plain literals: an integer, `1 ether`, a 0x
address or bytes value, `true`/`false`, or a quoted string.

  vm.warp(1735689600)
  vm.deal(0xf39F..2266, 10 ether)
  vm.startPrank(0xf39F..2266)
  vm.load(0xf39F..2266, 0x00)      returning cheats print their result

`vm.prank(...)` at the prompt and inside the test hit the same intercept. An unimplemented
selector reverts with a clear message instead of doing nothing.

[bold]Implemented[/bold]
"""
        + "\n".join(rows)
        + "\n\n[bold]Assertions[/bold]\n"
        + "\n".join(asserts)
        + """

forge-std's own `assertEq(a, b)` calls these, so a failed assertion in a test reverts with
`assertion failed: 1 != 2` (or your own message, when you pass one).
"""
    )


HELP_TOPICS["assembly"] = _assembly_help()
HELP_TOPICS["asm"] = HELP_TOPICS["assembly"]
HELP_TOPICS["yul"] = HELP_TOPICS["assembly"]
HELP_TOPICS["foundry"] = """
[bold]Foundry projects and libraries[/bold]
sevm compiles a `.t.sol` the way forge does, and installs what is missing first.

  sevm run test/Counter.t.sol        inside a project: its foundry.toml, remappings.txt
                                     and lib/ are used as they are
  sevm run /tmp/scratch/Demo.t.sol   outside one: writes foundry.toml, clones forge-std
                                     into lib/, appends remappings.txt
  sevm run -y ...                    skip the confirmation prompt
  sevm run --no-install ...          resolve from disk only; refuse a missing import

An unresolved import is looked up in sevm's table, then on npm, and cloned from its git
repository at the newest release tag:

  import "@openzeppelin/contracts/token/ERC20/ERC20.sol";
  -> lib/openzeppelin-contracts, and the remapping to reach it

Libraries are cloned, never updated: the pin stays until you change it, as with
`forge install`. git is required; the `forge` binary is not.
"""

HELP_TOPICS["cheatcodes"] = _cheatcode_help()
HELP_TOPICS["vm"] = HELP_TOPICS["cheatcodes"]
