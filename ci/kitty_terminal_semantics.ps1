# Native, headless terminal semantics in the GUI created by installed clud.
# This checks protocol/PTY behavior, not rendered pixels or physical input.
param(
    [Parameter(Mandatory = $true)][string]$Wezterm,
    [Parameter(Mandatory = $true)][string]$Socket,
    [Parameter(Mandatory = $true)][int]$SeedPane,
    [Parameter(Mandatory = $true)][string]$MockAgentPath,
    [Parameter(Mandatory = $true)][string]$ProbeDir,
    [Parameter(Mandatory = $true)][string]$ScriptsDir
)

$ErrorActionPreference = 'Stop'
$esc = [char]27
$panes = [Collections.Generic.List[int]]::new()

function Invoke-GuiCli {
    param([string[]]$CliArgs, [int]$TimeoutMs = 5000)
    $start = [Diagnostics.ProcessStartInfo]::new($Wezterm)
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $ScriptsDir
    $start.Environment['WEZTERM_UNIX_SOCKET'] = $Socket
    [void]$start.ArgumentList.Add('cli')
    foreach ($arg in $CliArgs) { [void]$start.ArgumentList.Add([string]$arg) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMs)) {
            try { $process.Kill($true) } catch { }
            [void]$process.WaitForExit(2000)
            throw "WezTerm CLI timed out after $TimeoutMs ms: $($CliArgs -join ' ')"
        }
        if (-not $stdout.Wait(2000) -or -not $stderr.Wait(2000)) {
            throw "WezTerm CLI output did not drain: $($CliArgs -join ' ')"
        }
        if ($process.ExitCode -ne 0) {
            throw "WezTerm CLI exit $($process.ExitCode): $($CliArgs -join ' '): $($stderr.Result)"
        }
        return [string]$stdout.Result
    } finally {
        try { $process.Dispose() } catch { }
    }
}

function Start-ProbePane {
    param([string[]]$AgentArgs, [switch]$Right)
    $command = @('split-pane', '--pane-id', "$SeedPane", '--cwd', $ScriptsDir)
    if ($Right) { $command += '--right' }
    $command += @('--', $MockAgentPath) + $AgentArgs
    $output = (Invoke-GuiCli $command).Trim()
    if ($output -notmatch '^\d+$') { throw "WezTerm did not return a pane ID: $output" }
    $pane = [int]$output
    $panes.Add($pane)
    return $pane
}

function Stop-ProbePane {
    param([int]$Pane)
    [void](Invoke-GuiCli @('kill-pane', '--pane-id', "$Pane") 2000)
    [void]$panes.Remove($Pane)
}

