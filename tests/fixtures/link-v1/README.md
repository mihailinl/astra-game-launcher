# Astra Link v1 manifest goldens, ahead of the pin

Copied byte for byte from mihailinl/AstraPlugins at commit
`1ba3fcbc7f0238651890660ca7862d23060479c4` (`testdata/games/manifests/…`, the same relative
paths: every `link-*` golden and `game-specific-link`), GPL-3.0-or-later like the rest of that repository's testdata.

They are here, and not in `tests/goldens/astraplugins/`, because a re-pin of that copy to this
commit also brings content goldens the launcher does not answer yet (`E_CONTENT_AVATAR_IN_PACK`,
`F_CONTENT_LICENCE_DECLARED`). `tests/link_v1.rs` checks `SHA256SUMS` and holds `Manifest::lint`
and `link_digest` to them. When the vendored copy is re-pinned at or past this commit, delete
this folder and point `tests/link_v1.rs` at the vendored files.
