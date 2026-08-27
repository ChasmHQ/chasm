# Foundry tests and cheatcodes

Use sevm with standalone Foundry tests, existing projects, dependencies, and cheatcodes.

[Back to README](../README.md)

`sevm run` dispatches on the extension: a `.py` argument is the web3 driver, a `.sol`
argument is a Foundry test.

```bash
sevm run test/Counter.t.sol                     # fullscreen TUI, opens in the first test
sevm run --console -m testDeposit Vault.t.sol   # plain text, one test by name
```

Each test gets a fresh deploy + `setUp()` + the test call, as forge isolates them, and the
debugger opens on the first line of the first test. `continue` runs to the next test body.
`-m/--match` narrows to test functions matching a substring, `--match-contract` to a
contract.

## A lone .t.sol with nothing installed

```solidity
// /tmp/scratch/Vault.t.sol
import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract VaultTest is Test {
    address alice = address(0xA11CE);

    function testDeal() public {
        vm.deal(alice, 3 ether);
        assertEq(alice.balance, 3 ether);
    }
}
```

```console
$ sevm run --console -y /tmp/scratch/Vault.t.sol
standalone test at /tmp/scratch; compiling ...
installing forge-std from https://github.com/foundry-rs/forge-std @ v1.16.2
installing @openzeppelin/contracts from https://github.com/OpenZeppelin/openzeppelin-contracts @ v5.7.0
wrote 2 remapping(s) to remappings.txt
debugging 1 test(s): VaultTest.testDeal
Breakpoint 1, VaultTest.testDeal() at Vault.t.sol:11
  11          vm.deal(alice, 3 ether);
(sevm)
```

That run leaves a directory `forge` also understands:

```
/tmp/scratch
├── Vault.t.sol
├── foundry.toml        [profile.default] with libs = ["lib"], no src/test
├── remappings.txt      forge-std/=lib/forge-std/src/
│                       @openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/
└── lib
    ├── forge-std                 v1.16.2
    └── openzeppelin-contracts    v5.7.0
```

Drop `-y` and sevm asks once before writing anything:

```console
$ sevm run --console /tmp/scratch/Vault.t.sol
sevm will:
  - create /tmp/scratch/foundry.toml
  - install forge-std, @openzeppelin/contracts into /tmp/scratch/lib
[y/N]
```

Answer `n` and sevm writes nothing, compiling against what is already there. `--no-install`
skips the prompt and refuses the same way:

```console
$ sevm run --console --no-install /tmp/scratch/Vault.t.sol
standalone test at /tmp/scratch; compiling ...
compile failed: unresolved import 'forge-std/Test.sol' in Vault.t.sol, and sevm was told
not to install it. Run again with -y to let it, or install it yourself:
  forge install <org>/<repo>
  echo 'forge-std/=lib/<repo>/src/' >> remappings.txt
```

## Inside a Foundry project

```console
$ sevm run --console test/Token.t.sol
foundry project at /work/mytoken; compiling ...
debugging 2 test(s): TokenTest.testMintAsOwner, TokenTest.testMintPrankRevertsForNonOwner
Breakpoint 1, TokenTest.testMintAsOwner() at test/Token.t.sol:18
  18          token.mint(bob, 100);
```

Nothing is fetched: the project's `foundry.toml`, `remappings.txt` and `lib/` are used as
they are, including its `solc` pin and `evm_version`. forge-std is cloned only when `lib/`
has none.

## How an import is resolved

sevm looks up the prefix in its own table (`forge-std`, `ds-test`, `solmate`, `solady`,
openzeppelin), then in npm's registry metadata for anything else, and clones the repository
it finds at the newest release tag. Prereleases are skipped. The remapping comes from where
the imported file actually landed in the clone, so `src/`, `contracts/` and flat layouts
all work.

A clone is a pin. sevm never updates it; to move a version, delete `lib/<name>` or run
`forge install <org>/<repo>@<tag>` yourself. git is required, the `forge` binary is not,
and only the first run for a given library needs the network.

## Build cache

The first run compiles. The next one does not.

```console
$ time sevm compile .
wrote 48 artifact(s) to out/sevm
solc 0.8.36, optimizer off
real  1.88
$ time sevm compile .
cache hit (40 sources)
real  0.61
$ $EDITOR src/Setup.sol
$ time sevm compile .
recompiled 3 of 40 sources
wrote 48 artifact(s) to out/sevm
real  1.04
```

A build leaves the same two directories forge does:

```
cache/sevm/     the compilation unit, keyed by solc version, settings and source content
out/sevm/       one JSON per contract, in forge's layout: out/sevm/Token.sol/Token.json
```

An edit invalidates the file you changed and everything that imports it; the rest is
reused, so solc only has to emit the parts that moved. `forge clean` clears both along with
forge's own.

Artifacts are nested under `out/sevm/` rather than written straight into `out/`, because
sevm compiles with the optimizer off: overwriting `out/Token.sol/Token.json` would leave
forge's cache calling it fresh and the next `forge test` running sevm's build. They carry
`abi`, `bytecode`, `deployedBytecode`, `methodIdentifiers`, `storageLayout` and the source
id, with forge's field names; `metadata` is the one thing missing, as sevm never asks solc
for it.

A directory with no `foundry.toml` gets nothing written into it: its cache lives under
`~/.cache/sevm/` and no artifacts are written at all. `--force` recompiles and rewrites the
entry, `--no-cache` (or `SEVM_NO_CACHE=1`) writes nothing anywhere.

## Cheatcodes

Cheatcodes run against live Py-EVM state and `console.log` prints as you step. Implemented:
`warp roll fee chainId coinbase deal etch store load prank startPrank stopPrank addr sign
assume label`, plus the full `vm.assert*` family (`assertEq`, `assertGt`,
`assertApproxEqRel`, the `*Decimal` forms, 116 overloads) that forge-std's own `assertEq`
calls into. A failed assertion stops the debugger where it broke, with the comparison:

```console
(sevm) c
Stopped on error: reverted: "assertion failed: 100 != 120"
  StdAssertions.assertEq(uint256, uint256) at lib/forge-std/src/StdAssertions.sol:121
 121              vm.assertEq(left, right);
(sevm) up
#1  FailTest.testBalance() at Fail.t.sol:9
```

Fire one at the prompt against the frame you are stopped in:

```
(sevm) vm.warp(12345)
(sevm) p block.timestamp
$1 = 12345  (uint256)
```

Interactive arguments are plain literals, not Solidity expressions. `help cheatcodes` lists
the implemented set, generated from the registry so it cannot drift; `help foundry` covers
project resolution and installs.

**Not yet supported:** `expectRevert`/`expectEmit`/`expectCall`/`mockCall`, `ffi`, forking,
and fuzz/invariant argument generation.