function Wait-ProbeFile {
    param([string]$Path, [int]$TimeoutMs = 6000)
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    while (-not (Test-Path -LiteralPath $Path -PathType Leaf) -and
           [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 50
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Terminal probe marker missing within $TimeoutMs ms: $Path"
    }
}

function Write-Ansi {
    param([string]$Name, [string]$Content)
    $path = Join-Path $ProbeDir $Name
    [IO.File]::WriteAllBytes($path, [Text.Encoding]::ASCII.GetBytes($Content))
    return $path
}

function Get-ProbeReport {
    param([string]$Path)
    Wait-ProbeFile $Path
    $deadline = [DateTime]::UtcNow.AddSeconds(2)
    do {
        try { return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json } catch {
            Start-Sleep -Milliseconds 50
        }
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Terminal probe report remained unreadable: $Path"
}

function Wait-PaneText {
    param([int]$Pane, [string]$Sentinel, [int]$TimeoutSeconds = 4)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $content = Invoke-GuiCli @('get-text', '--pane-id', "$Pane")
        if ($content.Contains($Sentinel)) { return $content }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Pane $Pane did not render $Sentinel within $TimeoutSeconds seconds"
}

try {
    # Each query runs in the real GUI's ConPTY, then reports raw reply bytes.
    $keyboardScript = Write-Ansi 'keyboard-query.bin' "${esc}[?u"
    $keyboardReport = Join-Path $ProbeDir 'keyboard-query.json'
    $keyboardPane = Start-ProbePane @('--mock-ansi-script', $keyboardScript,
                            '--mock-read-stdin-ms', '1800',
                            '--mock-report-file', $keyboardReport,
                            '--mock-exit-code', '23')
    $keyboard = Get-ProbeReport $keyboardReport
    if ([string]$keyboard.stdin -notmatch '\x1b\[\?\d+u') {
        throw "Kitty keyboard mode query had no CSI ? flags u reply: $($keyboard.stdin)"
    }
    Stop-ProbePane $keyboardPane

    $graphicsScript = Write-Ansi 'graphics-query.bin' "${esc}_Gi=731,a=q,s=1,v=1,f=32;AAAAAA==${esc}\"
    $graphicsReport = Join-Path $ProbeDir 'graphics-query.json'
    $graphicsPane = Start-ProbePane @('--mock-ansi-script', $graphicsScript,
                            '--mock-read-stdin-ms', '1800',
                            '--mock-report-file', $graphicsReport,
                            '--mock-exit-code', '23')
    $graphics = Get-ProbeReport $graphicsReport
    if (-not ([string]$graphics.stdin).Contains("${esc}_Gi=731;OK")) {
        throw "Kitty graphics query had no matching OK reply: $($graphics.stdin)"
    }
    Stop-ProbePane $graphicsPane

    $pasteScript = Write-Ansi 'bracketed-paste.bin' "${esc}[?2004h"
    $pasteReady = Join-Path $ProbeDir 'paste-ready.json'
    $pasteReport = Join-Path $ProbeDir 'paste-report.json'
    $pastePane = Start-ProbePane @('--mock-ansi-script', $pasteScript,
                                  '--mock-stdin-ready-file', $pasteReady,
                                  '--mock-read-stdin-ms', '2500',
                                  '--mock-report-file', $pasteReport,
                                  '--mock-exit-code', '23')
    Wait-ProbeFile $pasteReady
    [void](Invoke-GuiCli @('send-text', '--pane-id', "$pastePane", 'PASTE_SENTINEL'))
    $paste = Get-ProbeReport $pasteReport
    if (-not ([string]$paste.stdin).Contains("${esc}[200~PASTE_SENTINEL${esc}[201~")) {
        throw "Bracketed paste framing missing from ConPTY stdin: $($paste.stdin)"
    }
    Stop-ProbePane $pastePane

    # A raw ETX injected by the CLI is not evidence of a physical Ctrl-C key.
    $etxReady = Join-Path $ProbeDir 'etx-ready.json'
    $etxReport = Join-Path $ProbeDir 'etx-report.json'
    $etxPane = Start-ProbePane @('--mock-stdin-ready-file', $etxReady,
                                '--mock-read-stdin-ms', '2500',
                                '--mock-report-file', $etxReport,
                                '--mock-exit-code', '23')
    Wait-ProbeFile $etxReady
    [void](Invoke-GuiCli @('send-text', '--pane-id', "$etxPane", '--no-paste', ([string][char]3)))
    $etx = Get-ProbeReport $etxReport
    if (-not ([string]$etx.stdin).Contains([string][char]3)) {
        throw "Raw ETX did not reach the ConPTY child: $($etx.stdin)"
    }
    Stop-ProbePane $etxPane

    $mainScript = Write-Ansi 'alternate-enter.bin' "MAIN_SENTINEL${esc}[?1049hALT_SENTINEL"
    $restoreScript = Write-Ansi 'alternate-exit.bin' "${esc}[?1049lRESTORED_SENTINEL"
    $altReady = Join-Path $ProbeDir 'alt-ready.json'
    $altRelease = Join-Path $ProbeDir 'alt-release.txt'
    $altReport = Join-Path $ProbeDir 'alt-report.json'
    $altPane = Start-ProbePane @('--mock-ansi-script', $mainScript,
                                '--mock-ansi-after-wait', $restoreScript,
                                '--mock-ready-file', $altReady,
                                '--mock-wait-for-file', $altRelease,
                                '--mock-report-file', $altReport,
                                '--mock-exit-code', '23')
    Wait-ProbeFile $altReady
    $altText = Wait-PaneText $altPane 'ALT_SENTINEL'
    if (-not $altText.Contains('ALT_SENTINEL') -or $altText.Contains('MAIN_SENTINEL')) {
        throw 'Alternate screen did not replace the main screen text'
    }
    [IO.File]::WriteAllText($altRelease, 'restore')
    [void](Get-ProbeReport $altReport)
    $mainText = Wait-PaneText $altPane 'RESTORED_SENTINEL'
    if (-not $mainText.Contains('MAIN_SENTINEL') -or
        -not $mainText.Contains('RESTORED_SENTINEL') -or
        $mainText.Contains('ALT_SENTINEL')) {
        throw 'Main screen did not return after alternate-screen exit'
    }
    Stop-ProbePane $altPane

    $rgbScript = Write-Ansi 'truecolor.bin' "${esc}[38;2;12;34;56mRGB_SENTINEL${esc}[0m"
    $rgbReport = Join-Path $ProbeDir 'rgb-report.json'
    $rgbPane = Start-ProbePane @('--mock-ansi-script', $rgbScript,
                                '--mock-report-file', $rgbReport,
                                '--mock-exit-code', '23')
    [void](Get-ProbeReport $rgbReport)
    [void](Wait-PaneText $rgbPane 'RGB_SENTINEL')
    $rgbText = Invoke-GuiCli @('get-text', '--escapes', '--pane-id', "$rgbPane")
    if (-not $rgbText.Contains('RGB_SENTINEL') -or
        $rgbText -notmatch '38(?:;2;12;34;56|:2::12:34:56)') {
        throw "Truecolor style was not retained in escaped pane text: $rgbText"
    }
    Stop-ProbePane $rgbPane

    $sizeReport = Join-Path $ProbeDir 'pty-size.json'
    $sizePane = Start-ProbePane @('--mock-report-pty-size', $sizeReport,
                                 '--mock-pty-size-samples', '50',
                                 '--mock-pty-size-interval-ms', '200',
                                 '--mock-exit-code', '23') -Right
    $sizeDeadline = [DateTime]::UtcNow.AddSeconds(6)
    $sizes = @()
    do {
        Start-Sleep -Milliseconds 100
        try {
            $sizes = @(Get-Content -LiteralPath $sizeReport -Raw | ConvertFrom-Json)
        } catch {
            # The mock rewrites its sample array; a read can catch an update.
            $sizes = @()
        }
    } while (($sizes.Count -lt 1 -or $null -eq $sizes[-1].cols) -and
             [DateTime]::UtcNow -lt $sizeDeadline)
    if ($sizes.Count -lt 1 -or $null -eq $sizes[-1].cols) {
        throw 'ConPTY child did not report a pre-resize terminal size'
    }
    $beforeCount = $sizes.Count
    $beforeCols = [int]$sizes[-1].cols
    [void](Invoke-GuiCli @('adjust-pane-size', '--pane-id', "$sizePane", '--amount', '8', 'Left'))
    $sizeDeadline = [DateTime]::UtcNow.AddSeconds(6)
    $changed = $false
    do {
        Start-Sleep -Milliseconds 100
        try {
            $sizes = @(Get-Content -LiteralPath $sizeReport -Raw | ConvertFrom-Json)
        } catch { continue }
        foreach ($sample in @($sizes | Select-Object -Skip $beforeCount)) {
            if ($null -ne $sample.cols -and [int]$sample.cols -ne $beforeCols) {
                $changed = $true
                break
            }
        }
    } while (-not $changed -and [DateTime]::UtcNow -lt $sizeDeadline)
    if (-not $changed) {
        throw "ConPTY pane resize did not change columns from $beforeCols in later samples: $($sizes | ConvertTo-Json -Compress)"
    }
    Stop-ProbePane $sizePane

    $heavy = [Text.StringBuilder]::new()
    for ($i = 0; $i -lt 20000; $i++) {
        [void]$heavy.Append("HEAVY_$i : 0123456789abcdef0123456789abcdef`n")
    }
    [void]$heavy.Append("HEAVY_FINAL_SENTINEL`n")
    $heavyScript = Write-Ansi 'heavy-output.bin' $heavy.ToString()
    $heavyReport = Join-Path $ProbeDir 'heavy-report.json'
    $heavyPane = Start-ProbePane @('--mock-ansi-script', $heavyScript,
                                  '--mock-report-file', $heavyReport,
                                  '--mock-exit-code', '23')
    [void](Get-ProbeReport $heavyReport)
    $heavyText = Wait-PaneText $heavyPane 'HEAVY_FINAL_SENTINEL' 12
    if (-not $heavyText.Contains('HEAVY_FINAL_SENTINEL')) {
        throw 'Heavy ConPTY output did not reach its final marker in the GUI'
    }
    Stop-ProbePane $heavyPane
    Write-Host 'Native Kitty GUI terminal semantics passed: keyboard/graphics replies, paste, ETX, alt screen, RGB, resize, heavy output'
} finally {
    foreach ($pane in $panes) {
        try { [void](Invoke-GuiCli @('kill-pane', '--pane-id', "$pane") 2000) } catch {
            Write-Host "Terminal probe cleanup could not kill pane $pane`: $_"
        }
    }
}
