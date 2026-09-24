"""Guard the bundled Kitty-style WezTerm configuration against drift."""

from __future__ import annotations

import re
from pathlib import Path

CONFIG = (
    Path(__file__).resolve().parents[1]
    / "crates/clud-bin/assets/kitty/clud-kittyterm.lua"
)
SMOKE = Path(__file__).resolve().parents[1] / "ci/kitty_windows_smoke.ps1"


def test_software_renderer_is_opt_in_for_ci() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    assert re.search(
        r"if os\.getenv\('CLUD_KITTYTERM_SOFTWARE_RENDERER'\) == '1' then\s+"
        r"config\.front_end = 'WebGpu'\s+"
        r"config\.webgpu_force_fallback_adapter = true\s+end",
        config,
    )
    assert config.count("config.front_end = 'WebGpu'") == 1
    assert config.count("config.webgpu_force_fallback_adapter = true") == 1
    assert not re.search(r"config\.front_end\s*=", config.split("if os.getenv", 1)[0])
    before_ci_override = config.split("if os.getenv", 1)[0]
    assert not re.search(r"config\.webgpu_force_fallback_adapter\s*=", before_ci_override)


def test_windows_smoke_selects_software_renderer_for_both_gui_paths() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    assert "$start.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'" in smoke
    assert "$outer.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'" in smoke


def test_windows_smoke_proves_shared_gui_and_independent_child_statuses() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    launch_args = smoke.split("foreach ($arg in @(", 1)[1].split(")) {", 1)[0]
    assert "--always-new-process" not in launch_args
    assert "socket=$env:WEZTERM_UNIX_SOCKET" in smoke
    assert "WEZTERM_UNIX_SOCKET" in smoke
    assert '$start.Environment[\'PATH\'] = "$probeDir;' in smoke
    assert "$start.Environment['CLUD_KITTY_BACKEND_MARKER'] = $backendMarker" in smoke
    assert "[IO.File]::Copy($MockAgentPath, $backend)" in smoke
    assert "reused the live GUI" in smoke
    assert "first GUI child status 23" in smoke
    assert "reused pane status 37" in smoke


def test_windows_smoke_timeout_reports_whether_child_ran() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    timeout = smoke.split("if (-not (Test-Path -LiteralPath $marker", 1)[1]
    assert "marker=absent" in timeout
    assert "ParentProcessId = $($process.Id)" in timeout
    assert "exited=$($process.HasExited)" in timeout
    assert "backend_marker=$backendState version_probe=$versionState seed=$seedState" in smoke


def test_windows_smoke_cleans_up_both_process_trees_and_probe_directory() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    cleanup = smoke.rsplit("} finally {", 1)[1]
    assert "Stop-SmokeProcess $outerProcess 'clud launcher'" in cleanup
    assert "Stop-SmokeProcess $process 'seed GUI'" in cleanup
    assert 'Get-CimInstance Win32_Process -Filter "Name = \'wezterm-gui.exe\'"' in smoke
    assert "if ($candidate.ExecutablePath -eq $gui)" in smoke
    assert "$baselineGuiPids = @(Get-BundledGuiPids)" in smoke
    assert "if ($guiPid -in $baselineGuiPids) { continue }" in cleanup
    assert "Stop-SmokeProcess $newGuiProcess 'new bundled GUI'" in cleanup
    assert cleanup.index("Stop-SmokeProcess $outerProcess") < cleanup.index(
        "Stop-SmokeProcess $process"
    )
    assert "Remove-Item -LiteralPath $probeDir -Recurse -Force" in cleanup
    assert "$Target.Kill($true)" in smoke
    assert "$Target.WaitForExit(5000)" in smoke
    assert "$Target.Dispose()" in smoke
    assert "Write-Host \"Cleanup could not" in smoke


def test_windows_smoke_outer_timeout_reports_backend_and_gui_state() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    timeout = smoke.split("if (-not $outerProcess.WaitForExit", 1)[1].split(
        "if (-not (Test-Path -LiteralPath $backendMarker", 1
    )[0]
    assert "Test-Path -LiteralPath $backendMarker -PathType Leaf" in timeout
    assert "Get-Content -LiteralPath $backendMarker -Raw" in timeout
    assert "$guiPids = @(Get-BundledGuiPids)" in timeout
    assert (
        'Get-CimInstance Win32_Process -Filter "ParentProcessId = $($outerProcess.Id)"'
        in timeout
    )
    assert "backend_marker=$backendState" in timeout
    assert "bundled_gui_pids=$($guiPids -join ',')" in timeout
    assert "outer_children=$($outerChildren -join ',')" in timeout
    assert "version_probe=$versionState" in timeout
    assert "launch_trace=$($launchTrace -join ' || ')" in timeout
    assert "outer_argv=$($outer.ArgumentList -join ' ') cwd=$ScriptsDir" in timeout
    assert "no_daemon_control=$controlState" in timeout
    assert "control_caveat=first_attempt_pane_may_remain" in timeout
    assert "Stop-SmokeProcess $outerProcess 'timed-out clud launcher'" in timeout
    assert "$controlProcess.WaitForExit(15000)" in timeout
    assert "Test-Path -LiteralPath $controlMarker -PathType Leaf" in timeout
    assert "'--no-daemon', '-p', 'kitty-smoke-no-daemon'" in timeout
    assert timeout.index("Stop-SmokeProcess $outerProcess") < timeout.index(
        "$controlProcess = [Diagnostics.Process]::Start($control)"
    )
    assert "Get-ChildItem -LiteralPath $probeDir -Filter 'clud-*.log'" in timeout
    assert "Get-Content -LiteralPath $log.FullName -Tail 25" in timeout
    assert "Stop-SmokeProcess $process 'seed GUI'" not in timeout


