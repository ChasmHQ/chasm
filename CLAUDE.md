# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Chasm is an all-in-one development and security tool for smart contracts, combining features similar to Remix, Postman, and Foundry. It provides a unified interface for blockchain developers and security researchers to interact with, debug, and audit contracts.

**Architecture:** The project consists of two main components:
- **Backend (Rust)**: Axum-based web server that handles contract compilation, blockchain interaction, and Foundry integration
- **Frontend (React + Vite)**: Interactive UI for contract workspace, transaction traces, storage inspection, and blockchain exploration

## Build Commands

### Installation for Users
Users can install via the installer script:
```bash
curl -fsSL https://raw.githubusercontent.com/ChasmHQ/chasm/main/install.sh | bash
```

### Build from Source (Development)
```bash
# Install UI dependencies and build assets
npm --prefix ui install
npm --prefix ui run build

# Build the Rust binary
cargo build --release
```

### Development
```bash
# Build and run in development mode
cargo run -- ./contracts

# Run the binary from a specific directory
./target/release/chasm <path-to-solidity-contracts>

# UI development (with hot reload)
cd ui && npm run dev

# Lint the UI
cd ui && npm run lint
```

### Production
```bash
# Release build
cargo build --release

# The binary will be at: ./target/release/chasm
```

## Prerequisites

The following tools must be installed on the system:
- **Rust**: For building the backend
- **Foundry** (`anvil`, `cast`, `forge`): Required for local simulation, tracing, and storage inspection
- **Node.js**: For building UI assets

## Architecture

### Backend Architecture (Rust + Axum)

**Entry Point:** `src/main.rs`
- Starts two Anvil nodes: primary (port 8545) and forked (port 8546)
- Initializes file watcher for auto-recompilation
- Serves WebSocket endpoint for real-time compilation updates
- Serves REST API endpoints for blockchain interaction
- Serves static UI from embedded `ui/dist` directory

**Core Modules:**
- `src/compiler.rs`: Wraps `foundry-compilers` to recursively compile Solidity files from the contracts directory or project root
- `src/watcher.rs`: File system watcher using `notify` crate that triggers recompilation on `.sol` file changes
- `src/anvil.rs`: Manages Anvil node lifecycle (start/stop/fork)

**Key API Endpoints:**
- `GET /ws`: WebSocket for live compilation updates
- `GET /inspect/:contract`: Storage layout inspection via `forge inspect`
- `GET /trace/:tx_hash`: Transaction trace via `cast run`
- `POST /trace/calltree`: Call tree trace via `cast run --trace`
- `POST /trace/call`: Call trace simulation
- `POST /fork/start`: Start forked Anvil node
- `POST /fork/stop`: Stop forked Anvil node
- `GET /fork/status`: Get fork node status
- `GET /keystores`: List available keystores in `~/.chasm/keystores`
- `POST /keystores/unlock`: Unlock keystore with password
- `POST /keystores/create`: Create new keystore
- `POST /keystores/remove`: Remove keystore

**Compilation Flow:**
1. `Compiler::new()` detects `contracts/` directory or falls back to root
2. Uses `foundry-compilers` with ephemeral project (no artifacts written to disk)
3. Creates temporary cache and artifacts directories to avoid dependency on foundry.toml
4. Returns JSON with contract names and full artifacts (ABI + bytecode)
5. Errors are returned as `{"type": "compile_error", "error": "..."}` JSON

**Note:** Chasm can run on any folder without requiring a foundry.toml file in the target directory. It uses temporary directories for compilation cache and artifacts.

**Anvil Management:**
- Primary node always runs on port 8545
- Fork node (port 8546) can be started/stopped via API with custom RPC URL and block number
- Both nodes are automatically killed on application shutdown

### Frontend Architecture (React + Vite + viem)

**Entry Point:** `ui/src/main.tsx` → `ui/src/App.tsx`

