# Exercise the installed Windows wheel's native WezTerm GUI on a hosted runner.
# This verifies process startup, child spawning, config environment, and exit
# propagation. It cannot establish visual rendering or physical key behavior.
# The second probe below also exercises the installed clud --kitty-term path.
param(
    [Parameter(Mandatory = $true)][string]$ScriptsDir,
    [int]$TimeoutSeconds = 45
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Kitty Windows smoke requires native Windows' }
if ($TimeoutSeconds -lt 1) { throw 'TimeoutSeconds must be positive' }

$bundle = Join-Path (Split-Path $ScriptsDir -Parent) 'clud-kittyterm'
$wezterm = Join-Path $bundle 'wezterm.exe'
$gui = Join-Path $bundle 'wezterm-gui.exe'
$config = Join-Path $bundle 'clud-kittyterm.lua'
$clud = Join-Path $ScriptsDir 'clud.exe'
foreach ($file in @($wezterm, $gui, $config, $clud)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "Installed Kitty bundle is missing $file"
    }
}

$self = [Diagnostics.Process]::GetCurrentProcess()
Write-Host "Windows session=$($self.SessionId) userInteractive=$([Environment]::UserInteractive)"
Write-Host "Explorer running=$([bool](Get-Process explorer -ErrorAction SilentlyContinue))"
try {
    $display = Get-CimInstance Win32_VideoController -ErrorAction Stop |
        Select-Object -ExpandProperty Name
    Write-Host "Display adapters=$($display -join ', ')"
} catch {
    Write-Host "Display adapter query unavailable: $_"
}

# wezterm.exe is a console application. wezterm-gui.exe has no console output
# on Windows, so inspect flags and parse the real config through this binary.
$helpText = (& $wezterm start --help 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0 -or -not $helpText.Contains('--return-initial-exit-code')) {
    throw "Installed WezTerm lacks required exit flag: exit=$LASTEXITCODE help=$helpText"
}
$keys = (& $wezterm --config-file $config show-keys 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($keys)) {
    throw "Installed Kitty config failed to load: exit=$LASTEXITCODE output=$keys"
}
Write-Host 'WezTerm CLI and Kitty config loaded successfully'

if (-not $env:RUNNER_TEMP) { $env:RUNNER_TEMP = [IO.Path]::GetTempPath() }
$probeDir = Join-Path $env:RUNNER_TEMP "clud-kitty-backend-$([Guid]::NewGuid().ToString('N'))"
[void][IO.Directory]::CreateDirectory($probeDir)
$marker = Join-Path $probeDir 'seed.txt'
$backendMarker = Join-Path $probeDir 'backend.txt'
$child = 'Set-Content -LiteralPath $env:CLUD_KITTY_SMOKE_MARKER -Value "kitty=$env:CLUD_KITTY_TERM pane=$env:WEZTERM_PANE socket=$env:WEZTERM_UNIX_SOCKET"; while (-not (Test-Path -LiteralPath $env:CLUD_KITTY_SMOKE_RELEASE)) { Start-Sleep -Milliseconds 100 }; exit 23'
$start = [Diagnostics.ProcessStartInfo]::new($gui)
$start.UseShellExecute = $false
$start.WorkingDirectory = $ScriptsDir
$start.Environment['CLUD_KITTY_SMOKE_MARKER'] = $marker
$start.Environment['CLUD_KITTY_SMOKE_RELEASE'] = $backendMarker
$start.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'
foreach ($arg in @('--config-file', $config, 'start', '--no-auto-connect',
                   '--return-initial-exit-code', '--cwd',
                   $ScriptsDir, '--', 'pwsh.exe', '-NoProfile', '-NonInteractive',
                   '-Command', $child)) {
    [void]$start.ArgumentList.Add($arg)
}

