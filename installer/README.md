# Installer source

`catalog.py` verifies public release assets and builds the v1 install catalog.
`site.py` renders the GitHub Pages artifact; `verify_site.py` checks its public
URL contract. See [Installer Pages architecture](../docs/architecture/installer-pages.md)
for the cross-file flow and the `bosn`/`act` validation command.

The repository root `install` file and the existing `install.sh` and
`install.ps1` are separate developer and shell installation routes; this
directory does not replace them.
