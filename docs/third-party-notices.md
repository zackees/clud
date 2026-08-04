# Third-party notices and compatibility references

This repository does not ship OpenAI Codex or CLIProxyAPI source code, binaries,
or a sidecar. The Codex-via-Claude implementation is clud-owned Rust code.
These notices record compatibility research and must be checked by a release
maintainer whenever copied code or a new dependency is proposed.

| Project | License | Relationship |
|---|---|---|
| [OpenAI Codex](https://github.com/openai/codex) | Apache-2.0 | OAuth compatibility shape and public client behavior were researched; no source was copied. |
| [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) | MIT | Translation/failure classifications were compared during design; no source was copied. |

Before release, the maintainer must confirm the current upstream license files,
review every new bridge file for copied material, and add the applicable notice
and license text if source is ever imported. Do not turn a compatibility
reference into an attribution claim that conceals copied code.
