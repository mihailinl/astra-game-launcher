#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Cuts a verifiable release: a tag, a source tarball built from that tag (not the working
# tree), and a GitHub release that publishes the tarball's SHA-256 alongside the SOURCE TREE
# SHA-256 this crate's own `build.rs` computes — the same hash Astra's build of its pinned git
# commit reports via `--version`, so "what Astra compiled in" and "what this release published"
# can be compared by anyone, from nothing but a clone and `sha256sum`.
#
# Usage:
#   scripts/release.sh <version> [--dry-run]
#
# --dry-run runs every check for real (clean tree, HEAD == origin/main, Cargo.toml's version,
# cargo test, cargo clippy, and the archive-vs-build hash self-consistency proof below) and only
# PRINTS the writes (tag, tarball, checksum files, push, GitHub release) instead of making them.
#
# What a real run does, in order:
#   1. Refuse unless the working tree is clean (untracked files count).
#   2. Refuse unless HEAD is origin/main (fetched fresh).
#   3. Refuse unless Cargo.toml's [package] version equals <version>.
#   4. cargo test, cargo clippy --all-targets -D warnings — both must pass.
#   5. Extract `git archive HEAD` into a scratch directory (never the working tree, which could
#      disagree with HEAD in ways `git status` doesn't show, e.g. a dirty mtime-only rebuild).
#      Recompute its tree hash two independent ways — scripts/tree-hash.sh's shell algorithm, and
#      `cargo build --locked` + `--version` (build.rs's compiled-in algorithm) — and refuse if
#      they disagree. This IS the proof that build.rs and the shell recompute are the same
#      algorithm, run fresh, on exactly the bytes about to be tagged.
#   6. Create annotated tag v<version>; `git archive --prefix=astra-game-launcher-<version>/` it
#      into a source tarball; re-extract THAT tarball and recompute its hash a third time, to
#      prove the --prefix wrapping didn't change what a downloader would get; write SHA256SUMS
#      and SOURCE_TREE_SHA256.txt; push the tag; `gh release create` with the tarball and both
#      checksum files attached, and notes naming the commit, the hash, and how to recompute it.
#
# No binaries are built or attached — source only. That is a deliberate scope cut, not an
# oversight: see the module doc of this file's caller-facing sibling, build.rs.
set -euo pipefail

REPO_SLUG="mihailinl/astra-game-launcher"
REPO_URL="https://github.com/$REPO_SLUG"

say() { printf '%s\n' "$*" >&2; }
die() {
  printf 'release.sh: refusing: %s\n' "$*" >&2
  exit 1
}

DRY_RUN=false
VERSION=""
for arg in "$@"; do
  case "$arg" in
  --dry-run) DRY_RUN=true ;;
  -h | --help)
    sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  -*) die "unknown flag: $arg" ;;
  *)
    if [[ -n "$VERSION" ]]; then die "one version argument only (got '$VERSION' and '$arg')"; fi
    VERSION="$arg"
    ;;
  esac
done
[[ -n "$VERSION" ]] || die "usage: scripts/release.sh <version> [--dry-run]"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z (got '$VERSION')"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TAG="v$VERSION"
ARTIFACT_DIR="$ROOT/dist/$VERSION"
TARBALL_NAME="astra-game-launcher-$VERSION-src.tar.gz"

if $DRY_RUN; then
  say "[dry-run] every write below is PRINTED, not performed; every check above it runs for real."
fi

# ---------------------------------------------------------------------------
# 1. Clean tree (untracked files count).
# ---------------------------------------------------------------------------
dirty="$(git status --porcelain)"
[[ -z "$dirty" ]] || die $'working tree is not clean:\n'"$dirty"
say "ok: working tree is clean"

# ---------------------------------------------------------------------------
# 2. HEAD is origin/main.
# ---------------------------------------------------------------------------
git fetch --quiet origin main
head_sha="$(git rev-parse HEAD)"
main_sha="$(git rev-parse origin/main)"
[[ "$head_sha" == "$main_sha" ]] ||
  die "HEAD ($head_sha) is not origin/main ($main_sha) — fetch, merge or push first"
say "ok: HEAD is origin/main ($head_sha)"

