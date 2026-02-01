# Chasm Installer for Windows

$ErrorActionPreference = "Stop"

$BINARY_URL = "https://github.com/ChasmHQ/chasm/releases/download/pre-release/chasm.exe"
$INSTALL_DIR = "$env:LOCALAPPDATA\Chasm"
$BINARY_NAME = "chasm.exe"

Write-Host "Chasm Installer"
Write-Host "==============="
Write-Host ""

# Check for Foundry (forge)
if (-not (Get-Command forge -ErrorAction SilentlyContinue)) {
    Write-Warning "Foundry is not installed. Chasm requires Foundry (anvil, cast, forge)."
    Write-Host "Please install Foundry via https://getfoundry.sh or run:"
    Write-Host "curl -L https://foundry.paradigm.xyz | bash"
    Write-Host ""
    $confirmation = Read-Host "Continue installation anyway? (y/N)"
    if ($confirmation -notmatch "^[Yy]$") {
        exit 1
    }
}

# Create Install Directory if not exists
if (-not (Test-Path $INSTALL_DIR)) {
    New-Item -ItemType Directory -Force -Path $INSTALL_DIR | Out-Null
}

$DEST_FILE = Join-Path $INSTALL_DIR $BINARY_NAME

Write-Host "Downloading Chasm binary..."
try {
    Invoke-WebRequest -Uri $BINARY_URL -OutFile $DEST_FILE
} catch {
    Write-Error "Failed to download binary: $_"
    exit 1
}

Write-Host "Installing to $DEST_FILE..."

# Add to PATH if not present
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$INSTALL_DIR*") {
    Write-Host "Adding $INSTALL_DIR to user PATH..."
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$INSTALL_DIR", "User")
    $env:Path += ";$INSTALL_DIR"
    Write-Host "PATH updated. You may need to restart your terminal for changes to take effect."
} else {
    Write-Host "Install directory is already in PATH."
}

# Verify installation
if (Get-Command chasm -ErrorAction SilentlyContinue) {
    Write-Host ""
    Write-Host "✓ Chasm installed successfully!" -ForegroundColor Green
    Write-Host ""
    Write-Host "Usage:"
    Write-Host "  chasm .              # Run in current directory"
    Write-Host "  chasm ./contracts    # Run in specific directory"
    Write-Host ""
    Write-Host "The web UI will be available at http://localhost:3000"
} else {
    Write-Host ""
    Write-Warning "Installation completed but 'chasm' command not found in current session PATH."
    Write-Host "Please restart your terminal (PowerShell/CMD) and try running 'chasm'."
}
