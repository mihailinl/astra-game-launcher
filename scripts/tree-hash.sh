#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Recomputes SOURCE_TREE_SHA256 exactly the way build.rs does (see its doc comment), over the
# directory given as $1 (default: this script's own repo root). One implementation shared by:
#   - `cargo test` (tests/source_tree_hash.rs), to prove build.rs and this script agree on THIS
#     checkout;
#   - `scripts/release.sh`, against a `git archive` extraction, before it builds and tags;
#   - the README's recompute instructions, which are this script's own commands written out so
#     someone with only a tarball (no clone, no script) can still check a release by hand.
#
# Algorithm: every regular file under the directory, excluding any file or directory named
# `.git`, `.cargo-ok` or `target` at any depth (nothing else excluded); for each file, sorted by
# its `/`-separated relative path as raw bytes (`LC_ALL=C sort`), `sha256sum` already prints
# `<hex>  <path>` (two spaces) in exactly the line format build.rs hashes; the tree hash is
# `sha256sum` of that sorted list of lines.
set -euo pipefail

dir="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

if ! command -v sha256sum >/dev/null 2>&1; then
  echo "tree-hash.sh: sha256sum is not on PATH" >&2
  exit 3
fi

cd "$dir"

find . -mindepth 1 \
    \( -name .git -o -name .cargo-ok -o -name target \) -prune -o \
    -type f -print \
  | sed 's|^\./||' \
  | LC_ALL=C sort \
  | while IFS= read -r f; do sha256sum -- "$f"; done \
  | sha256sum \
  | cut -d' ' -f1
