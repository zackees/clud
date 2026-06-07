# Changelog

## 2.0.19 - 2026-06-07

- `/clud-pr` and other bundled skills now install under `~/.codex/skills/`
  for `clud --codex`, matching the skill path Codex itself loads from.
- Old clud-managed bundled skill copies under `~/.agents/skills/` are removed
  on first Codex global setup after upgrade. User-authored content and
  unrelated skill directories are preserved.