def test_windows_smoke_uses_bundled_native_mock_backend() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    assert "[Parameter(Mandatory = $true)][string]$MockAgentPath" in smoke
    assert "[IO.File]::Copy($MockAgentPath, $backend)" in smoke
    assert "--mock-report-file', $backendMarker" in smoke
    assert "--mock-exit-code', '37'" in smoke
    assert "ConvertFrom-Json" in smoke
    assert "$backendResult.env.WEZTERM_UNIX_SOCKET" in smoke


def test_kitty_term_config_has_core_behaviors() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    for required in (
        "CLUD_KITTY_TERM = '1'",
        "enable_kitty_graphics = true",
        "enable_kitty_keyboard = true",
        "SplitVertical",
        "SplitHorizontal",
        "ActivatePaneDirection",
        "PaneSelect",
        "current_working_dir",
        "format-tab-title",
        "PasteFrom 'Clipboard'",
        "text_min_contrast_ratio = 7.0",
    ):
        assert required in config


def test_windows_paste_snapshots_clipboard_and_sanitizes_before_bracketed_paste() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    assert "clud-kittyterm-paste.exe" in config
    assert "powershell.exe" in config
    assert "wezterm.run_child_process" in config
    assert "wezterm.json_parse" in config
    assert "safe_text" in config
    assert "text:gsub('%c'" in config
    assert "char == '\\n' or char == '\\r' or char == '\\t'" in config
    assert "quote_windows_path(value, pane)" in config
    assert "get_foreground_process_name" in config
    assert "path:gsub(\"'\", \"''\")" in config
    assert "return '\"' .. path .. '\"'" in config
    assert "pane:send_paste" in config
    assert "16384" in config
    assert "action_callback" in config
    assert not CONFIG.with_name("clud-kittyterm-paste.ps1").exists()


def test_windows_image_path_quoting_distinguishes_shell_and_agent_prompts() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    # A direct PowerShell path can include spaces, apostrophes, $, backticks and
    # percent signs; its single-quoted form must be literal in PowerShell.
    assert "program == 'powershell.exe' or program == 'pwsh.exe'" in config
    assert "return \"'\" .. path:gsub(\"'\", \"''\") .. \"'\"" in config
    # Claude/Codex receive prompt text, so cmd-style double quotes remain easy
    # for them to read. cmd's interactive % expansion is documented in Lua.
    assert "program == 'cmd.exe'" in config
    assert "AI prompt" in config
    assert "percent expansion" in config
    assert "program:gsub('\\\\', '/')" in config
    for path in (
        r"C:\Users\Alice Smith\Pictures\paste.png",
        r"C:\Users\O'Neil\Pictures\paste.png",
        r"C:\Users\$name\Pictures\paste.png",
        r"C:\Users\tick`name\Pictures\paste.png",
        r"C:\Users\%USERNAME%\Pictures\paste.png",
    ):
        quoted = "'" + path.replace("'", "''") + "'"
        assert quoted[1:-1].replace("''", "'") == path


def test_shortcut_sheet_uses_live_key_bindings() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    assert "bind('/', 'SUPER', wezterm.action_callback(shortcut_sheet)" in config
    assert "bind('/', 'SUPER|SHIFT', wezterm.action_callback(shortcut_sheet)" in config
    assert "for _, binding in ipairs(config.keys)" in config
    assert "act.InputSelector" in config
    assert "shortcut_descriptions[binding]" in config
    assert "assert(type(description) == 'string' and description ~= ''" in config
    assert "description or tostring(action)" not in config


def test_supported_pane_and_scrollback_shortcuts() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    assert "act.RotatePanes 'Clockwise'" in config
    assert "act.RotatePanes 'CounterClockwise'" in config
    assert "act.ScrollToTop" in config
    assert "act.ScrollToBottom" in config
    assert "act.ScrollByPage" in config


def test_reference_tab_and_pane_management_bindings() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    assert "bind('t', 'ALT', act.ShowLauncherArgs { flags = 'FUZZY|TABS' }" in config
    assert "bind('t', 'ALT|SHIFT', rename_tab" in config
    assert "act.PromptInputLine" in config
    assert "window:active_tab():set_title(line)" in config
    assert "bind('d', 'ALT|SHIFT', act.PaneSelect" in config
    assert "bind('x', 'SUPER|SHIFT', act.PaneSelect" in config
    assert "bind('g', 'ALT|SHIFT', act.ScrollToPrompt(-1)" in config
    assert "bind('F5', 'CTRL|SHIFT', act.ReloadConfiguration" in config


def test_mouse_bindings_consume_both_right_click_edges() -> None:
    config = CONFIG.read_text(encoding="utf-8")
    assert "Down = { streak = 1, button = 'Right' }" in config
    assert "Up = { streak = 1, button = 'Right' }" in config