$process = $null
try {
    $process = [Diagnostics.Process]::Start($start)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not (Test-Path -LiteralPath $marker -PathType Leaf) -and
           -not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
        $children = @()
        try {
            $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($process.Id)" |
                ForEach-Object { "$($_.Name):$($_.ProcessId)" })
        } catch {
            $children = @("unavailable: $($_.Exception.Message)")
        }
        throw "GUI_UNAVAILABLE: WezTerm failed to spawn its child within $TimeoutSeconds seconds; marker=absent children=$($children -join ',') exited=$($process.HasExited) session=$($self.SessionId) interactive=$([Environment]::UserInteractive)"
    }
    $observed = (Get-Content -LiteralPath $marker -Raw).Trim()
    if ($observed -notmatch '^kitty=1 pane=\d+ socket=(.+)$') {
        throw "WezTerm child lacked Kitty config/pane environment: $observed"
    }
    $serverSocket = $Matches[1]
    if ($process.HasExited) {
        throw "GUI_UNAVAILABLE: seed GUI exited before the reuse probe: exit=$($process.ExitCode)"
    }
    Write-Host "Native Kitty GUI child is live with $observed"
} catch {
    if ($process) { $process.Dispose() }
    throw
}

# Route a real installed clud invocation through its packaged launcher. A
# private claude.cmd on PATH is deterministic and needs no account or network.
# The version branch satisfies clud's Claude Code version probe.
$backend = Join-Path $probeDir 'claude.cmd'
$batch = @'
@echo off
if /I "%~1"=="--version" (
  echo 9.9.9 (mock-agent)
  exit /b 0
)
echo kitty=%CLUD_KITTY_TERM% pane=%WEZTERM_PANE% socket=%WEZTERM_UNIX_SOCKET% args=%* >"%CLUD_KITTY_SMOKE_MARKER%"
exit /b 37
'@
[IO.File]::WriteAllText($backend, $batch, [Text.Encoding]::ASCII)
$outer = [Diagnostics.ProcessStartInfo]::new($clud)
$outer.UseShellExecute = $false
$outer.WorkingDirectory = $ScriptsDir
$outer.Environment['PATH'] = "$probeDir;$($outer.Environment['PATH'])"
$outer.Environment['CLUD_KITTY_SMOKE_MARKER'] = $backendMarker
$outer.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'
$outer.Environment['CLUD_NO_UNLOCK'] = '1'
foreach ($arg in @('--kitty-term', '--claude', '--subprocess', '-p', 'kitty-smoke')) {
    [void]$outer.ArgumentList.Add($arg)
}

$outerProcess = $null
try {
    $outerProcess = [Diagnostics.Process]::Start($outer)
    if (-not $outerProcess.WaitForExit($TimeoutSeconds * 1000)) {
        $backendState = 'absent'
        if (Test-Path -LiteralPath $backendMarker -PathType Leaf) {
            $backendState = (Get-Content -LiteralPath $backendMarker -Raw).Trim()
        }
        $seedState = "pid=$($process.Id) exited=$($process.HasExited)"
        $outerProcess.Kill($true)
        throw "CLUD_KITTY_LAUNCH_TIMEOUT: installed clud did not exit within $TimeoutSeconds seconds; backend=$backendState seed=$seedState"
    }
    if (-not (Test-Path -LiteralPath $backendMarker -PathType Leaf)) {
        throw "CLUD_KITTY_LAUNCH_FAILED: backend did not run; outer exit=$($outerProcess.ExitCode)"
    }
    $backendResult = (Get-Content -LiteralPath $backendMarker -Raw).Trim()
    if ($backendResult -notmatch '^kitty=1 pane=\d+ socket=(\S+) args=.*kitty-smoke') {
        throw "Installed clud did not forward into a Kitty pane: $backendResult"
    }
    if ($Matches[1] -ne $serverSocket) {
        throw "Installed clud started another GUI instead of reusing the live GUI: seed=$serverSocket probe=$($Matches[1])"
    }
    if ($outerProcess.ExitCode -ne 37) {
        throw "Installed clud failed to propagate backend exit 37: exit=$($outerProcess.ExitCode)"
    }
    Write-Host "Installed clud --kitty-term reused the live GUI: $backendResult; exit 37 propagated"
    $seedDeadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $process.HasExited -and [DateTime]::UtcNow -lt $seedDeadline) {
        Start-Sleep -Milliseconds 100
    }
    if (-not $process.HasExited) {
        throw "GUI_UNAVAILABLE: seed GUI stayed open after reused pane completed"
    }
    if ($process.ExitCode -ne 23) {
        throw "WezTerm failed to propagate first GUI child status 23: status=$($process.ExitCode)"
    }
    Write-Host 'Independent first GUI child status 23 propagated after reused pane status 37'
} finally {
    if ($outerProcess) { Write-Verbose "Outer process $($outerProcess.Id) completed" }
    Write-Verbose "Probe directory: $probeDir"
}
