-- Owned by clud. Kitty-style defaults modeled on zackees/nixos/home/kitty.
-- Loaded with `wezterm --config-file`; do not depend on the user's wezterm.lua.
local wezterm = require 'wezterm'
local act = wezterm.action
local config = wezterm.config_builder()

-- Headless Windows CI uses WebGpu's fallback CPU adapter to avoid glium's
-- OpenGL requirement on the hosted runner. Interactive installs retain
-- WezTerm's normal renderer selection.
if os.getenv('CLUD_KITTYTERM_SOFTWARE_RENDERER') == '1' then
  config.front_end = 'WebGpu'
  config.webgpu_force_fallback_adapter = true
end

-- Keep graphics enabled and let applications negotiate the Kitty keyboard
-- protocol. Keep ConPTY's native input mode on
-- Windows, where it takes precedence for Windows console applications.
config.enable_kitty_graphics = true
config.enable_kitty_keyboard = true
config.allow_win32_input_mode = true
config.set_environment_variables = { CLUD_KITTY_TERM = '1' }

config.font = wezterm.font('JetBrainsMono Nerd Font')
config.font_size = 10.0
config.line_height = 1.1
config.default_cursor_style = 'BlinkingBar'
config.cursor_blink_rate = 500
config.cursor_thickness = '1.6pt'
config.scrollback_lines = 100000
config.hide_mouse_cursor_when_typing = true
config.window_padding = { left = 8, right = 8, top = 8, bottom = 8 }
config.initial_cols = 130
config.initial_rows = 36
config.window_close_confirmation = 'AlwaysPrompt'
config.audible_bell = 'Disabled'
config.visual_bell = { fade_in_duration_ms = 0, fade_out_duration_ms = 100 }

-- Breeze Dark, including the source configuration's ANSI and split colors.
config.colors = {
  background = '#232629', foreground = '#fcfcfc',
  cursor_bg = '#fcfcfc', cursor_fg = '#232629',
  selection_bg = '#3daee9', selection_fg = '#232629',
  split = '#4d4d4d',
  ansi = { '#232627', '#ed1515', '#11d116', '#f67400',
           '#1d99f3', '#9b59b6', '#1abc9c', '#fcfcfc' },
  brights = { '#7f8c8d', '#c0392b', '#1cdc9a', '#fdbc4b',
              '#3daee9', '#8e44ad', '#16a085', '#ffffff' },
  tab_bar = {
    background = '#2a2e32',
    active_tab = { bg_color = '#3daee9', fg_color = '#232629' },
    inactive_tab = { bg_color = '#2a2e32', fg_color = '#7f8c8d' },
  },
}
-- Available in current WezTerm nightlies. The pinned fork must contain it.
config.text_min_contrast_ratio = 7.0
config.hide_tab_bar_if_only_one_tab = true
config.use_fancy_tab_bar = false

local function directory_label(uri)
  if uri == nil then return nil end
  local ok, file_path = pcall(function() return uri.file_path end)
  local path = (ok and file_path) or tostring(uri)
  if path == nil or path == '' then return nil end
  path = path:gsub('[\\/]+$', '')
  return path:match('([^/\\]+)$') or path
end

wezterm.on('format-tab-title', function(tab)
  local pane = tab.active_pane
  local directory = directory_label(pane.current_working_dir)
  local title = directory or pane.title
  if tab.tab_title and tab.tab_title ~= '' then title = tab.tab_title end
  if (not tab.tab_title or tab.tab_title == '') and directory and pane.title and pane.title ~= directory then
    title = directory .. ' · ' .. pane.title
  end
  return string.format(' %d: %s ', tab.tab_index + 1, title or 'shell')
end)

local shortcut_descriptions = {}
local function bind(key, mods, action, description)
  assert(type(description) == 'string' and description ~= '', 'binding needs a shortcut description')
  local binding = { key = key, mods = mods, action = action }
  shortcut_descriptions[binding] = description
  return binding
end

