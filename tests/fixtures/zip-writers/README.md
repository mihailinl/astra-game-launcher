<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.
-->

# One integration, packed by every common ZIP writer

`make.sh` packs one small game integration (`astra-gi.toml`, `payload/a.txt`, `payload/café.txt`,
an empty `payload/sub/empty.txt`, 4 KiB of zeros) with each writer a release script reaches for.
`tests/zip_consistency.rs` opens every one with the launcher: what `src/zip_consistency.rs`
refuses is a descriptor or a stream that lies, never one that is there.

| file | writer | data descriptors (bit 3) |
|---|---|---|
| `infozip.zip` | Info-ZIP `zip` to a file | none |
| `infozip-pipe.zip` | Info-ZIP `zip -` into a pipe | on every deflated entry (and on the stored empty one) |
| `infozip-pipe-stored.zip` | Info-ZIP `zip -0 -` into a pipe | on every stored entry |
| `python.zip` | Python `zipfile` to a file, deflated | none |
| `python-stream.zip` | Python `zipfile` to a pipe, deflated | on every entry |
| `python-stream-stored.zip` | Python `zipfile` to a pipe, stored | on every entry |
| `bsdtar.zip` | `bsdtar --format zip` | on every entry |
| `7zip.zip` | 7-Zip | none |
| `jar.zip` | `jar --create` | on every deflated entry |

Written 2026-11-01 with Info-ZIP Zip 3.0, Python 3.14.8, bsdtar 3.8.10 (libarchive 3.8.10),
7-Zip 26.05 and jar 27. Every non-ASCII name is flagged UTF-8 (general-purpose bit 11) by each.
`SHA256SUMS` is the digests `make.sh` wrote; a new run with other tool versions writes other bytes.

Info-ZIP's first-party releases (`zip -qr - . > astra-gi.zip`) write to a FILE through the shell's
redirection, which is seekable, so they carry no descriptors: `infozip.zip`'s shape.
