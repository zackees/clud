# Shared global agent directives

Run `bash contrib/agent-directives/install.sh` once. It creates
`~/.config/agent-directives/INSTRUCTIONS.md` from the template and links both
global instruction paths to it:

- Codex: `$CODEX_HOME/AGENTS.md` (default `~/.codex/AGENTS.md`)
- Claude Code: `$CLAUDE_CONFIG_DIR/CLAUDE.md` (default `~/.claude/CLAUDE.md`)

Add one short instruction per line to the shared file. New sessions in both
harnesses read the updated content automatically. The installer preserves an
existing shared file, is safe to repeat, and refuses to replace unrelated
instruction files.

These files are the harnesses' global startup instructions, not literal
system-role messages. Direct user requests may override them.