local function shortcut_sheet(window, pane)
  local choices = {}
  local actions = {}
  for _, binding in ipairs(config.keys) do
    local description = shortcut_descriptions[binding]
    if description then
      local id = tostring(#choices + 1)
      local keys = binding.mods:gsub('SUPER', 'Super'):gsub('SHIFT', 'Shift')
        :gsub('CTRL', 'Ctrl'):gsub('ALT', 'Alt'):gsub('NONE', '')
      local label = (keys == '' and '' or keys:gsub('|', '+') .. '+')
        .. binding.key .. '  —  ' .. description
      table.insert(choices, { id = id, label = label })
      actions[id] = binding.action
    end
  end
  window:perform_action(act.InputSelector {
    title = 'Kitty-style shortcuts',
    choices = choices,
    fuzzy = true,
    action = wezterm.action_callback(function(selected_window, selected_pane, id)
      if id and actions[id] then
        selected_window:perform_action(actions[id], selected_pane)
      end
    end),
  }, pane)
end

local rename_tab = act.PromptInputLine {
  description = 'New tab title (empty restores the cwd and command label)',
  action = wezterm.action_callback(function(window, _, line)
    if line ~= nil then window:active_tab():set_title(line) end
  end),
}

-- SpawnCommand inherits the active pane's OSC 7 cwd when its cwd is omitted.
-- In WezTerm, SplitVertical places panes above/below; SplitHorizontal places
-- them side by side, unlike Kitty's hsplit/vsplit naming.
local above_below = act.SplitVertical { domain = 'CurrentPaneDomain' }
local side_by_side = act.SplitHorizontal { domain = 'CurrentPaneDomain' }

-- WezTerm Lua cannot read the system clipboard. On Windows, clud's adjacent
-- Rust helper snapshots text or image data, then this callback pastes into the
-- pane that initiated the action. No shell evaluates the helper path or data.
local smart_paste = act.PasteFrom 'Clipboard'
if wezterm.target_triple:find('windows', 1, true) then
  local config_dir = wezterm.config_file:match('^(.*)[/\\]') or '.'
  local helper = config_dir .. '\\clud-kittyterm-paste.exe'

  local function safe_text(text)
    return text:gsub('%c', function(char)
      if char == '\n' or char == '\r' or char == '\t' then return char end
      return ''
    end)
  end

  local function quote_windows_path(path, pane)
    local program = (pane:get_foreground_process_name() or ''):lower()
    program = program:gsub('\\', '/'):match('([^/]+)$') or program
    if program == 'powershell.exe' or program == 'pwsh.exe' then
      -- Single quotes keep $, backticks and % literal; double apostrophes.
      return "'" .. path:gsub("'", "''") .. "'"
    end
    if program == 'cmd.exe' then
      -- Interactive cmd does percent expansion inside quotes (and ! when
      -- delayed expansion is enabled). This is a bounded limitation for paths
      -- containing those characters; batch-file %% escaping is wrong here.
      return '"' .. path .. '"'
    end
    -- An AI prompt is text, not a shell. Keep spaces and apostrophes readable.
    -- Windows file names cannot contain a double quote.
    return '"' .. path .. '"'
  end

  smart_paste = wezterm.action_callback(function(window, pane)
    local ok, output, err = wezterm.run_child_process { helper }
    if not ok then
      wezterm.log_error('clud Kitty paste helper failed: ' .. tostring(err))
      return
    end
    local decoded, payload = pcall(wezterm.json_parse, output)
    if not decoded or type(payload) ~= 'table' or type(payload.value) ~= 'string'
        or type(payload.bytes) ~= 'number' then
      wezterm.log_error('clud Kitty paste helper returned invalid JSON')
      return
    end
    if payload.kind ~= 'text' and payload.kind ~= 'image' then return end
    if payload.bytes < 0 or payload.bytes > 52428800 then return end

    local value = payload.value
    if payload.kind == 'text' then
      value = safe_text(value)
      if value == '' then return end
    else
      -- The helper only emits a validated PNG path in our own pictures folder.
      if not value:lower():match('%.png$') then return end
      value = quote_windows_path(value, pane)
    end

    local function deliver(accepted)
      if accepted then
        -- send_paste uses bracketed paste when the target pane requests it.
        local sent, send_error = pcall(function() pane:send_paste(value) end)
        if not sent then
          if payload.kind == 'image' then os.remove(payload.value) end
          wezterm.log_error('clud Kitty paste failed: ' .. tostring(send_error))
        end
      elseif payload.kind == 'image' then
        os.remove(payload.value)
      end
    end

    if payload.bytes <= 16384 then
      deliver(true)
      return
    end
    window:perform_action(act.InputSelector {
      title = string.format('Paste %s (%d kB)?', payload.kind,
                            math.ceil(payload.bytes / 1000)),
      choices = { { id = 'no', label = 'No' }, { id = 'yes', label = 'Yes' } },
      action = wezterm.action_callback(function(_, _, id)
        deliver(id == 'yes')
      end),
    }, pane)
  end)
end

config.keys = {
  bind('F5', 'NONE', above_below, 'Split above/below'),
  bind('F6', 'NONE', side_by_side, 'Split side by side'),
  bind('Enter', 'CTRL|SHIFT', side_by_side, 'Split side by side'),
  bind('Enter', 'ALT', side_by_side, 'Split side by side'),
  bind('-', 'ALT', above_below, 'Split above/below'),
  bind('\\', 'ALT', side_by_side, 'Split side by side'),
  bind('Enter', 'SUPER', side_by_side, 'Split side by side'),
  bind('Enter', 'SUPER|SHIFT', act.SpawnCommandInNewWindow {}, 'New window'),
  bind('t', 'CTRL|SHIFT', act.SpawnTab 'CurrentPaneDomain', 'New tab'),
  bind('n', 'CTRL|SHIFT', act.SpawnCommandInNewWindow {}, 'New window'),
  bind('h', 'ALT', act.ActivatePaneDirection 'Left', 'Focus pane left'),
  bind('j', 'ALT', act.ActivatePaneDirection 'Down', 'Focus pane down'),
  bind('k', 'ALT', act.ActivatePaneDirection 'Up', 'Focus pane up'),
  bind('l', 'ALT', act.ActivatePaneDirection 'Right', 'Focus pane right'),
  bind('h', 'ALT|CTRL', act.AdjustPaneSize { 'Left', 3 }, 'Resize pane left'),
  bind('j', 'ALT|CTRL', act.AdjustPaneSize { 'Down', 3 }, 'Resize pane down'),
  bind('k', 'ALT|CTRL', act.AdjustPaneSize { 'Up', 3 }, 'Resize pane up'),
  bind('l', 'ALT|CTRL', act.AdjustPaneSize { 'Right', 3 }, 'Resize pane right'),
  bind('z', 'ALT', act.TogglePaneZoomState, 'Zoom pane'),
  bind('p', 'ALT', act.PaneSelect { alphabet = '1234567890' }, 'Choose pane'),
  bind('p', 'ALT|SHIFT', act.PaneSelect { mode = 'SwapWithActive', alphabet = '1234567890' }, 'Swap panes'),
  bind('r', 'ALT|SHIFT', act.RotatePanes 'Clockwise', 'Rotate panes clockwise'),
  bind('r', 'ALT|CTRL|SHIFT', act.RotatePanes 'CounterClockwise', 'Rotate panes counterclockwise'),
  bind('d', 'ALT|SHIFT', act.PaneSelect { mode = 'MoveToNewWindow' }, 'Detach pane to window'),
  bind('w', 'ALT|SHIFT', act.CloseCurrentPane { confirm = true }, 'Close pane'),
  bind('d', 'SUPER|SHIFT', act.PaneSelect { mode = 'MoveToNewWindow' }, 'Move pane to new window'),
  bind('t', 'SUPER|SHIFT', act.PaneSelect { mode = 'MoveToNewTab' }, 'Move pane to new tab'),
  bind('x', 'SUPER|SHIFT', act.PaneSelect { mode = 'MoveToNewWindow' }, 'Move pane to new window'),
  bind('t', 'ALT', act.ShowLauncherArgs { flags = 'FUZZY|TABS' }, 'Choose tab'),
  bind('t', 'ALT|SHIFT', rename_tab, 'Rename tab'),
  bind('m', 'CTRL|SHIFT', act.ActivateCommandPalette, 'Command palette'),
  bind('u', 'ALT|SHIFT', act.ActivateCopyMode, 'Browse scrollback'),
  bind('Home', 'CTRL|SHIFT', act.ScrollToTop, 'Scroll to top'),
  bind('End', 'CTRL|SHIFT', act.ScrollToBottom, 'Scroll to bottom'),
  bind('PageUp', 'CTRL|SHIFT', act.ScrollByPage(-1), 'Scroll up one page'),
  bind('PageDown', 'CTRL|SHIFT', act.ScrollByPage(1), 'Scroll down one page'),
  bind('g', 'ALT|SHIFT', act.ScrollToPrompt(-1), 'Go to previous prompt'),
  bind('F5', 'CTRL|SHIFT', act.ReloadConfiguration, 'Reload configuration'),
  bind('/', 'SUPER', wezterm.action_callback(shortcut_sheet), 'Show shortcut sheet'),
  bind('/', 'SUPER|SHIFT', wezterm.action_callback(shortcut_sheet), 'Show shortcut sheet'),
  bind('c', 'SUPER', act.CopyTo 'Clipboard', 'Copy'),
  bind('v', 'SUPER', smart_paste, 'Paste'),
  bind('v', 'CTRL', smart_paste, 'Paste'),
  bind('Insert', 'SHIFT', smart_paste, 'Paste'),
  bind('v', 'CTRL|SHIFT', act.PasteFrom 'Clipboard', 'Paste text'),
  bind('+', 'CTRL', act.IncreaseFontSize, 'Increase font size'),
  bind('-', 'CTRL', act.DecreaseFontSize, 'Decrease font size'),
  bind('0', 'CTRL', act.ResetFontSize, 'Reset font size'),
}

for index = 1, 9 do
  table.insert(config.keys, bind(tostring(index), 'ALT', act.ActivateTab(index - 1), 'Go to tab ' .. index))
end

-- WezTerm's default selection already copies to both clipboards. Override
-- middle click to paste the regular clipboard, as the NixOS config does.
config.mouse_bindings = {
  {
    event = { Down = { streak = 1, button = 'Middle' } },
    mods = 'NONE',
    action = smart_paste,
  },
  {
    event = { Down = { streak = 1, button = 'Right' } },
    mods = 'NONE',
    action = act.Nop,
  },
}

table.insert(config.mouse_bindings, {
  event = { Up = { streak = 1, button = 'Right' } },
  mods = 'NONE',
  action = act.Nop,
})

return config
