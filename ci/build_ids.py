"""Publish which GNU build-id each released Linux binary carries (#1016).

A `.dwp` sidecar is a relocatable object with no `.note.gnu.build-id` of its
own, so nothing on the sidecar side says which binary it belongs to. The
release publishes that pairing instead: a `BUILD-IDS.txt` asset, one line per
shipped Linux binary whose `.dwp` is attached, in `sha256sum`-like shape::

    <gnu-build-id-hex>  <triple>

`clud symbols install` (crates/clud-bin/src/symbols.rs) compares a crash
report's `build_id` against the line for its triple and only fetches the
`.dwp` on an exact match.

Two halves, run in different jobs:

- `record` runs in the per-triple build job, right after that job staged its
  `.dwp`. It reads the id from the *shipped* binary -- the `clud` entry inside
  the final wheel, after every post-link rewrite -- and writes a one-line
  fragment `clud-<triple>.build-id` beside the `.dwp` in `dist-debuginfo/`, so
  it travels in the same `debuginfo-<triple>` artifact.
- `combine` runs in the release job, which is the only place every triple's
  fragments meet. It folds them into `BUILD-IDS.txt` and removes the
  fragments, before `SHA256SUMS` is generated so the manifest covers it.

The ELF note is parsed here directly rather than by shelling out to
`readelf`: the release job's runner need not have binutils for the target's
architecture, and the parse mirrors `crates/clud-bin/src/build_id.rs`, which
is what produces the id a crash report records.
"""

from __future__ import annotations

import argparse
import struct
import sys
import zipfile
from pathlib import Path

#: The release asset the fetcher reads. Must match `BUILD_IDS_ASSET` in
#: crates/clud-bin/src/symbols.rs.
BUILD_IDS_ASSET = "BUILD-IDS.txt"

#: Per-triple fragment suffix staged beside each `.dwp`.
FRAGMENT_SUFFIX = ".build-id"

_PT_NOTE = 4
_NT_GNU_BUILD_ID = 3
_MAX_NOTE_BYTES = 64 * 1024


def _align4(value: int) -> int:
    return (value + 3) & ~3


def _build_id_from_notes(notes: bytes) -> str | None:
    pos = 0
    while pos + 12 <= len(notes):
        namesz, descsz, ntype = struct.unpack_from("<III", notes, pos)
        name_at = pos + 12
        desc_at = name_at + _align4(namesz)
        nxt = desc_at + _align4(descsz)
        if ntype == _NT_GNU_BUILD_ID and notes[name_at : name_at + namesz] == b"GNU\0":
            desc = notes[desc_at : desc_at + descsz]
            if len(desc) != descsz or not desc:
                return None
            return desc.hex()
        if nxt <= pos:
            return None
        pos = nxt
    return None


def read_gnu_build_id(data: bytes) -> str | None:  # noqa: C901
    """The `NT_GNU_BUILD_ID` note of an ELF image as lowercase hex, or None.

    Walks `PT_NOTE` program headers, the same view the loader and
    `build_id.rs` use. Little-endian only, like every ELF triple clud ships.
    Any malformed input yields None rather than raising.
    """
    if len(data) < 16 or data[:4] != b"\x7fELF" or data[5] != 1:
        return None
    try:
        if data[4] == 2:
            (phoff,) = struct.unpack_from("<Q", data, 0x20)
            phentsize, phnum = struct.unpack_from("<HH", data, 0x36)
            is_64 = True
        elif data[4] == 1:
            (phoff,) = struct.unpack_from("<I", data, 0x1C)
            phentsize, phnum = struct.unpack_from("<HH", data, 0x2A)
            is_64 = False
        else:
            return None
        if phentsize == 0:
            return None
        for index in range(phnum):
            entry = phoff + index * phentsize
            (p_type,) = struct.unpack_from("<I", data, entry)
            if p_type != _PT_NOTE:
                continue
            if is_64:
                (offset,) = struct.unpack_from("<Q", data, entry + 8)
                (size,) = struct.unpack_from("<Q", data, entry + 32)
            else:
                (offset,) = struct.unpack_from("<I", data, entry + 4)
                (size,) = struct.unpack_from("<I", data, entry + 16)
            if size == 0 or size > _MAX_NOTE_BYTES or offset + size > len(data):
                continue
            found = _build_id_from_notes(data[offset : offset + size])
            if found:
                return found
    except struct.error:
        return None
    return None


def wheel_binary_build_id(wheel: Path, name: str = "clud") -> str | None:
    """Build-id of the `.data/scripts/<name>` ELF shipped inside `wheel`."""
    with zipfile.ZipFile(wheel) as archive:
        for info in archive.infolist():
            if info.filename.endswith(f".data/scripts/{name}"):
                return read_gnu_build_id(archive.read(info))
    return None


def fragment_line(build_id: str, target: str) -> str:
    return f"{build_id.lower()}  {target}\n"


def record(target: str, wheels: list[Path], debuginfo_dir: Path) -> Path | None:
    """Stage `clud-<target>.build-id` for the shipped binary in `wheels`.

    Returns the fragment path, or None (with a warning) when no shipped binary
    carries an id. Best-effort like the `.dwp` itself: a missing entry only
    means `clud symbols install` declines to fetch that triple's sidecar.
    """
    for wheel in wheels:
        build_id = wheel_binary_build_id(wheel)
        if build_id:
            debuginfo_dir.mkdir(parents=True, exist_ok=True)
            fragment = debuginfo_dir / f"clud-{target}{FRAGMENT_SUFFIX}"
            fragment.write_text(fragment_line(build_id, target), encoding="utf-8")
            print(f"build-id: {build_id}  {target} ({fragment})", flush=True)
            return fragment
    print(
        f"::warning::no GNU build-id in the shipped clud for {target}; "
        f"its .dwp will be published without a {BUILD_IDS_ASSET} entry",
        flush=True,
    )
    return None


def combine(debuginfo_dir: Path) -> Path | None:
    """Fold every `*.build-id` fragment into `BUILD-IDS.txt`, sorted by triple.

    The fragments are removed so they are not attached as release assets of
    their own. Returns None when there were no fragments (no `.dwp` shipped).
    """
    fragments = sorted(debuginfo_dir.glob(f"*{FRAGMENT_SUFFIX}"))
    if not fragments:
        return None
    lines: list[tuple[str, str]] = []
    for fragment in fragments:
        for raw in fragment.read_text(encoding="utf-8").splitlines():
            parts = raw.split()
            if len(parts) != 2:
                raise ValueError(f"malformed build-id fragment {fragment}: {raw!r}")
            lines.append((parts[1], parts[0].lower()))
    triples = [triple for triple, _ in lines]
    duplicates = sorted({triple for triple in triples if triples.count(triple) > 1})
    if duplicates:
        raise ValueError(f"more than one build-id for {', '.join(duplicates)}")
    out = debuginfo_dir / BUILD_IDS_ASSET
    out.write_text(
        "".join(fragment_line(build_id, triple) for triple, build_id in sorted(lines)),
        encoding="utf-8",
    )
    for fragment in fragments:
        fragment.unlink()
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m ci.build_ids", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    combine_parser = sub.add_parser("combine", help="merge fragments into BUILD-IDS.txt")
    combine_parser.add_argument("debuginfo_dir", type=Path)
    args = parser.parse_args(argv)
    if args.command == "combine":
        out = combine(args.debuginfo_dir)
        if out is None:
            print(f"no build-id fragments under {args.debuginfo_dir}", flush=True)
        else:
            print(out.read_text(encoding="utf-8"), end="", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
