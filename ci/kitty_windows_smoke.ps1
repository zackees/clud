# Exercise the installed Windows wheel's native WezTerm GUI on a hosted runner.
# This verifies process startup, child spawning, config environment, and exit
# propagation. It cannot establish visual rendering or physical key behavior.
# The second probe below also exercises the installed clud --kitty-term path.
param(
    [Parameter(Mandatory = $true)][string]$ScriptsDir,
    [Parameter(Mandatory = $true)][string]$MockAgentPath,
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
if (-not (Test-Path -LiteralPath $MockAgentPath -PathType Leaf)) {
    throw "Mock agent is missing at $MockAgentPath"
}

function Get-BundledGuiPids {
    $found = @()
    foreach ($candidate in (Get-CimInstance Win32_Process -Filter "Name = 'wezterm-gui.exe'" -ErrorAction Stop)) {
        if ($candidate.ExecutablePath -eq $gui) { $found += [int]$candidate.ProcessId }
    }
    return $found
}
$baselineGuiPids = @(Get-BundledGuiPids)

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

function Stop-SmokeProcess {
    param([Diagnostics.Process]$Target, [string]$Name)
    if ($null -eq $Target) { return }
    try {
        if (-not $Target.HasExited) { $Target.Kill($true) }
    } catch {
        Write-Host "Cleanup could not kill $Name process tree: $_"
    }
    try {
        if (-not $Target.HasExited -and -not $Target.WaitForExit(5000)) {
            Write-Host "Cleanup timed out waiting for $Name process tree"
        }
    } catch {
        Write-Host "Cleanup could not wait for $Name process tree: $_"
    } finally {
        try { $Target.Dispose() } catch {
            Write-Host "Cleanup could not dispose process handle: $_"
        }
    }
}

$process = $null
$outerProcess = $null
$controlProcess = $null
try {
$marker = Join-Path $probeDir 'seed.txt'
$backendMarker = Join-Path $probeDir 'backend.txt'
$controlMarker = Join-Path $probeDir 'backend-no-daemon.txt'
$versionMarker = Join-Path $probeDir 'version.txt'
$backend = Join-Path $probeDir 'claude.exe'
[IO.File]::Copy($MockAgentPath, $backend)
$start = [Diagnostics.ProcessStartInfo]::new($clud)
$start.UseShellExecute = $false
$start.WorkingDirectory = $ScriptsDir
$start.Environment['CLUD_KITTY_SMOKE_MARKER'] = $marker
$start.Environment['CLUD_KITTY_SMOKE_RELEASE'] = $backendMarker
$start.Environment['CLUD_KITTY_BACKEND_MARKER'] = $backendMarker
$start.Environment['CLUD_KITTY_BACKEND_CONTROL_MARKER'] = $controlMarker
$start.Environment['CLUD_KITTY_SMOKE_VERSION_MARKER'] = $versionMarker
$start.Environment['CLUD_VERBOSE_LOG_DIR'] = $probeDir
$start.Environment['PATH'] = "$probeDir;$($start.Environment['PATH'])"
$start.Environment['CLUD_NO_UNLOCK'] = '1'
$start.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'
foreach ($arg in @('--kitty-term', '--claude', '--subprocess', '--verbose', '-p',
                   'kitty-seed', '--', '--mock-report-file', $marker,
                   '--mock-sleep-ms', '15000', '--mock-exit-code', '23')) {
    [void]$start.ArgumentList.Add($arg)
}

    $process = [Diagnostics.Process]::Start($start)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $seedGuiPids = @()
    while ($seedGuiPids.Count -eq 0 -and -not $process.HasExited -and
           [DateTime]::UtcNow -lt $deadline) {
        $seedGuiPids = @(Get-BundledGuiPids | Where-Object { $_ -notin $baselineGuiPids })
        Start-Sleep -Milliseconds 100
    }
    if ($seedGuiPids.Count -eq 0) {
        $children = @()
        try {
            $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($process.Id)" |
                ForEach-Object { "$($_.Name):$($_.ProcessId)" })
        } catch {
            $children = @("unavailable: $($_.Exception.Message)")
        }
        throw "GUI_UNAVAILABLE: installed clud failed to seed WezTerm within $TimeoutSeconds seconds; children=$($children -join ',') exited=$($process.HasExited) session=$($self.SessionId) interactive=$([Environment]::UserInteractive)"
    }
    if ($process.HasExited) {
        throw "GUI_UNAVAILABLE: seed GUI exited before the reuse probe: exit=$($process.ExitCode)"
    }
    Write-Host "Native Kitty GUI seeded through installed clud: pids=$($seedGuiPids -join ',')"

# Route a real installed clud invocation through its packaged launcher. A
# private native mock agent on PATH is deterministic and needs no account or network.
$outer = [Diagnostics.ProcessStartInfo]::new($clud)
$outer.UseShellExecute = $false
$outer.WorkingDirectory = $ScriptsDir
$outer.Environment['PATH'] = "$probeDir;$($outer.Environment['PATH'])"
$outer.Environment['CLUD_KITTY_SMOKE_MARKER'] = $backendMarker
$outer.Environment['CLUD_KITTY_BACKEND_MARKER'] = $backendMarker
$outer.Environment['CLUD_KITTY_BACKEND_CONTROL_MARKER'] = $controlMarker
$outer.Environment['CLUD_KITTY_SMOKE_VERSION_MARKER'] = $versionMarker
$outer.Environment['CLUD_VERBOSE_LOG_DIR'] = $probeDir
$outer.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'
$outer.Environment['CLUD_NO_UNLOCK'] = '1'
foreach ($arg in @('--kitty-term', '--claude', '--subprocess', '--verbose', '-p', 'kitty-smoke', '--',
                   '--mock-report-file', $backendMarker, '--mock-exit-code', '37')) {
    [void]$outer.ArgumentList.Add($arg)
}

    $outerProcess = [Diagnostics.Process]::Start($outer)
    if (-not $outerProcess.WaitForExit($TimeoutSeconds * 1000)) {
        $backendState = 'absent'
        try {
            if (Test-Path -LiteralPath $backendMarker -PathType Leaf) {
                $backendState = "present: $((Get-Content -LiteralPath $backendMarker -Raw).Trim())"
            }
        } catch {
            $backendState = "unavailable: $($_.Exception.Message)"
        }
        $guiPids = @()
        $versionState = 'absent'
        try {
            if (Test-Path -LiteralPath $versionMarker -PathType Leaf) {
                $versionState = (Get-Content -LiteralPath $versionMarker -Raw).Trim()
            }
        } catch {
            $versionState = "unavailable: $($_.Exception.Message)"
        }
        try {
            $guiPids = @(Get-BundledGuiPids)
        } catch {
            $guiPids = @("unavailable: $($_.Exception.Message)")
        }
        $outerChildren = @()
        $launchTrace = @()
        try {
            foreach ($log in (Get-ChildItem -LiteralPath $probeDir -Filter 'clud-*.log' -File -ErrorAction Stop)) {
                $launchTrace += "$($log.Name): $((Get-Content -LiteralPath $log.FullName -Tail 25 -ErrorAction Stop) -join ' | ')"
            }
            if ($launchTrace.Count -eq 0) { $launchTrace = @('absent') }
        } catch {
            $launchTrace = @("unavailable: $($_.Exception.Message)")
        }
        try {
            foreach ($candidate in (Get-CimInstance Win32_Process -Filter "ParentProcessId = $($outerProcess.Id)" -ErrorAction Stop)) {
                $outerChildren += "$($candidate.Name):$($candidate.ProcessId)"
            }
        } catch {
            $outerChildren = @("unavailable: $($_.Exception.Message)")
        }
        $seedState = "pid=$($process.Id) exited=$($process.HasExited)"
        # Keep the seed GUI alive for one bounded control run. This is only a
        # differential diagnostic; the default launch still fails the smoke.
        Stop-SmokeProcess $outerProcess 'timed-out clud launcher'
        $outerProcess = $null
        $controlState = 'not-started'
        try {
            $control = [Diagnostics.ProcessStartInfo]::new($clud)
            $control.UseShellExecute = $false
            $control.WorkingDirectory = $ScriptsDir
            foreach ($entry in $outer.Environment.GetEnumerator()) {
                $control.Environment[$entry.Key] = $entry.Value
            }
            $control.Environment['CLUD_KITTY_SMOKE_MARKER'] = $controlMarker
            $control.Environment['CLUD_KITTY_BACKEND_CONTROL_MARKER'] = $controlMarker
            foreach ($arg in @('--kitty-term', '--claude', '--subprocess', '--verbose',
                               '--no-daemon', '-p', 'kitty-smoke-no-daemon')) {
                [void]$control.ArgumentList.Add($arg)
            }
            $controlProcess = [Diagnostics.Process]::Start($control)
            $controlExited = $controlProcess.WaitForExit(15000)
            $controlBackend = 'absent'
            if (Test-Path -LiteralPath $controlMarker -PathType Leaf) {
                $controlBackend = "present: $((Get-Content -LiteralPath $controlMarker -Raw).Trim())"
            }
            $controlExit = if ($controlExited) { "$($controlProcess.ExitCode)" } else { 'running' }
            $controlState = "marker=$controlBackend exit=$controlExit"
        } catch {
            $controlState = "unavailable: $($_.Exception.Message)"
        }
        throw "CLUD_KITTY_LAUNCH_TIMEOUT: installed clud did not exit within $TimeoutSeconds seconds; backend_marker=$backendState version_probe=$versionState seed=$seedState bundled_gui_pids=$($guiPids -join ',') outer_children=$($outerChildren -join ',') launch_trace=$($launchTrace -join ' || ') outer_argv=$($outer.ArgumentList -join ' ') cwd=$ScriptsDir no_daemon_control=$controlState control_caveat=first_attempt_pane_may_remain"
    }
    if (-not (Test-Path -LiteralPath $backendMarker -PathType Leaf)) {
        throw "CLUD_KITTY_LAUNCH_FAILED: backend did not run; outer exit=$($outerProcess.ExitCode)"
    }
    $backendResult = Get-Content -LiteralPath $backendMarker -Raw | ConvertFrom-Json
    if ($backendResult.exit_code -ne 37 -or [string]::IsNullOrWhiteSpace($backendResult.env.WEZTERM_UNIX_SOCKET)) {
        throw "Installed clud did not forward into a Kitty pane: $backendResult"
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
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
        throw 'Seed mock agent did not write its report'
    }
    $seedResult = Get-Content -LiteralPath $marker -Raw | ConvertFrom-Json
    if ($seedResult.exit_code -ne 23 -or
        $seedResult.env.WEZTERM_UNIX_SOCKET -ne $backendResult.env.WEZTERM_UNIX_SOCKET) {
        throw "Installed clud started another GUI instead of reusing the live GUI: seed=$seedResult probe=$backendResult"
    }
    Write-Host 'Independent first GUI child status 23 propagated after reused pane status 37'
} finally {
    # Stop the launcher before the seed GUI, so no new pane can be spawned
    # while the GUI is being torn down. Never replace the probe's real error.
    Stop-SmokeProcess $controlProcess 'no-daemon control launcher'
    Stop-SmokeProcess $outerProcess 'clud launcher'
    Stop-SmokeProcess $process 'seed GUI'
    # A launcher may exit after spawning another GUI. Match the exact wheel
    # executable and only PIDs absent before this probe.
    try {
        foreach ($guiPid in (Get-BundledGuiPids)) {
            if ($guiPid -in $baselineGuiPids) { continue }
            try {
                $newGuiProcess = [Diagnostics.Process]::GetProcessById($guiPid)
                Stop-SmokeProcess $newGuiProcess 'new bundled GUI'
            } catch {
                Write-Host "Cleanup could not stop new bundled GUI process $guiPid`: $_"
            }
        }
    } catch {
        Write-Host "Cleanup could not enumerate new bundled GUI processes: $_"
    }
    try {
        Remove-Item -LiteralPath $probeDir -Recurse -Force -ErrorAction Stop
    } catch {
        Write-Host "Cleanup could not remove probe directory $probeDir`: $_"
    }
}
