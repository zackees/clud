# Kimi real-key smoke (opt-in, never CI)

Manual check that a real Kimi key works end to end without leaking. Run it
when you change Kimi's descriptor, catalog row, overlay, or unified route. CI
covers the same paths with fake upstreams; this is the only check against
Moonshot itself, and it spends a small amount of credit.

Never paste the key, `/status` output that shows it, or a transcript into an
issue or PR.

1. Store the key through the vault, not the environment:

   ```
   clud auth login kimi
   clud auth status kimi          # expect: configured
   ```

2. Direct launch: `clud --kimi`. In the child:
   - `/status` shows the Moonshot endpoint (`api.moonshot.ai/anthropic`) and
     `kimi-k3[1m]`.
   - Ask one question and watch the reply stream in progressively.
   - Ask for one tool call, for example "list the files in this directory".
   - WebFetch is expected to fail: it is a known Moonshot limitation
     ([provider-selection.md](../docs/architecture/provider-selection.md#kimi-known-provider-side-limitation)).

3. Unified launch: `clud --unified`. `/model` lists **Kimi K3**
   (`clud-claude-kimi-k3`). Select it, send one turn, then switch to a Claude
   model and send another. Both succeed in one session.

4. Leak sweep. Take the key's first 8 characters as `PREFIX` (not the whole
   key), then search everything a session writes:

   ```
   grep -rF "$PREFIX" ~/.clud ~/.claude/projects 2>/dev/null
   ```

   Expect no matches. A match anywhere is a bug: file it without the key.

5. Clean up if the key was a throwaway: `clud auth logout kimi`.
