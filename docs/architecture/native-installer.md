# Native clud installer entry

Issue #1493 adds an early native entry point to the regular `clud` binary.
`--installer` opens a selector menu; `--install-current` and
`--install-version VERSION` choose an installation source, and `--yes` skips
interactive consent only when one of those choices is explicit. Existing
`--yes --clean-worktrees` behavior is unaffected. Installer flags are claimed
by both clap and the known/unknown argument splitter, so they cannot leak to
a backend agent.

`main.rs::run` calls `self_install::entry` after argument normalization and
before utility dispatch, daemon startup, the runtime-cache hop, Windows
trampoline handling, and the console-title keeper. The early path keeps an
installer invocation independent of backend credentials and session setup.

A bare interactive invocation offers installation only when `clud` cannot be
resolved by name on the effective PATH. A found executable, even one different
from the invoking binary, suppresses that offer. The offer defaults to **Not
now** and declining continues ordinary startup. Explicit mode works regardless
of PATH. Noninteractive explicit mode requires a single choice and `--yes`;
otherwise it exits without writing.

All menus are selector models in `self_install/picker.rs` rendered by the
shared `selector.rs` terminal driver. The driver owns pending-input draining,
raw mode, CRLF output, viewport scrolling, Windows key-release filtering,
cursor restoration, and Escape/Ctrl-C handling. The release picker uses
`Catalog::compatible_releases`; its default follows the `latest-stable`
channel rather than assuming the first row is stable. Browser choice opens
`https://zackees.github.io/clud/install/index.html` and prints that URL if
the opener fails.

An accepted selection becomes either `InstallIntent::CurrentExecutable` or
`InstallIntent::Published(asset)`. `transaction.rs::plan` runs without writes,
records the source digest and destination state, and shows the exact plan
before consent. Only then does `execute` create the approved per-user bin
directory, acquire its install lock, stage a private executable, verify its
format, digest, and version, and commit it with a recoverable prior copy.
The running-executable path copies offline. The release path uses the exact
catalog asset, bounds and hashes its transfer, validates every redirect, and
extracts only one executable from a historical wheel. A matching direct
release can reuse the invoking executable only when its digest matches.

The binary commit returns a pending activation status. Persistent shell or
Windows User PATH edits and fresh name-based lookup are owned by #1495, so
an absolute-path version check cannot claim overall install success.