**Core Components:**
- `App.tsx`: Main application state, WebSocket connection, tab management, and RPC client setup
- `RequestTab.tsx`: Contract function call interface with parameter inputs and response display
- `ContractDetailsTab.tsx`: Contract ABI viewer and deployment status
- `BottomPanel.tsx`: Activity log, transaction viewer, and snapshot history
- `Explorer.tsx`: Blockchain explorer for viewing blocks, transactions, and addresses
- `TransactionViewer.tsx`: Detailed transaction trace viewer with colored execution logs
- `SettingsModal.tsx`: RPC configuration and wallet settings
- `UserProfile.tsx`: Wallet management and keystore integration
- `Cheatcodes.tsx`: Foundry cheatcode interface for local testing

**State Management:**
- All state is React hooks-based (no external state library)
- WebSocket connection for real-time compilation updates
- `viem` for all blockchain interactions (wallet, public, test clients)
- Local storage persistence for deployed instances and settings

**Interaction Modes:**
- **Live Mode**: Transactions sent directly to configured RPC URL
- **Local Mode**: Transactions can be reverted using Anvil snapshots via test client

**Tab System:**
- Tabbed interface inspired by Postman for persistent function calls
- Three tab types: `deploy`, `function`, `details`
- Each tab can be linked to a specific deployed contract instance

## Development Patterns

### Adding New API Endpoints

1. Define request/response types as structs with `Serialize`/`Deserialize`
2. Implement async handler function in `src/main.rs`
3. Add route to `Router` in `main()` function
4. Update frontend to call the endpoint (typically in `App.tsx` or relevant component)

### Adding UI Components

1. Create component in `ui/src/components/`
2. Import and use in `App.tsx` or parent component
3. Use `viem` for blockchain interactions
4. Use `lucide-react` for icons
5. Style with Tailwind CSS utility classes

### Working with Foundry Tools

All Foundry commands (`cast`, `forge`, `anvil`) are invoked via Rust's `std::process::Command`:
- Commands inherit current working directory from `state.root_dir`
- Outputs are captured and returned as JSON responses
- ANSI colors from `cast run` are preserved for frontend display

### Keystore Management

Keystores are stored in `~/.chasm/keystores/` as JSON files:
```json
{
  "address": "0x...",
  "crypto": { "cipher": "aes-128-ctr", ... }
}
```
Unlocking requires password and returns decrypted private key for transaction signing.

## Common Development Workflows

### Testing Contract Changes
1. Edit Solidity files in `contracts/` directory
2. File watcher automatically triggers recompilation
3. WebSocket pushes update to frontend
4. Frontend refreshes contract list with new ABIs

### Debugging Transactions
1. Execute transaction from RequestTab
2. View colored execution trace in TransactionViewer
3. Use Storage Inspector to examine state changes
4. Use `cast run` trace endpoint for detailed call trees

### Working with Forks
1. Start fork via `/fork/start` with RPC URL and optional block number
2. Frontend switches to forked node (port 8546)
3. Test interactions against forked state
4. Stop fork to return to local Anvil

## File Structure

```
chasm/
├── src/               # Rust backend
│   ├── main.rs        # HTTP server, API routes, WebSocket
│   ├── compiler.rs    # foundry-compilers wrapper
│   ├── watcher.rs     # File system watcher
│   └── anvil.rs       # Anvil node manager
├── ui/                # React frontend
│   ├── src/
│   │   ├── App.tsx    # Main app component
│   │   ├── components/
│   │   └── types/
│   └── dist/          # Built assets (embedded in binary)
├── contracts/         # Default Solidity contracts directory
├── cache/             # foundry-compilers cache
├── out/               # Foundry build artifacts
└── Cargo.toml         # Binary name is "chasm"
```

## Important Notes

- The application expects Foundry tools to be available in PATH
- UI assets are embedded in the Rust binary at compile time via `include_dir!` macro
- The application listens on `http://127.0.0.1:3000`
- Anvil primary node runs on port 8545, fork node on port 8546
- Contract compilation is completely in-memory (ephemeral, no artifacts written)
- WebSocket connection is required for live compilation updates
- Chasm works on any directory without requiring a foundry.toml file - it uses temporary directories for compilation
- The foundry.toml in the Chasm project root is only for Chasm's own development, not for analyzed projects
