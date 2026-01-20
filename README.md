# Chasm

Chasm is an all-in-one development and security tool for smart contracts. It is designed to bridge the gap between writing code and analyzing it, providing a unified interface that combines the best features of Remix, Postman, and Foundry.

> **Note:** This project is still in the prototyping phase, so any feedback is welcome. It might contain a lot of bugs.

Just as Burp Suite is essential for web security and Postman for API testing, Chasm provides a workspace for blockchain developers and security researchers to interact with, debug, and audit contracts in a structure-agnostic environment.

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

### Blockchain Explorer
- **Universal Search**: Search for wallet addresses, contract addresses, transaction hashes, or block numbers.
- **Network Dashboard**: Real-time view of latest blocks and transactions on the connected network.

## Installation

### Prerequisites
- **Rust**: Required to build the backend.
- **Foundry**: Required for the underlying simulation and tracing tools (`anvil`, `cast`, `forge`).
- **Node.js**: Required to build the UI assets once.

### Steps
1. Clone the repository:
   ```bash
   git clone https://github.com/ChasmHQ/chasm
   cd Chasm/chainsmith
   ```
2. Build the UI assets:
   ```bash
   npm --prefix ui install
   npm --prefix ui run build
   ```
3. Build the binary:
   ```bash
   cargo build --release
   ```
4. Run Chasm from any Solidity project directory:
   ```bash
   ./target/release/chasm ./contracts
   ```

5. Open the web on http://localhost:3000

## Usage
Launch Chasm in any folder containing Solidity files by running `chasm .`. The tool will automatically detect your contracts, allowing you to deploy them to a local node or attach to existing addresses on any network. Use the activity bar to switch between the Contract Workspace and the Blockchain Explorer.

---
Built for the Ethereum development and security community.
