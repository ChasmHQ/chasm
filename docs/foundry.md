# Foundry projects, libraries and the build cache

sevm reads a Foundry project the way `forge` does, and writes back into it in a way that
cannot disturb `forge`. This page covers the parts the README summarises.

[back to README](../README.md)

`sevm run` dispatches on the extension. A `.py` argument is a web3 driver, a `.sol`
argument is a Foundry test.

```bash
sevm run test/Counter.t.sol                     # fullscreen TUI, opens in the first test
sevm run --console -m testDeposit examples/bank/test/Bank.t.sol   # one test by name
```

Each test gets a fresh deploy, a `setUp()` and the test call, as forge isolates them, and
the debugger opens on the first line of the first test. `continue` runs to the next test
body. `-m/--match` narrows to test functions matching a substring, `--match-contract` to a
contract.

## A lone .t.sol with nothing installed

```solidity
// examples/standalone/Vault.t.sol
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

```bash
$ sevm run --console -y examples/standalone/Vault.t.sol
standalone test at examples/standalone; compiling ...
installing forge-std from https://github.com/foundry-rs/forge-std @ v1.16.2
installing @openzeppelin/contracts from https://github.com/OpenZeppelin/openzeppelin-contracts @ v5.7.0
wrote 2 remapping(s) to remappings.txt
wrote 25 artifact(s) to out/sevm
debugging 1 test(s): VaultTest.testDeal
Breakpoint 1, VaultTest.testDeal() at Vault.t.sol:11
  11          vm.deal(alice, 3 ether);
```

That run leaves a directory `forge` also understands:

```
examples/standalone
├── Vault.t.sol
├── foundry.toml        [profile.default] with libs = ["lib"], no src/test
├── remappings.txt      forge-std/=lib/forge-std/src/
│                       @openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/
├── cache/sevm          the compilation unit, keyed by solc version, settings, sources
├── out/sevm            one JSON per contract, in forge's layout
└── lib
    ├── forge-std                 v1.16.2
    └── openzeppelin-contracts    v5.7.0
```

Drop `-y` and sevm asks once before writing anything:

```bash
$ sevm run --console examples/standalone/Vault.t.sol
sevm will:
  - create examples/standalone/foundry.toml
  - install forge-std, @openzeppelin/contracts into examples/standalone/lib
[y/N]
```

Answer `n` and sevm writes nothing, compiling against what is already there. A
non-interactive run declines by itself. `--no-install` skips the prompt and refuses the
same way, with the manual recipe:

```bash
$ sevm run --console --no-install examples/standalone/Vault.t.sol
compile failed: unresolved import 'forge-std/Test.sol' in Vault.t.sol, and sevm was told
not to install it. Run again with -y to let it, or install it yourself:
  forge install <org>/<repo>
  echo 'forge-std/=lib/<repo>/src/' >> remappings.txt
```

## Inside a Foundry project

```bash
$ sevm run --console examples/bank/test/Bank.t.sol
foundry project at examples/bank; compiling ...
debugging 3 test(s): BankTest.testDeposit, BankTest.testFeeIsTakenOnDeposit,
FailingAssertionTest.testBalanceIgnoresFee
Breakpoint 1, BankTest.testDeposit() at test/Bank.t.sol:17
  17          vm.prank(alice);
```

Nothing is fetched. The project's `foundry.toml`, `remappings.txt` and `lib/` are used as
they are, including its `solc` pin and `evm_version`. forge-std is cloned only when `lib/`
has none.

## How an import is resolved

sevm looks up the prefix in its own table (`forge-std`, `ds-test`, `solmate`, `solady`,
openzeppelin), then in npm's registry metadata for anything else, and clones the repository
it finds at the newest release tag. Prereleases are skipped. The remapping comes from where
the imported file actually landed in the clone, so `src/`, `contracts/` and flat layouts
all work.

A clone is a pin. sevm never updates it. To move a version, delete `lib/<name>` or run
`forge install <org>/<repo>@<tag>` yourself. git is required, the `forge` binary is not, and
only the first run for a given library needs the network.

## Build cache

The first run compiles. The next one does not.

```bash
$ time sevm compile .
wrote 27 artifact(s) to out/sevm
solc 0.8.36, optimizer off
real  1.95

$ time sevm compile .
cache hit (22 sources)
real  0.53

