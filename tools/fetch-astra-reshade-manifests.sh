#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Refreshes tests/fixtures/astra-reshade-unreal/: the astra-gi.toml of every Unreal runner release
# astra-reshade published (v0.2.0 … v0.2.11), verbatim from its GitHub release asset astra-gi.zip.
# They are the NAMED source of src/former.rs's digests (UE4SS's dwmapi.dll, ReShade's dxgi.dll,
# our astra.addon64 per release): a unit test reads them and checks every constant against them.
# Needs `gh` (signed in) and `unzip`. Writes nothing else.
set -euo pipefail
repo=mihailinl/astra-reshade
out="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/tests/fixtures/astra-reshade-unreal"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out"
for v in 0.2.0 0.2.1 0.2.2 0.2.3 0.2.4 0.2.5 0.2.6 0.2.7 0.2.8 0.2.9 0.2.10 0.2.11; do
  gh release download "v$v" -R "$repo" -p astra-gi.zip -D "$tmp/v$v" --clobber
  unzip -p "$tmp/v$v/astra-gi.zip" astra-gi.toml > "$out/v$v.astra-gi.toml"
  echo "v$v: $(sha256sum "$tmp/v$v/astra-gi.zip" | cut -c1-16)…"
done
