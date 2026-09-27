# Installer source

`catalog.py` verifies public release assets and builds the v1 install catalog.
`site.py` renders the GitHub Pages artifact; `verify_site.py` checks its public
URL contract. See [Installer Pages architecture](../docs/architecture/installer-pages.md)
for the cross-file flow and the `bosn`/`act` validation command.

The compact APE uses the operating system's `curl` command for HTTPS downloads.
macOS and supported Windows versions provide it; minimal Linux installations
may need to install `curl` first (for example, `sudo pacman -S curl` on Arch).
The installer reports this requirement if it cannot fetch the catalog or the
selected release asset.

The repository root `install` file and the existing `install.sh` and
`install.ps1` are separate developer and shell installation routes; this
directory does not replace them.
