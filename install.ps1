# One-line installer for clud — the fast Rust CLI for Claude Code and Codex.
#
#   irm https://raw.githubusercontent.com/zackees/clud/main/install.ps1 | iex
#
# What this does:
#   1. Ensures `uv` (https://docs.astral.sh/uv) is installed (idempotent;
#      bootstraps it via Astral's official one-line installer if missing).
#   2. Runs `uv tool install clud[==VERSION]`, which drops `clud.exe` and
#      helper executables such as `clud-block-bad-cmd.exe` into uv's tool bin
#      directory.
#   3. Adds uv's tool bin directory to the User PATH (HKCU\Environment)
#      so new terminals find `clud` without manual intervention.
#   4. Prints the exact `Set-Item Env:PATH` line for the current session.
#
# Environment:
#   $env:CLUD_VERSION   Pin a specific clud version (e.g. "2.0.10"). Unset → latest.
#   $env:CLUD_NO_PATH   Set to "1" to skip the User PATH mutation.
#
# Re-running this script upgrades or reinstalls cleanly
# (`uv tool install --force`).
#
# This is the END-USER installer for clud. The repo's `./install` script
# (no extension) is a DEVELOPER helper that installs `soldr` for building
# clud from source — unrelated.

#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

function Write-Info($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Write-Warn($msg) { Write-Host "warn: $msg" -ForegroundColor Yellow }
function Write-ErrLine($msg) { Write-Host "error: $msg" -ForegroundColor Red }

function Test-Command($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

function Ensure-Uv {
    if (Test-Command 'uv') { return }
    Write-Info "uv not found — installing via https://astral.sh/uv/install.ps1"
    # Astral's official one-line PowerShell installer. It lands uv.exe in
    # %USERPROFILE%\.local\bin and adds that dir to the User PATH for new
    # shells; we surface it on PATH for the CURRENT session below.
    Invoke-RestMethod -Uri 'https://astral.sh/uv/install.ps1' | Invoke-Expression

    foreach ($candidate in @(
            (Join-Path $env:USERPROFILE '.local\bin'),
            (Join-Path $env:USERPROFILE '.cargo\bin'))) {
        if ((Test-Path $candidate) -and -not ($env:PATH -split ';' -contains $candidate)) {
            $env:PATH = "$candidate;$env:PATH"
        }
    }
    if (-not (Test-Command 'uv')) {
        Write-ErrLine "uv install did not put uv.exe on PATH — open a new terminal and re-run, or install uv manually from https://docs.astral.sh/uv"
        exit 1
    }
}

function Install-Clud {
    $spec = 'clud'
    if ($env:CLUD_VERSION) {
        $spec = "clud==$($env:CLUD_VERSION)"
    }
    Write-Info "installing $spec via uv tool"
    # --force replaces any prior install cleanly so re-running this
    # script upgrades or repairs without manual `uv tool uninstall`.
    & uv tool install --force $spec
    if ($LASTEXITCODE -ne 0) {
        Write-ErrLine "uv tool install failed with exit code $LASTEXITCODE"
        exit $LASTEXITCODE
    }
}

function Verify-CludInstall {
    $toolBin = Get-UvToolBinDir
    if (-not $toolBin) {
        Write-ErrLine "could not determine uv tool bin dir after install"
        exit 1
    }
    $cludExe = Join-Path $toolBin 'clud.exe'
    $guardExe = Join-Path $toolBin 'clud-block-bad-cmd.exe'
    if (-not (Test-Path $cludExe)) {
        Write-ErrLine "installed clud is missing at $cludExe"
        exit 1
    }
    if (-not (Test-Path $guardExe)) {
        Write-ErrLine "installed clud is missing native helper at $guardExe; re-run this installer"
        exit 1
    }

    $denyCommand = 'bad'
    $denyCommand = "$denyCommand cmd"
    $denyPayload = @{ tool_name = 'Bash'; tool_input = @{ command = $denyCommand } } | ConvertTo-Json -Compress
    $denyOutput = $denyPayload | & $guardExe 2>$null
    $denyCode = $LASTEXITCODE
    if ($denyCode -ne 2 -or ($denyOutput -join "`n") -notmatch 'permissionDecision' -or ($denyOutput -join "`n") -notmatch 'deny') {
        Write-ErrLine "native clud-block-bad-cmd deny smoke failed (exit $denyCode)"
        exit 1
    }

    $allowPayload = @{ tool_name = 'Bash'; tool_input = @{ command = 'echo ok' } } | ConvertTo-Json -Compress
    $null = $allowPayload | & $guardExe 2>$null
    $allowCode = $LASTEXITCODE
    if ($allowCode -ne 0) {
        Write-ErrLine "native clud-block-bad-cmd allow smoke failed (exit $allowCode)"
        exit 1
    }
}

function Get-UvToolBinDir {
    try {
        $dir = (& uv tool dir --bin 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -eq 0 -and $dir) { return $dir }
    } catch {}
    return $null
}

function Add-UserPath($dir) {
    # Idempotent User PATH mutation via the registry to avoid duplicate
    # entries that setx + repeated runs would otherwise pile up.
    $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    if (-not $userPath) { $userPath = '' }
    $entries = $userPath -split ';' | Where-Object { $_ -ne '' }
    if ($entries -contains $dir) { return $false }
    $newPath = if ($userPath) { "$userPath;$dir" } else { $dir }
    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    return $true
}

function Setup-Path {
    $toolBin = Get-UvToolBinDir
    if (-not $toolBin) {
        Write-Warn "could not determine uv tool bin dir — skipping PATH setup"
        return
    }
    if ($env:CLUD_NO_PATH -eq '1') {
        Write-Info "CLUD_NO_PATH=1 — skipping User PATH update"
        Write-Host ""
        Write-Host "clud installed at: $toolBin\clud.exe"
        Write-Host "Add this directory to your current shell with:"
        Write-Host "  `$env:PATH = `"$toolBin;`$env:PATH`""
        return
    }
    $added = Add-UserPath $toolBin
    # Surface on PATH for the CURRENT session too, since registry-level
    # changes don't apply to the running process.
    if (-not ($env:PATH -split ';' -contains $toolBin)) {
        $env:PATH = "$toolBin;$env:PATH"
    }
    Write-Host ""
    if ($added) {
        Write-Info "added $toolBin to User PATH — new terminals will find clud automatically"
    } else {
        Write-Info "$toolBin already on User PATH"
    }
    Write-Host "For the current session run:"
    Write-Host "  `$env:PATH = `"$toolBin;`$env:PATH`""
    Write-Host "Then verify with:  clud --version"
}

function Main {
    Ensure-Uv
    Install-Clud
    Verify-CludInstall
    Setup-Path
}

Main
