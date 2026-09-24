# Exercise the installed Windows wheel's native WezTerm GUI on a hosted runner.
# Reused-GUI RPC behavior is covered by the pinned fork's native Windows tests.
param([Parameter(Mandatory = $true)][string]$ScriptsDir, [int]$TimeoutSeconds = 45)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Kitty Windows smoke requires native Windows' }
$bundle = Join-Path (Split-Path $ScriptsDir -Parent) 'clud-kittyterm'
$wezterm = Join-Path $bundle 'wezterm.exe'
$gui = Join-Path $bundle 'wezterm-gui.exe'
$config = Join-Path $bundle 'clud-kittyterm.lua'
$clud = Join-Path $ScriptsDir 'clud.exe'
foreach ($file in @($wezterm, $gui, $config, $clud)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Installed Kitty bundle is missing $file" }
}
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
$marker = Join-Path $env:RUNNER_TEMP "clud-kitty-smoke-$([Guid]::NewGuid().ToString('N')).txt"
$child = 'Set-Content -LiteralPath $env:CLUD_KITTY_SMOKE_MARKER -Value "kitty=$env:CLUD_KITTY_TERM pane=$env:WEZTERM_PANE"; exit 37'
$start = [Diagnostics.ProcessStartInfo]::new($gui)
$start.UseShellExecute = $false
$start.WorkingDirectory = $ScriptsDir
$start.Environment['CLUD_KITTY_SMOKE_MARKER'] = $marker
$start.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'
foreach ($arg in @('--config-file', $config, 'start', '--always-new-process', '--no-auto-connect',
                   '--return-initial-exit-code', '--cwd', $ScriptsDir, '--', 'pwsh.exe',
                   '-NoProfile', '-NonInteractive', '-Command', $child)) {
    [void]$start.ArgumentList.Add($arg)
}
$process = [Diagnostics.Process]::Start($start)
if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
    $process.Kill($true)
    throw "GUI_UNAVAILABLE: WezTerm did not exit within $TimeoutSeconds seconds"
}
if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
    throw "GUI_UNAVAILABLE: WezTerm exited $($process.ExitCode) without spawning its child"
}
$observed = (Get-Content -LiteralPath $marker -Raw).Trim()
if ($observed -notmatch '^kitty=1 pane=\d+$') { throw "WezTerm child lacked Kitty environment: $observed" }
if ($process.ExitCode -ne 37) { throw "WezTerm failed to propagate child exit 37: exit=$($process.ExitCode)" }
$process.Dispose()
Write-Host "Native installed Kitty GUI child ran with $observed and exit 37 propagated"