# A tag for this version must not already exist, locally or on the remote.
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  die "tag $TAG already exists locally"
fi
if [[ -n "$(git ls-remote --tags origin "refs/tags/$TAG")" ]]; then
  die "tag $TAG already exists on origin"
fi

# ---------------------------------------------------------------------------
# 3. Cargo.toml's [package] version.
# ---------------------------------------------------------------------------
toml_version="$(awk '
  /^\[/{ in_pkg = ($0 == "[package]") }
  in_pkg && /^version[[:space:]]*=/ {
    match($0, /"[^"]*"/)
    print substr($0, RSTART+1, RLENGTH-2)
    exit
  }
' Cargo.toml)"
[[ "$toml_version" == "$VERSION" ]] ||
  die "Cargo.toml's [package] version is '$toml_version', not '$VERSION' — bump it first"
say "ok: Cargo.toml version is $VERSION"

# ---------------------------------------------------------------------------
# 4. cargo test, cargo clippy.
# ---------------------------------------------------------------------------
say "running: cargo test"
cargo test --quiet
say "ok: cargo test"

say "running: cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets --quiet -- -D warnings
say "ok: cargo clippy (0 warnings)"

# ---------------------------------------------------------------------------
# 5. Fresh `git archive HEAD` extraction: prove the shell recompute and build.rs's compiled-in
#    algorithm agree, on exactly HEAD's tree, independent of anything the working tree's target/
#    directory or mtimes might be caching.
# ---------------------------------------------------------------------------
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

head_extract="$SCRATCH/head-extract"
mkdir -p "$head_extract"
git archive HEAD | tar -x -C "$head_extract"

say "recomputing the tree hash from a fresh HEAD extraction (shell algorithm)"
hash_shell="$("$ROOT/scripts/tree-hash.sh" "$head_extract")"

say "building that same extraction and reading its --version (compiled algorithm)"
extract_target="$head_extract/target"
( cd "$head_extract" && CARGO_TARGET_DIR="$extract_target" cargo build --locked --quiet )
version_out="$("$extract_target/debug/astra-game-launcher" --version)"
hash_binary="$(printf '%s' "$version_out" | sed -n 's/.*sha256 \([0-9a-f]\{64\}\)).*/\1/p')"
[[ -n "$hash_binary" ]] || die "could not parse a sha256 out of: $version_out"

[[ "$hash_shell" == "$hash_binary" ]] ||
  die "tree hash disagreement on a fresh HEAD extraction: shell=$hash_shell binary=$hash_binary \
(scripts/tree-hash.sh and build.rs are supposed to implement the same algorithm)"
SOURCE_TREE_SHA256="$hash_shell"
say "ok: source tree sha256 is $SOURCE_TREE_SHA256 (shell recompute and build.rs agree)"

# ---------------------------------------------------------------------------
# 6. Tag, tarball, checksums, push, GitHub release.
# ---------------------------------------------------------------------------
tag_message="astra-game-launcher $TAG

commit: $head_sha
source tree sha256: $SOURCE_TREE_SHA256"

if $DRY_RUN; then
  say "[dry-run] would create annotated tag $TAG:"
  say "$tag_message" | sed 's/^/    /'
else
  git tag -a "$TAG" -m "$tag_message" "$head_sha"
  say "ok: created annotated tag $TAG"
fi

archive_ref="$TAG"
$DRY_RUN && archive_ref="$head_sha" # the tag doesn't exist yet in a dry run

if $DRY_RUN; then
  say "[dry-run] would run: git archive --prefix=astra-game-launcher-$VERSION/ -o $ARTIFACT_DIR/$TARBALL_NAME $TAG"
  say "[dry-run] would write $ARTIFACT_DIR/SHA256SUMS (the tarball's own sha256sum line)"
  say "[dry-run] would write $ARTIFACT_DIR/SOURCE_TREE_SHA256.txt:"
  say "    $SOURCE_TREE_SHA256"
else
  mkdir -p "$ARTIFACT_DIR"
  git archive --prefix="astra-game-launcher-$VERSION/" -o "$ARTIFACT_DIR/$TARBALL_NAME" "$archive_ref"
  say "ok: wrote $ARTIFACT_DIR/$TARBALL_NAME"

  # Re-extract the shipped tarball itself and recompute a third time: proves the --prefix
  # wrapping a downloader sees doesn't change the published hash.
  reextract="$SCRATCH/reextract"
  mkdir -p "$reextract"
  tar -xzf "$ARTIFACT_DIR/$TARBALL_NAME" -C "$reextract"
  hash_tarball="$("$ROOT/scripts/tree-hash.sh" "$reextract/astra-game-launcher-$VERSION")"
  [[ "$hash_tarball" == "$SOURCE_TREE_SHA256" ]] ||
    die "the shipped tarball's own tree hash ($hash_tarball) disagrees with $SOURCE_TREE_SHA256"
  say "ok: the shipped tarball reproduces $SOURCE_TREE_SHA256"

  (cd "$ARTIFACT_DIR" && sha256sum "$TARBALL_NAME" >SHA256SUMS)
  say "ok: wrote $ARTIFACT_DIR/SHA256SUMS"

  printf '%s\n' "$SOURCE_TREE_SHA256" >"$ARTIFACT_DIR/SOURCE_TREE_SHA256.txt"
  say "ok: wrote $ARTIFACT_DIR/SOURCE_TREE_SHA256.txt"
fi

if $DRY_RUN; then
  say "[dry-run] would run: git push origin $TAG"
else
  git push origin "$TAG"
  say "ok: pushed $TAG"
fi

notes_file="$SCRATCH/release-notes.md"
cat >"$notes_file" <<EOF_NOTES
**Commit:** \`$head_sha\`
**Source tree sha256:** \`$SOURCE_TREE_SHA256\`

Astra pins this crate by git rev in \`astra-rs/astra-daemon/Cargo.toml\`; the Astra release
notes / About screen show this same source-tree hash for whatever commit it pinned, so what
Astra compiled in can be checked against what this release built from source — they're both
a hash of the source tree, never a binary, since the crate is compiled into the daemon rather
than shipped on its own.

Recompute it yourself from this tag (needs \`git\` and \`sha256sum\`):

\`\`\`bash
git clone --quiet --depth 1 --branch $TAG $REPO_URL /tmp/astra-gl-verify-src && \\
cd /tmp/astra-gl-verify-src && git archive $TAG | tar -x -C /tmp/astra-gl-verify && \\
cd /tmp/astra-gl-verify && \\
find . -mindepth 1 \\( -name .git -o -name .cargo-ok -o -name target \\) -prune -o -type f -print \\
  | sed 's|^\\./||' | LC_ALL=C sort \\
  | while IFS= read -r f; do sha256sum -- "\$f"; done | sha256sum | cut -d' ' -f1
\`\`\`

Or run \`scripts/tree-hash.sh\` against a checkout of this tag — see the README's
"Verifying a release against what Astra compiles in" section.

**Assets:**
- \`$TARBALL_NAME\` — the exact source tree this hash covers (\`git archive $TAG\`, prefixed
  \`astra-game-launcher-$VERSION/\`)
- \`SHA256SUMS\` — the tarball's own sha256
- \`SOURCE_TREE_SHA256.txt\` — just the hex hash above

No binaries are attached to this release — source only.
EOF_NOTES

if $DRY_RUN; then
  say "[dry-run] would run: gh release create $TAG --repo $REPO_SLUG --title \"$TAG\" --notes-file <notes> $ARTIFACT_DIR/$TARBALL_NAME $ARTIFACT_DIR/SHA256SUMS $ARTIFACT_DIR/SOURCE_TREE_SHA256.txt"
  say "[dry-run] notes would read:"
  sed 's/^/    /' "$notes_file"
else
  gh release create "$TAG" \
    --repo "$REPO_SLUG" \
    --title "$TAG" \
    --notes-file "$notes_file" \
    "$ARTIFACT_DIR/$TARBALL_NAME" \
    "$ARTIFACT_DIR/SHA256SUMS" \
    "$ARTIFACT_DIR/SOURCE_TREE_SHA256.txt"
  say "ok: published $REPO_URL/releases/tag/$TAG"
fi

say "done."
