<#
.SYNOPSIS
Download the published Chasm binary, or install the native app from this checkout.
.EXAMPLE
.\install.ps1 -WithDebugger
Install target\release\chasm.exe and its local worker. Requires uv, Rust 1.91+
and MSVC build tools. uv uses Python 3.12 and the checkout's dependency locks.
.EXAMPLE
.\install.ps1 -NativeOnly -DryRun
Preview a checkout install without the worker. With no mode, the installer downloads the published binary.
#>
[CmdletBinding()]
param(
    [switch]$WithDebugger,
    [switch]$NativeOnly,
    [switch]$DryRun,
    [switch]$NoPath
)
$ErrorActionPreference = 'Stop'
if ($WithDebugger -and $NativeOnly) { throw 'Choose either -WithDebugger or -NativeOnly.' }
$installDir = Join-Path $env:LOCALAPPDATA 'Chasm'
$destination = Join-Path $installDir 'chasm.exe'
$worker = Join-Path $installDir 'debugger'
$checkout = $WithDebugger -or $NativeOnly
if ($checkout) {
    if (-not $PSScriptRoot) { throw 'Run install.ps1 from a complete checkout for -WithDebugger or -NativeOnly.' }
    $sourceBinary = Join-Path $PSScriptRoot 'target\release\chasm.exe'
    $sourceWorker = Join-Path $PSScriptRoot 'debugger'
} else {
    $sourceBinary = 'https://github.com/ChasmHQ/chasm/releases/download/pre-release/chasm.exe'
}
$assets = @('examples\mainnet\synthetic-replay.json', 'examples\web-debugger\source-replay.json',
    'examples\web-debugger\build-info.json', 'examples\web-debugger\WorkshopVault.sol')
Write-Host "Binary: $sourceBinary -> $destination"
if ($WithDebugger) {
    Write-Host "Worker: $sourceWorker -> $worker (private venv; dependencies may be downloaded)"
} else {
    Write-Host 'No debugger will be installed or updated.'
}
if ($DryRun) { return }

if ($checkout -and -not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
    throw 'No matching release executable. Run cargo build --release --locked in this checkout first.'
}
if ($WithDebugger) {
    if (Get-Item -LiteralPath $worker -Force -ErrorAction SilentlyContinue) {
        throw "Refusing to replace $worker. Run .\uninstall.ps1 -WorkerOnly for an installer-managed worker, or move the existing directory yourself."
    }
    foreach ($file in (@('uv.lock', 'Cargo.lock', 'pyproject.toml', 'rust\python\Cargo.toml') + $assets)) {
        if (-not (Test-Path -LiteralPath (Join-Path $sourceWorker $file) -PathType Leaf)) { throw "Missing checkout file: debugger\$file" }
    }
    foreach ($tool in 'uv', 'cargo') {
        if (-not (Get-Command $tool -CommandType Application -ErrorAction SilentlyContinue)) {
            throw 'Install uv and Rust 1.91+ (including the MSVC build tools) before setting up the worker.'
        }
    }
}
if (-not (Get-Command forge -ErrorAction SilentlyContinue)) {
    Write-Warning 'Chasm requires Foundry (anvil, cast, forge): https://getfoundry.sh'
    if ((Read-Host 'Continue installation anyway? (y/N)') -notmatch '^[Yy]$') { throw 'Installation cancelled.' }
}

$parentItem = Get-Item -LiteralPath $installDir -Force -ErrorAction SilentlyContinue
if ($parentItem -and ($parentItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Refusing to install through a linked directory: $installDir"
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$destItem = Get-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
if ($destItem -and ($destItem.PSIsContainer -or ($destItem.Attributes -band [IO.FileAttributes]::ReparsePoint))) {
    throw "Refusing to overwrite non-regular destination: $destination"
}
$tmp = Join-Path $installDir ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $staged = Join-Path $tmp 'chasm.exe'
    if ($checkout) { Copy-Item -LiteralPath $sourceBinary -Destination $staged }
    else { Invoke-WebRequest -UseBasicParsing -Uri $sourceBinary -OutFile $staged }

    if ($WithDebugger) {
        # A venv's entry points contain absolute paths. Build it at its final location.
        New-Item -ItemType Directory -Path $worker | Out-Null
        Set-Content -LiteralPath (Join-Path $worker '.chasm-installed') -Value 'chasm-desktop-worker-v1' -Encoding Ascii
        Write-Host 'If setup fails, run .\uninstall.ps1 -WorkerOnly before retrying.'
        foreach ($asset in $assets) {
            $target = Join-Path $worker $asset
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
            Copy-Item -LiteralPath (Join-Path $sourceWorker $asset) -Destination $target
        }
        $venv = Join-Path $worker '.venv'
        $python = Join-Path $venv 'Scripts\python.exe'
        $oldEnv = $env:UV_PROJECT_ENVIRONMENT
        try {
            $env:UV_PROJECT_ENVIRONMENT = $venv
            & uv sync --project $sourceWorker --python 3.12 --locked --no-dev --no-install-project
            if ($LASTEXITCODE -ne 0) { throw 'Could not install the locked Python dependencies.' }
        } finally { $env:UV_PROJECT_ENVIRONMENT = $oldEnv }
        Push-Location -LiteralPath $sourceWorker
        try {
            # Match the build backend in debugger/uv.lock, outside the runtime venv.
            & uv tool run --from 'maturin==1.15.0' maturin build --release --locked --interpreter $python --out $tmp
            if ($LASTEXITCODE -ne 0) { throw 'Failed to build the REVM worker wheel.' }
        } finally { Pop-Location }
        $wheels = @(Get-ChildItem -LiteralPath $tmp -Filter '*.whl' -File)
        if ($wheels.Count -ne 1) { throw 'Expected one worker wheel from maturin.' }
        & uv pip install --no-index --no-deps --python $python $wheels[0].FullName
        if ($LASTEXITCODE -ne 0) { throw 'Failed to install the local worker wheel.' }
        & $python -I -c 'from chasm_debugger import _revm, web_service'
        if ($LASTEXITCODE -ne 0) { throw 'The installed REVM worker could not be imported.' }
    }
    if ($destItem) { [IO.File]::Replace($staged, $destination, (Join-Path $tmp 'previous.exe')) }
    else { [IO.File]::Move($staged, $destination) }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $NoPath -and ($userPath -split ';') -notcontains $installDir) {
    $newPath = (@($userPath, $installDir) | Where-Object { $_ }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path += ";$installDir"
    Write-Host 'Added Chasm to user PATH. Restart your terminal to use the new entry.'
}
Write-Host "Installed $destination. Run: chasm ."