$ $EDITOR test/Bank.t.sol
$ time sevm compile .
recompiled 1 of 22 sources
wrote 27 artifact(s) to out/sevm
real  1.02
```

A build leaves the same two directories forge does:

```
cache/sevm/     the compilation unit, keyed by solc version, settings and source content
out/sevm/       one JSON per contract, in forge's layout: out/sevm/Token.sol/Token.json
```

An edit invalidates the file you changed and everything that imports it. The rest is
reused, so solc only has to emit the parts that moved. `forge clean` clears both of those
along with forge's own.

Artifacts are nested under `out/sevm/` rather than written straight into `out/`, because
sevm compiles with the optimizer off. Overwriting `out/Token.sol/Token.json` would leave
forge's cache calling it fresh, and the next `forge test` would run sevm's build. They
carry `abi`, `bytecode`, `deployedBytecode`, `methodIdentifiers`, `storageLayout` and the
source id, with forge's field names. `metadata` is the one thing missing, as sevm never
asks solc for it.

A directory with no `foundry.toml` gets nothing written into it. Its cache lives under
`~/.cache/sevm/` and no artifacts are written at all. `--force` recompiles and rewrites the
entry, `--no-cache` (or `SEVM_NO_CACHE=1`) writes nothing anywhere.

## Cheatcodes

Cheatcodes run against live Py-EVM state and `console.log` prints as you step.
Implemented:

- block env: `warp roll fee chainId coinbase prevrandao difficulty`, plus the
  `getBlockNumber getBlockTimestamp getChainId` readers.
- account state: `deal etch store load getNonce setNonce setNonceUnsafe resetNonce warmSlot`.
- identity: `prank startPrank stopPrank label getLabel`, including the overloads that also
  rewrite `tx.origin` and the `delegateCall` flag, which rewrites `msg.sender` *and*
  `address(this)` inside a DELEGATECALL the pranking contract makes.
- keys and wallets: `addr sign signCompact deriveKey rememberKey createWallet`.
- environment: the whole `vm.env*` family (`envBytes envUint envInt envAddress envBool
  envBytes32 envString`, their `(name, delimiter)` array forms, `envOr`, `envExists`,
  `setEnv`), reading the process environment exactly as forge does.
- pure transforms: `toString parseUint parseInt parseBool parseAddress parseBytes
  parseBytes32 toBase64 toBase64URL toLowercase toUppercase trim replace contains indexOf
  split computeCreateAddress computeCreate2Address`.
- randomness (seeded, reproducible; reseed with `setSeed`): `randomUint randomInt
  randomAddress randomBool randomBytes randomBytes4 randomBytes8`.
- `assume`, and the full `vm.assert*` family (`assertEq`, `assertGt`, `assertApproxEqRel`,
  the `*Decimal` forms, 116 overloads) that forge-std's own `assertEq` calls into.
- gas-metering knobs (`pauseGasMetering resumeGasMetering resetGasMetering`) and `skip` are
  accepted as no-ops, since sevm meters gas itself and produces no gas snapshots.

A failed assertion stops the debugger where it broke, with the comparison:

```bash
(sevm) c
Stopped on error: reverted: "assertion failed: 997500000000000000 != 1000000000000000000"
  StdAssertions.assertEq(uint256, uint256) at lib/forge-std/src/StdAssertions.sol:121
 121              vm.assertEq(left, right);
(sevm) up
#1  FailingAssertionTest.testBalanceIgnoresFee() at test/FailingAssertion.t.sol:20
```

Fire one at the prompt against the frame you are stopped in:

```bash
(sevm) vm.warp(12345)
vm.warp -> ok
(sevm) p block.timestamp
$1 = 12345  (uint256)
```

An argument is a literal (`12345`, `5 ether`, `0xcafe`, `true`, `"a string"`) when it reads
as one, and otherwise a Solidity expression evaluated against the paused frame, exactly as
`p` evaluates one:

```bash
(sevm) vm.deal(alice, 5 ether)
vm.deal -> ok
(sevm) p alice.balance
$2 = 5000000000000000000 (5 ether)  (uint256)
(sevm) vm.label(bank.owner(), "owner")
vm.label -> ok
(sevm) vm.getLabel(bank.owner())
vm.getLabel -> owner
```

Only the expression path compiles, so a literal argument costs nothing. Solc's type for an
evaluated argument also picks the overload outright, where a bare literal has to be ranked
against the declared types (`1` fits `bool` as readily as `uint256`). A short hex literal
pads to an address, the way Solidity's own `address(0xcafe)` does, and text solc rejects
falls back to being a plain string, which is how an unquoted word still reaches a `string`
parameter. If that does not fit either, the error names the argument and carries solc's
reason:

```bash
(sevm) vm.prank(alcie)
error: vm.prank: argument 1 (alcie) is not a valid address (Undeclared identifier. Did you mean "alice"? (in
`alcie`))
```

`help cheatcodes` lists the implemented set, generated from the registry so it cannot drift.
`help foundry` covers project resolution and installs.

**Not yet supported:** the expectation and mocking cheats (`expectRevert`, `expectEmit`,
`expectCall`, `mockCall`), `ffi`, state snapshots (`snapshotState`/`revertToState`), forking
and RPC (`createFork`, `rollFork`, `rpc`), filesystem and JSON/TOML cheats (`readFile`,
`writeFile`, `parseJson`, `serialize*`), broadcast/script cheats, and fuzz or invariant
argument generation. An unimplemented selector reverts the calling contract with a clear
`unimplemented cheatcode` message rather than passing silently.
