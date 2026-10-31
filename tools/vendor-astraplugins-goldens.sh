#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Vendors AstraPlugins' game-integration goldens into tests/goldens/astraplugins/: every file of
# testdata/games/, plus spec/games/reserved.yaml and spec/games/schemas/*.json, exactly as the
# commit holds them (read out of git with `git archive`, so the checkout's working tree, dirty or
# not, plays no part), then writes PIN (that commit), NOTICE (where the files come from, and their
# licence) and SHA256SUMS (every file but SHA256SUMS itself, `sha256sum` format, sorted by path).
#
#   tools/vendor-astraplugins-goldens.sh <AstraPlugins checkout> [<commit>]
#
# <commit> defaults to the checkout's HEAD; when given, HEAD must be that commit, so a re-pin
# names what it pins. A re-pin is this one command: tests/goldens.rs reads the PIN and the sums,
# `gi-probe` reads the PIN, and every known disagreement with the goldens is one list,
# SPEC_DEFECTS_AT_PIN (tests/common/spec_defects.rs), which a fixed golden turns red until its
# entry goes. Never edit a vendored file by hand: the sums would say so.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/tests/goldens/astraplugins"

die() {
  printf 'vendor-astraplugins-goldens: %s\n' "$*" >&2
  exit 1
}

[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: $0 <AstraPlugins checkout> [<commit>]"
co="$1"
git -C "$co" rev-parse --git-dir >/dev/null 2>&1 || die "$co is not a git checkout"
head="$(git -C "$co" rev-parse --verify 'HEAD^{commit}')"
if [ $# -eq 2 ]; then
  want="$(git -C "$co" rev-parse --verify "$2^{commit}")" || die "no commit $2 in $co"
  [ "$head" = "$want" ] || die "$co is at $head, not $want: check the commit out first"
fi
for p in testdata/games spec/games/reserved.yaml spec/games/schemas; do
  git -C "$co" cat-file -e "$head:$p" 2>/dev/null || die "$head has no $p"
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
git -C "$co" archive --format=tar "$head" testdata/games spec/games/reserved.yaml |
  tar -x -C "$tmp"
# Only the schemas' JSON files.
mapfile -t schemas < <(git -C "$co" ls-tree --name-only "$head" spec/games/schemas/ | grep '\.json$')
[ "${#schemas[@]}" -gt 0 ] || die "$head has no spec/games/schemas/*.json"
git -C "$co" archive --format=tar "$head" "${schemas[@]}" | tar -x -C "$tmp"

rm -rf "$dest"
mkdir -p "$dest"
cp -a "$tmp/testdata" "$tmp/spec" "$dest/"
printf '%s\n' "$head" >"$dest/PIN"
cat >"$dest/NOTICE" <<EOF
Copied by tools/vendor-astraplugins-goldens.sh from mihailinl/AstraPlugins at commit $head:
testdata/games/, spec/games/reserved.yaml and spec/games/schemas/*.json, byte for byte.
AstraPlugins licenses spec/ and testdata/ under GPL-3.0-or-later (its LICENSE); these files keep
that licence. They are test data this crate's tests read, never part of the launcher's
MPL-2.0 sources or of anything it builds.
EOF
(
  cd "$dest"
  find . -type f ! -name SHA256SUMS -printf '%P\n' | LC_ALL=C sort | while IFS= read -r f; do
    sha256sum -- "$f"
  done >SHA256SUMS
)
printf 'vendored AstraPlugins %s into %s (%s files)\n' "$head" "${dest#"$root"/}" \
  "$(wc -l <"$dest/SHA256SUMS")"
