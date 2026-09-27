# Installer Pages and release catalog

Issue #1468 uses GitHub Pages as the installer discovery endpoint. The canonical
catalog URL is `https://zackees.github.io/clud/install/manifest.json`; it is a
`manifest.json` v1 Catalog, not the repository's internal test-bundle manifest.

`installer/catalog.py` fetches the public GitHub release list and verifies each
candidate wheel or direct clud asset against GitHub's published SHA-256 and byte
size. Older wheel-only releases are advertised only when their archive contains
exactly one `clud`/`clud.exe` member with executable-format bytes. A direct
standalone binary takes precedence over a wheel for the same platform variant.
Historical Linux GNU assets carry `platform.libc=glibc` and
`variant.flavor=gnu`; verified static musl direct assets carry
`variant.flavor=static-musl` without a host libc requirement. The generator
rejects musl assets with an ELF interpreter or a needed shared library. The
catalog retains every installable published version and puts semantic versions
newest first; `latest-stable` is a channel pointer, not array position.

The Rust reader in `crates/clud-bin/src/self_install/catalog.rs` accepts only a
bounded v1 clud Catalog with unique JSON keys, exact known platform variants,
matching release filenames and GitHub asset URLs, positive sizes, and SHA-256
digests. It rejects an incomplete or prerelease `latest-stable` pointer and
does not advertise incomplete historical releases. On Linux it selects a
static musl asset before GNU regardless of catalog row order. GNU-only history
requires an explicit host probe result proving a non-NixOS GNU loader and
glibc 2.17 or newer. The selected asset's size and digest are checked before
installation; a bad musl download is terminal rather than a reason to fall
back to GNU.

`installer/site.py` renders the exact Pages artifact paths: `index.html`,
`install/index.html`, and `install/manifest.json`. The project root is a static
client-side redirect to `/clud/install/index.html` with one hyperlink to the
catalog. Until a release actually contains `clud-installer.exe`, the landing
page links to the current release rather than a nonexistent installer asset.

`.github/workflows/install-pages.yml` runs the unit suite and upstream
`manifest-validate` on PRs, then publishes only on `main` or manual dispatch.
It performs an unauthenticated smoke check of all three public URLs after
deployment. `bosn run --task act-installer-pages` executes the build/validation
job locally through `act`; it cannot stand in for the Pages deployment API.
The full-history verification downloads about 5.5 GB across the 27 releases
present when this flow was introduced, so its build job has a 30-minute limit.

The installer and release jobs will consume this catalog and deploy an updated
site after new assets are published; those mechanics are described separately
in the implementation issue rather than pretending this Pages phase installs
clud by itself.
