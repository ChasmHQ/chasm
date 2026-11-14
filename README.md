# Chasm

Chasm is an all-in-one development and security tool for smart contracts. It is designed to bridge the gap between writing code and analyzing it, providing a unified interface that combines the best features of Remix, Postman, and Foundry.

> **Note:** This project is still in the prototyping phase, so any feedback is welcome. It might contain a lot of bugs.

Just as Burp Suite is essential for web security and Postman for API testing, Chasm provides a workspace for blockchain developers and security researchers to interact with, debug, and audit contracts in a structure-agnostic environment.

## Native desktop workspace

The Rust/Slint desktop has a dockable workspace, independent native floating panes, a shared debugger command console and owner-bound contract tabs with Simple, Raw JSON and on-demand local Trace views. Bank's Storage stays inside its contract document. Read [NATIVE_WORKSPACE.md](NATIVE_WORKSPACE.md) for panel controls, local-only boundaries and capability limits, and [NATIVE_PARITY.md](NATIVE_PARITY.md) for work still in progress. The feature list and older screenshots below also describe the legacy web workspace; they are not a complete native-parity claim.

## Screenshots
![Contract workspace](screenshots/Screenshot%202026-01-20%20at%2023.26.49.png)
![Transaction trace](screenshots/Screenshot%202026-01-20%20at%2023.27.01.png)
![Storage inspector](screenshots/Screenshot%202026-01-20%20at%2023.27.36.png)
![Blockchain explorer](screenshots/Screenshot%202026-01-20%20at%2023.28.01.png)

## Features

### Smart Contract Workspace
- **Zero-Config Compilation**: Chasm recursively scans the current directory for any Solidity files and recompiles them automatically using `foundry-compilers`.
- **Persistent Interaction**: A tabbed, Postman-style interface allows you to keep multiple function calls open simultaneously. Inputs and responses persist even when switching tabs.
- **Advanced Parameter Control**: Support for constructor arguments, ETH value sending (with Wei/Gwei/ETH unit conversion), and manual gas limit overrides.
- **Raw RPC Editor**: Toggle between a standard form view and a raw JSON-RPC request editor with syntax highlighting for low-level debugging.

### Debugging & Security
- **Visual Transaction Traces**: View detailed, colored execution traces for every transaction, similar to Foundry's `-vvvv` output.
- **Storage Inspector**: Read any storage slot directly from the UI, including private variables, mappings, and long strings.
- **Foundry Integration**: Built-in support for local simulation using Foundry cheatcodes, embedded Anvil node, easy transaction state revert, and more.

### Interactive Web Debugger
- **Local REVM Sessions**: Open raw or source-mapped offline replay in the Debugger activity, with stack, memory, storage, frames, gas, breakpoints and stepping.
- **Paused-State Experiments**: Evaluate supported Solidity expressions, discard or keep side effects, and apply explicit local mutations without signing or broadcasting.
- **Separate Baseline and Outcome**: Preserve receipt comparisons while marking counterfactual state changes. Historical support remains Ethereum L1 Cancun, not arbitrary current-mainnet replay.

This development integration requires the sibling Python/Rust debugger checkout. See [WEB_DEBUGGER.md](WEB_DEBUGGER.md) for full startup commands, examples, safety boundaries and remaining limitations.

### Blockchain Explorer
- **Universal Search**: Search for wallet addresses, contract addresses, transaction hashes, or block numbers.
- **Network Dashboard**: Real-time view of latest blocks and transactions on the connected network.

## Installation

### Quick Install (macOS ARM64)

**Prerequisites:**
- **Foundry**: Required for simulation and tracing tools (`anvil`, `cast`, `forge`). Install from [getfoundry.sh](https://getfoundry.sh)

**Install:**
```bash
curl -fsSL https://raw.githubusercontent.com/ChasmHQ/chasm/main/install.sh | bash
```

Or manually:
```bash
wget https://raw.githubusercontent.com/ChasmHQ/chasm/main/install.sh
chmod +x install.sh
./install.sh
```

**Uninstall:**
```bash
curl -fsSL https://raw.githubusercontent.com/ChasmHQ/chasm/main/uninstall.sh | bash
```

### Quick Install (Windows)

**Prerequisites:**
- **Foundry**: Required for simulation and tracing tools (`anvil`, `cast`, `forge`). Install from [getfoundry.sh](https://getfoundry.sh)

**Install:**
Run the following command in PowerShell:
```powershell
iwr -useb https://raw.githubusercontent.com/ChasmHQ/chasm/main/install.ps1 | iex
```

**Uninstall:**
Run the following command in PowerShell:
```powershell
iwr -useb https://raw.githubusercontent.com/ChasmHQ/chasm/main/uninstall.ps1 | iex
```

### Build from Source

**Prerequisites:**
- **Rust**: Required to build the backend.
- **Foundry**: Required for the underlying simulation and tracing tools (`anvil`, `cast`, `forge`).
- **Node.js**: Required to build the UI assets once.

**Steps:**
1. Clone the repository:
   ```bash
   git clone https://github.com/ChasmHQ/chasm
   cd chasm
   ```
2. Build the UI and binary:
   - **macOS / Linux:**
     ```bash
     ./build.sh
     ```
   - **Windows (PowerShell):**
     ```powershell
     ./build.ps1
     ```
3. Run Chasm from any Solidity project directory:
   ```bash
   ./target/release/chasm ./contracts
   ```
4. Open the web UI at http://localhost:3000

## Usage
Launch Chasm in any folder containing Solidity files by running `chasm .`. The tool will automatically detect your contracts, allowing you to deploy them to a local node or attach to existing addresses on any network. Use the activity bar to switch between the Contract Workspace and the Blockchain Explorer.

---
Built for the Ethereum development and security community.

## Agent skill

The repository's canonical [Chasm bug bounty skill](.agents/skills/chasm-bugbounty/README.md) includes offline validation and personal installation/update guidance. Its application documentation is a dated snapshot; using the skill does not grant testing authorization.

## Native desktop checkpoint

The current Rust/Slint desktop opens a Welcome screen without starting project services, then uses an owned silent local node when a project is opened. The native Debugger now has Code, Inspector, Inspection and Evaluation views, backed by the existing isolated gateway/REVM worker. Its bundled WorkshopVault preset is synthetic offline data; Solidity evaluation discards effects and no broadcast is involved. The complete browser launch forms and remote debugger commands are not yet ported to the native control API.

See [UI branch review](UI_BRANCH_REVIEW.md) for the fetched upstream revisions and selective-port decisions, [native parity status](NATIVE_PARITY.md) for delivered versus planned capabilities, and [API/MCP documentation](AI_CONTROL.md) for local pairing and the current action catalog. The historical screenshots and web feature list above are not a claim of complete native parity.

## Next development
We will focusing on dynamic analysis to help auditor for debugging the smart contract, like tenderly simulations. It will been show all the stack traces, the reason why it failed. Also will been adding Fund Flow, and Gas Profiler for more clarity in transaction flow. If can will be re-making the revoke feature. 
