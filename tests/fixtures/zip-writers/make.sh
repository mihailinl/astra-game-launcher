#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Writes this folder's zips: one small game integration (an astra-gi.toml and a payload/ folder)
# packed by each common ZIP writer, the ways a release script runs them — Info-ZIP to a file and to
# a pipe (deflated, and stored with -0), Python's zipfile to a file and to a pipe (deflated and
# stored), bsdtar, 7-Zip and jar. To a pipe, Info-ZIP and Python write every entry with a data
# descriptor (general-purpose bit 3): what `src/zip_consistency.rs` must still accept.
#
#   tests/fixtures/zip-writers/make.sh
#
# Run by hand, rarely: the zips are committed, and tests/zip_consistency.rs reads those. The bytes
# depend on the tools' versions (the README names the ones used), not on the clock: every file's
# time is fixed, and TZ is UTC.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Never /tmp (RAM on some desks).
tmp="$(mktemp -d -p "${TMPDIR:-$here}" zip-writers.XXXXXX)"
trap 'rm -rf "$tmp"' EXIT
export TZ=UTC LC_ALL=C.UTF-8

tree="$tmp/tree"
mkdir -p "$tree/payload/sub"
cat >"$tree/astra-gi.toml" <<'EOF'
schema = 1

[integration]
id = "test.zip-writers"
name = "Zip writers"
version = "1.0.0"
authors = ["tester"]
license = "MIT"

[target]
engine = "unity-mono"
exe = "*"
platforms = ["windows-x64"]
anti_cheat = "none"

[[files]]
from = "payload/**"
to = "${profile}/payload/"
EOF
printf 'hello\n' >"$tree/payload/a.txt"
printf 'une café\n' >"$tree/payload/café.txt"
: >"$tree/payload/sub/empty.txt"
# Compresses well, so the deflated and stored writers differ.
head -c 4096 /dev/zero >"$tree/payload/sub/zeros.bin"
find "$tree" -exec touch -h -d '2020-01-01 00:00:00' {} +

cd "$tree"
files=(astra-gi.toml payload/a.txt payload/café.txt payload/sub/empty.txt payload/sub/zeros.bin)

rm -f "$here"/*.zip
zip -qX "$here/infozip.zip" "${files[@]}"
zip -qX - "${files[@]}" | cat >"$here/infozip-pipe.zip"
zip -qX0 - "${files[@]}" | cat >"$here/infozip-pipe-stored.zip"
python3 - "$here" "${files[@]}" <<'EOF'
import io, sys, zipfile

out, names = sys.argv[1], sys.argv[2:]

class Pipe(io.RawIOBase):
    """What zipfile sees writing to a pipe: no seek, no tell."""
    def __init__(self):
        self.b = bytearray()
    def writable(self):
        return True
    def write(self, data):
        self.b += data
        return len(data)

def pack(target, method):
    with zipfile.ZipFile(target, "w", method) as z:
        for n in names:
            z.write(n)

pack(f"{out}/python.zip", zipfile.ZIP_DEFLATED)
for name, method in [("python-stream.zip", zipfile.ZIP_DEFLATED), ("python-stream-stored.zip", zipfile.ZIP_STORED)]:
    p = Pipe()
    pack(p, method)
    open(f"{out}/{name}", "wb").write(p.b)
EOF
bsdtar --format zip -cf "$here/bsdtar.zip" "${files[@]}"
7z a -tzip -bso0 -bsp0 "$here/7zip.zip" "${files[@]}"
jar --create --no-manifest --file "$here/jar.zip" "${files[@]}"

cd "$here"
sha256sum -- *.zip >SHA256SUMS
printf 'wrote %s zips into %s\n' "$(ls -1 *.zip | wc -l)" "$here"
