# Chasm Uninstaller for Windows

$ErrorActionPreference = "Stop"
$INSTALL_DIR = "$env:LOCALAPPDATA\Chasm"

Write-Host "Chasm Uninstaller"
Write-Host "================="
Write-Host ""

# Check if directory exists
if (-not (Test-Path $INSTALL_DIR)) {
    Write-Warning "Chasm installation directory not found at $INSTALL_DIR"
} else {
    Write-Host "Removing installation directory ($INSTALL_DIR)..."
    try {
        Remove-Item -Path $INSTALL_DIR -Recurse -Force -ErrorAction Stop
        Write-Host "Files removed."
    } catch {
        Write-Error "Failed to remove files: $_"
        Write-Host "Please ensure Chasm is not running and try again."
        exit 1
    }
}

# Cleanup PATH
Write-Host "Cleaning up PATH environment variable..."
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -like "*$INSTALL_DIR*") {
    # Remove the specific entry from PATH string
    # We split, filter, and join to be safe
    $PathParts = $UserPath -split ";"
    $NewPathParts = $PathParts | Where-Object { $_ -ne $INSTALL_DIR }
    $NewPath = $NewPathParts -join ";"
    
    if ($NewPath -ne $UserPath) {
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host "Removed Chasm from User PATH."
    }
} else {
    Write-Host "Chasm was not found in User PATH."
}

Write-Host ""
Write-Host "✓ Chasm uninstalled successfully!" -ForegroundColor Green
Write-Host "Note: Foundry keystores (~/.foundry/keystores) were NOT deleted as they may be used by other tools."
