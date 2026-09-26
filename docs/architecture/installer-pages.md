# Installer Pages and release catalog

Issue #1468 uses GitHub Pages as the installer discovery endpoint. The canonical
catalog URL is `https://zackees.github.io/clud/install/manifest.json`; it is a
`manifest.json` v1 Catalog, not the repository's internal test-bundle manifest.

`installer/catalog.py` fetches the public GitHub release list and verifies each
candidate wheel or direct clud asset against GitHub's published SHA-256 and byte
size. Older wheel-only releases are advertised only when their archive contains
exactly one `clud`/`clud.exe` member with executable-format bytes. A direct
standalone binary takes precedence over a wheel for the same platform. The
catalog retains every installable published version and puts semantic versions
newest first; `latest-stable` is a channel pointer, not array position.

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
