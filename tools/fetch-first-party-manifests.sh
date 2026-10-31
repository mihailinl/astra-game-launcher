#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Refreshes tests/fixtures/first-party/: for every source Astra 0.2.7 compiles in
# (src/bin/gi-probe/compat-0.2.7.json), the astra-gi.toml of the release 0.2.7 would install
# TODAY, verbatim below a header naming it. tests/gi_probe.rs holds each to launcher 0.3.1's
# parser and to Astra 0.2.7's own check of its source ("0.3.1 parses every first-party
# manifest").
#
# Which release: Astra 0.2.7's own choice, `gi-probe pick` (the replica of releases::pick_with),
# over the repository's FULL release listing, asked by its numeric id as 0.2.7 asks it — the
# newest STABLE release by semver whose tag is the source's prefix, never GitHub's "latest"
# (astra-reshade's v0.2.2 … v0.2.11 are prereleases, so 0.2.7 installs v0.2.1). A source with
# no such release falls back to its compiled pin, as 0.2.7 does offline; one with neither
# (astra-gta-trilogy's three) gets no <id>.astra-gi.toml. The newest release a prerelease-
# following owner test build would take is written too, as <id>.pre.astra-gi.toml, where it is
# another release.
#
# Every zip's digest is checked against GitHub's own asset digest (or the pin's) before its
# manifest is read. Needs `gh` (signed in), `jq`, `unzip`, `sha256sum` and cargo. Writes nothing
# but tests/fixtures/first-party/, and its scratch files under TMPDIR.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
table="$root/src/bin/gi-probe/compat-0.2.7.json"
out="$root/tests/fixtures/first-party"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out"

die() {
  printf 'fetch-first-party-manifests: %s\n' "$*" >&2
  exit 1
}

cargo build --quiet --manifest-path "$root/Cargo.toml" --features gi-probe --bin gi-probe
probe="$(cargo metadata --quiet --format-version 1 --manifest-path "$root/Cargo.toml" --no-deps |
  jq -r .target_directory)/debug/gi-probe"
[[ -x "$probe" ]] || die "no gi-probe at $probe"

# The repository's whole release listing, every page, as one array (once per repository).
listing_of() {
  local repo_id="$1" file="$tmp/listing-$1.json"
  if [[ ! -f "$file" ]]; then
    gh api --paginate "repositories/$repo_id/releases?per_page=100" --jq '.[]' | jq -s . >"$file"
  fi
  printf '%s\n' "$file"
}

# fetch <id> <repo> <tag> <asset> <sha256> <chosen> <github-prerelease> <dest>
fetch() {
  local id="$1" repo="$2" tag="$3" asset="$4" sha="$5" chosen="$6" pre="$7" dest="$8"
  local dir="$tmp/$id-$tag"
  mkdir -p "$dir"
  gh release download "$tag" -R "$repo" -p "$asset" -D "$dir" --clobber
  local got
  got="$(sha256sum "$dir/$asset" | cut -d' ' -f1)"
  [[ "$got" == "$sha" ]] || die "$id $tag: $asset is $got, not the $chosen digest $sha"
  {
    printf '# gi-probe first-party fixture (tools/fetch-first-party-manifests.sh): the release'"'"'s astra-gi.toml, verbatim below.\n'
    printf '# source: %s\n' "$id"
    printf '# repo: %s\n' "$repo"
    printf '# tag: %s\n' "$tag"
    printf '# asset: %s\n' "$asset"
    printf '# sha256: %s\n' "$sha"
    printf '# chosen: %s\n' "$chosen"
    printf '# github-prerelease: %s\n' "$pre"
    printf '# --- verbatim below ---\n'
    unzip -p "$dir/$asset" astra-gi.toml
  } >"$dest"
  printf '%s: %s %s (%s, %s…)\n' "$id" "$tag" "$asset" "$chosen" "${sha:0:16}"
}

for id in $(jq -r '.sources[].id' "$table"); do
  row="$(jq -c --arg id "$id" '.sources[] | select(.id == $id)' "$table")"
  repo="$(jq -r .repo <<<"$row")"
  repo_id="$(jq -r .repo_id <<<"$row")"
  asset="$(jq -r .asset <<<"$row")"
  listing="$(listing_of "$repo_id")"

  stable="$("$probe" pick --source "$id" --listing "$listing" || true)"
  tag="$(jq -r '.release.tag // empty' <<<"$stable")"
  sha="$(jq -r '.release.sha256 // empty' <<<"$stable")"
  chosen=release
  pre=false
  if [[ -z "$tag" ]]; then
    tag="$(jq -r '.fallback.tag // empty' <<<"$stable")"
    sha="$(jq -r '.fallback.sha256 // empty' <<<"$stable")"
    pre="$(jq -r '.fallback.prerelease // false' <<<"$stable")"
    chosen=fallback-pin
  fi
  if [[ -n "$tag" ]]; then
    fetch "$id" "$repo" "$tag" "$asset" "$sha" "$chosen" "$pre" "$out/$id.astra-gi.toml"
  else
    rm -f "$out/$id.astra-gi.toml"
    printf '%s: no stable release carries %s, and the source has no pin\n' "$id" "$asset"
  fi

  newest="$("$probe" pick --source "$id" --listing "$listing" --prerelease || true)"
  ptag="$(jq -r '.release.tag // empty' <<<"$newest")"
  if [[ -n "$ptag" && "$ptag" != "$tag" ]]; then
    psha="$(jq -r '.release.sha256' <<<"$newest")"
    ppre="$(jq -r --arg t "$ptag" '.[] | select(.tag_name == $t) | .prerelease' "$listing")"
    fetch "$id" "$repo" "$ptag" "$asset" "$psha" prerelease "$ppre" "$out/$id.pre.astra-gi.toml"
  else
    rm -f "$out/$id.pre.astra-gi.toml"
  fi
done
