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
        r"config\.front_end = 'Software'\s+end",
        config,
    )
    assert config.count("config.front_end = 'Software'") == 1
    assert not re.search(r"config\.front_end\s*=", config.split("if os.getenv", 1)[0])


def test_windows_smoke_selects_software_renderer_for_both_gui_paths() -> None:
    smoke = SMOKE.read_text(encoding="utf-8")
    assert "$start.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'" in smoke
    assert "$outer.Environment['CLUD_KITTYTERM_SOFTWARE_RENDERER'] = '1'" in smoke


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
