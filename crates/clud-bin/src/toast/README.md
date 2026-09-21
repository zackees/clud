# `toast/`

In-terminal toast notifications (#1189). Design and limits:
[docs/architecture/toasts.md](../../../../docs/architecture/toasts.md).

| File | What it owns |
|---|---|
| `mod.rs` | `Toast`, `ToastEvent`, `ToastBoard`, `ToastHub`, `ToastSink`, `ToastLaunchCfg` |
| `compositor.rs` | `Compositor` on the PTY writer thread: safe injection, tier rendering, fallback surfaces, close-button arming, persistent Kitty usage strip; `ToastPumpOptions`, `Fallback`, `ToastInput` |
| `tracker.rs` | `EscapeTracker`: sequence/UTF-8 state, DECSET 2026, DECOM, DECSTBM, image-dropping events |
| `kitty.rs` | kitty graphics commands (`transmit_png`, `place`, `delete_*`), all with `q=2` |
| `raster.rs` | toast and persistent-usage PNG panels via bundled DejaVu Sans Mono (`assets/fonts/`) |
| `text_tier.rs` | one-row cell toast and restore-from-shadow (`restore_cells`, `restore_screen`) |
| `mouse.rs` | `MouseFilter`: consume only toast/usage-panel SGR controls, pass keyboard and unrelated mouse bytes through |
| `tier.rs` | `decide`: kitty / text cells / fallback per terminal; `CLUD_TOAST_TIER` override |
| `statusline.rs` | Claude status-line surface: `StatusStateWriter`, `clud statusline` (`run`), command composition, user status-line discovery and chaining |
| `launch.rs` | per-launch wiring for both runners: status-line writer, injection, fallback choice, `CLUD_TOAST_DEMO` |

Callers:

- `cpu_banner.rs` publishes through a `ToastSink` (`toast_events`).
- `runner.rs` (subprocess) and `runner_execution.rs` (PTY) build the sink,
  hub, launch-wide usage writer and compositor options.
- `session.rs` / `session_output.rs` run the compositor and the mouse filter.
- `foreground_runtime.rs::start_with_statusline` composes Claude's `statusLine`.
- `main.rs` dispatches `clud statusline` before any launch work.
