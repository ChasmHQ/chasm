# Chasm uninstaller for Windows
[CmdletBinding()]
param([switch]$WorkerOnly, [switch]$DryRun)
$ErrorActionPreference = 'Stop'
$installDir = Join-Path $env:LOCALAPPDATA 'Chasm'
$binary = Join-Path $installDir 'chasm.exe'
$worker = Join-Path $installDir 'debugger'
if (-not $WorkerOnly) { Write-Host "Binary: $binary" }
Write-Host "Managed worker: $worker"
if ($DryRun) { return }

# Windows PowerShell 5.1 can follow nested junctions during recursive removal.
function Remove-WorkerTree([string]$Path) {
    foreach ($item in Get-ChildItem -LiteralPath $Path -Force) {
        if ($item.PSIsContainer) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { [IO.Directory]::Delete($item.FullName) }
            else { Remove-WorkerTree $item.FullName }
        } else { Remove-Item -LiteralPath $item.FullName -Force }
    }
    [IO.Directory]::Delete($Path)
}
$parentItem = Get-Item -LiteralPath $installDir -Force -ErrorAction SilentlyContinue
if ($parentItem -and ($parentItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Refusing to uninstall through a linked installation directory: $installDir"
}
$workerItem = Get-Item -LiteralPath $worker -Force -ErrorAction SilentlyContinue
if ($workerItem) {
    $markerPath = Join-Path $worker '.chasm-installed'
    $managed = $workerItem.PSIsContainer -and -not ($workerItem.Attributes -band [IO.FileAttributes]::ReparsePoint)
    if ($managed) {
        $marker = Get-Item -LiteralPath $markerPath -Force -ErrorAction SilentlyContinue
        $managed = $marker -and -not $marker.PSIsContainer -and -not ($marker.Attributes -band [IO.FileAttributes]::ReparsePoint) -and
            $marker.Length -gt 0 -and $marker.Length -lt 64 -and (Get-Content -LiteralPath $markerPath -Raw).Trim() -ceq 'chasm-desktop-worker-v1'
    }
    if ($managed) {
        Remove-WorkerTree $worker
        Write-Host 'Removed the worker.'
    } elseif ($WorkerOnly) { throw "Not an installer-managed worker: $worker" }
    else { Write-Warning "Left unrecognized directory or link untouched: $worker" }
}
if ($WorkerOnly) { return }

$binaryItem = Get-Item -LiteralPath $binary -Force -ErrorAction SilentlyContinue
if ($binaryItem) {
    if ($binaryItem.PSIsContainer -or ($binaryItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Refusing to remove non-regular binary: $binary"
    }
    Remove-Item -LiteralPath $binary -Force
}
if ($parentItem -and -not (Get-ChildItem -LiteralPath $installDir -Force)) { [IO.Directory]::Delete($installDir) }
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -contains $installDir) {
    $newPath = (($userPath -split ';') | Where-Object { $_ -ne $installDir }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host 'Removed Chasm from user PATH.'
}
Write-Host 'Workspace data and wallets were not touched.'
