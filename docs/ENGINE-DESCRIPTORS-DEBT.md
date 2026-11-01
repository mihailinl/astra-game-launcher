# Engine descriptors: what L1, L2 and L3 so far leave open

This lists what launcher 0.5.0's L1 (engine descriptors and Pass B), L2 (the built-in rules as
descriptors, the 0.4.1 detector deleted) and the L3 steps committed so far (C1b over installs,
`gi-probe compat` and `validate-descriptor`, the `[integration.*]` tables and `gi-probe check`,
planning that asks the descriptors: `plan_install_in`) knowingly do not do yet. Each row gives the step that owes it and the test or line that pins it.
A row comes off this file only in the commit that closes it.

| # | What | Why it is not done | Owed by | Pinned by |
|---|---|---|---|---|
| 1 | **C1 has no recordings.** Plan B §2's C1 also replays `bin/list-shape` recordings of real game folders (`tests/corpus/recorded/`). The 20,000-tree corpus, `tests/corpus/divergence.txt` and running 0.4.1 in a child process with a timeout are done (L2: `tests/divergence.rs`). | A recording is a capture of a real Steam library's game folders: names, kinds, sizes, and the few bytes the built-ins read. L2 reads no real game folder, and the capture tool is L3's `gi-probe capture`. | L3 (the tool), then the owner (capturing his libraries) | `tests/identity.rs` module doc |
| 3 | **One branch of the Windows reads has no fixture: a reparse point that is no link, that a filter leaves visible and serves.** `src/engines/reads.rs` opens with `FILE_FLAG_OPEN_REPARSE_POINT`; a name surrogate (a symbolic link, a junction) is refused, and any other reparse point is opened again the ordinary way and read only when it is the same file (volume, index), as 0.4.1's plain open read it. `reads::windows::tests` run on the Windows VM (2026-10-31, Windows 11, elevated session; `cargo test --no-default-features --lib engines::reads`): a symbolic link to a file and a junction read nothing; an app execution alias (`0x8000001B`) does not open again and reads nothing; a file compressed by `compact /exe` reads as its bytes — WOF hides its reparse point while its filter is attached (`fsutil reparsepoint query`: not a reparse point), so that is the ordinary path, and a build that refuses every visible reparse point passes all three too. The re-opened read itself (a deduplicated file, a cloud placeholder its sync engine shows as one) is reached by no fixture. | It needs a Data Deduplication volume (Windows Server) or a registered cloud sync root. | before 0.5.0 is tagged | `src/engines/reads.rs` `windows::disk_file` |
| 4 | **One lookup pool per game, shared by every standing.** Descriptors whose anchors the game does hold can drain it. That wipes Pass B's answer for their standing and every one below it, so it also breaks developer mode's "Test on this game". Plan B §3 says only "that descriptor yields nothing". | A pool per standing changes plan B's numbers ("per game 1,024"), so the plan owner decides. Absent anchors already cost nothing on both paths. The built-ins have no budget at all. | plan owner, then L3 | `tests/engines.rs::a_game_over_its_budget_gets_no_pass_b_answer` |
| 5 | **Two publishers may give two codes the same display name.** | Refusing it needs a new code (`engine-name-taken`) that the spec must name first. Codes are owned as read (`descriptor::code_identity`). | spec, then L3 | none yet |
| 6 | **`engine-ambiguous` carries codes only.** It has no descriptor digest, program or evidence per candidate. | This changes `Detection`'s JSON shape. D1 needs the per-candidate data to resolve the result through a moderator's `games[]` `engine` row or the player's pick, so D1 decides the shape. | D1 | `Detection::engine_candidates` |
| 7 | **Pass B refuses the built-ins' own grammar.** From a first-party descriptor in an `EngineSet`, `walk.list_at_limit`, `scope` (`anywhere`, `named`), `programs = "none"`, the `name` check and `skip_wrappers = "folders"` are `descriptor-unsupported`; the evaluator runs them under the built-in policy only. (`weak_order`, `skip_wrappers = false` and anchorless rules Pass B admits, as in L1.) | `walk.list_at_limit` shapes the one walk Pass A has already made, so it can never be Pass B's. For the rest, no first-party Pass B descriptor needs one yet, and Pass B's choosing (standings, ambiguity, the hint) has not been decided for an unranked first match or a planning-only rule. | the first first-party descriptor that needs one | `tests/engines.rs::first_party_only_and_unsupported_parts_are_refused_as_such` |
| 9 | **A file swapped for a device node between the look and the open is opened.** Since L2 a probe opens only what is a regular file when looked at, and refuses what it opened unless it is one; a swap in between still opens the device (and is then refused). | Closing the race needs an open that cannot open a device at all, which no portable flag gives. Making the swap needs write access to the game folder and `mknod` (CAP_MKNOD). | none (accepted) | `src/engines/reads.rs` `open_regular` |
| 12 | **C1b never meets a real file of an older Astra.** `src/former.rs` knows one only by the shipped bytes' digests (RE-UE4SS 3.0.1's `dwmapi.dll`, ReShade 6.8.0, our add-ons), and those bytes are not committed. C1b's "older Astra" change (`tests/identity.rs` `Change::OlderAstra`) lays look-alikes, which every launcher must leave alone, and does; so `InstallPlan::former_files`, `UninstallReport::former_removed` and `not_restored` are held to 0.4.1's only while empty (`c1b_former_removed`, printed, 0, no floor). The rules themselves are pinned by the unit tests in `src/former.rs` and by `tests/former.rs`. | Integration tests cannot stand test digests in: `former::STAND_INS` is `cfg(test)`, the library's own unit tests only. Reaching it needs the three upstream files committed as test fixtures (MIT, BSD-3-Clause, ours), or a test seam in the library. | the owner (whether to commit the upstream binaries), then L3 | `tests/identity.rs` `c1b_former_removed` |
| 13 | **`validate-descriptor` speaks the launcher's codes, not the spec's.** The spec (`engine-descriptor-v1.md` §5, AstraPlugins d46e44a) names `descriptor-anchor-invalid` for a missing or ubiquitous anchor and `E_DESCRIPTOR_TOO_BROAD` for a negative-folder claim; `gi-probe` reports what `Descriptor::check_pass_b` returns, `descriptor-too-broad` with `reason` = `no-anchor`, `ubiquitous-runtime`, `program`, … , and `descriptor-too-broad{reason=negative:<folder>}` (merged plan R32: kebab-case wire codes). The spec says the launcher wins until one of the two is fixed. | K17 (L3-3, `tests/goldens.rs`) holds the spec's goldens to the launcher's parser, and the spec says the launcher wins: the launcher's spelling stays. The disagreement is recorded where it shows, `grief-unknown-games-no-anchor` in `SPEC_DEFECTS_AT_PIN` (below, "For the specification"). | the spec | `tests/common/spec_goldens.rs` `SPEC_DEFECTS_AT_PIN`; `tests/gi_probe.rs` `validate_descriptor_refuses_the_anchorless_and_the_runtime_anchored` |
| 14 | **`compat` launches on Windows only.** Steps 3 and 4 lay the synthetic game as `Platform::Windows` (the brief's choice), so launcher 0.3.1's Proton half of `prepare_launch` — the prefix check and `proton::apply_prefix` writing `[launch] proton_dll_overrides` into `user.reg` — is never run on a release. A release whose overrides 0.3.1 parses but cannot apply would pass. | Laying a Proton prefix (`steamapps/compatdata/<appid>/pfx/user.reg`) is a second synthetic game per release; no first-party release has needed it yet. | L3 (with `check`), or accepted | `src/bin/gi-probe/trees.rs` `lay` |
| 15 | **`gi-probe check` does not validate a Link's schema files.** It checks that every `params`/`data` file and the `skill` `[integration.link]` names is in the zip and within 4 KiB / 16 KiB, and Link v1's counts, names and lengths (`Manifest::lint`). Whether a schema keeps to `link-v1.md` §6's restricted profile (flat, ≤8 properties, local `$ref`, linear-time patterns) is not checked; the report says so (`link.schemas_validated: false`, notice `link-schemas-unvalidated`). | `link-v1` is a draft and may freeze as late as 0.2.8.x; a validator written against a draft would hold authors to rules that may change. | L3 or later, once `link-v1` freezes | `src/bin/gi-probe/check.rs` step 3 |
| 16 | **`gi-probe check` has no loader classification and no `loader-undeclared` yet.** A hijack-named `[[game_files]]` entry outside `[integration.loader] proxies` (K18) passes. | `HIJACK_NAMES` and the loader keep/proxy rules in planning are L3's step 5; `check` gets K18 with them. | L3 step 5 | `src/bin/gi-probe/check.rs` module doc |
| 17 | **`reserved-name` folds less than NFKC.** `reserved::fold_name` reads full-width forms, mathematical letters and digits and circled letters as ASCII, then `fold_confusables`, and keeps only letters and digits; any other compatibility form (a superscript, a parenthesised letter) is not folded. The spec says "NFKC plus a Unicode confusables fold". | An `[integration] name` may hold any script, so `name_char_allowed` cannot close it as it does for a descriptor's display name; full NFKC needs a Unicode normalisation dependency this crate does not have. A catalogue entry's name is also read by a moderator. | the spec (whether NFKC is required), then L3 | `src/reserved.rs` `names_are_reserved_through_every_fold` |

## For the daemon (D1)

Every place that turns a `Detection`'s engine back into the built-in enum must match on
`EngineCode::builtin()`. It must never use `unwrap_or(Engine::Unknown)`, which would hand a
descriptor's engine to the Mono runner. The sites, on Astra main (`astra-daemon/src/games/service.rs`):
:295, :358, :662, :725, :1235, :2084-2096 and :2345. Under plan B §4 they become `foundations_for(code)`.

```rust
match d.engine.builtin() {
    Some(e) => { /* a built-in engine, as before */ }
    None => { /* a descriptor's engine: no built-in runner; foundations_for(code) */ }
}
```

The narrow-only gate is `Detection::open_to_descriptors()`. A moderator's `games[].engine` row
applies only where it is true, or where the detection `is_ambiguous()`. Call the gate itself,
never a copy of it.

`Manifest::parse` refuses only what would make an install wrong (`engine-undeclared`,
`needs-newer-astra`, `loader-policy-conflict`, `unknown-key` in `[integration.loader]`). What a
release must also pass before anyone is offered it — a community id or name reading as Astra's
(`reserved-id`, `reserved-name`), `${bridge_token}` handed on without the capability
(`capability-undeclared`), Link v1's limits (`link-invalid`), the descriptor's Pass B admission —
is `Manifest::lint(first_party)`, which a catalogue entry has passed at ingest (`gi-probe check`).
A developer-mode folder import has passed nothing: call `Manifest::lint(false)` on it and show
what it returns. Every code the launcher can return is `codes::ALL` (K15).

## For the specification (K13)

### The goldens at d46e44a: `SPEC_DEFECTS_AT_PIN`

`tests/goldens.rs` (the library) and `tests/gi_probe.rs` (`gi-probe check`) hold AstraPlugins'
goldens, vendored at `tests/goldens/astraplugins/` (`PIN` d46e44a089387589018f28cfc3403399f7c3b89d,
PR #117), to this launcher's parser (K17) and `reserved.yaml` to `src/reserved.rs` (K13). These
are the goldens the launcher answers otherwise than they are labelled. Each is an entry of
`SPEC_DEFECTS_AT_PIN` (`tests/common/spec_goldens.rs`) that asserts the disagreement still holds,
so a golden fixed at a new pin turns the tests red until its entry goes. An answer is the
refusals `(code, field)` in order; a reason's text is never pinned. Re-pin with
`tools/vendor-astraplugins-goldens.sh <checkout> <commit>`.

| golden | the launcher answers | why it is the spec's |
|---|---|---|
| `descriptors/builtin/unity.toml` | parses; digest is not `src/engines/builtin/unity.toml`'s | copied from plan B §1's text, not from the launcher's L2 TOML, which C1 holds to 0.4.1's detector. Copy the launcher's file. |
| `descriptors/builtin/godot.toml` | parses; digest is not the launcher's | the same: copy `src/engines/builtin/godot.toml`. |
| `descriptors/builtin/unreal.toml` | `descriptor-invalid` (field `""`: `programs` matches no form, `ProgramsSpec`) | does not parse; copy `src/engines/builtin/unreal.toml`, which writes weak 100/110/120 in full. |
| `descriptors/hostile/take-recognised-game.toml` | `descriptor-invalid` `rule[0].evidence` | `evidence = "{project}"` with a `programs` that has no `at`, so `{project}` is never captured; it refuses before `engine-code-reserved`. Give `programs` an `at`, or evidence another capture. |
| `descriptors/hostile/grief-unknown-games-no-anchor.toml` | `descriptor-invalid` `rule[0].evidence` | the same `{project}`; and the rule, once reached, is the launcher's `descriptor-too-broad{reason}` (row 13), not `descriptor-anchor-invalid`. |
| `descriptors/hostile/anticheat-list-misses.toml` | `descriptor-invalid` `rule[0].evidence` | labelled for a later layer (lane D, a real folder), but the parser refuses it first, so the anti-cheat question is never asked. Fix the `{project}` capture. |
| `descriptors/hostile/slow-detection-budget-exhaustion.toml` | `descriptor-invalid` (field `""`: missing `binary`) | its rules lack `binary`, a key every rule must have, so it refuses for that before its 17 rules meet the rule budget it is named for (the launcher's answer for that: `descriptor-invalid` `rule`). |
| `manifests/invalid/engine-code-reserved.toml` | `descriptor-invalid` `rule[0].evidence` | its descriptor's `{project}` is never captured, so it refuses before `engine-code-reserved`. |
| `manifests/invalid/engine-undeclared.toml` | `engine-undeclared`, then `descriptor-invalid` `rule[0].evidence` | refused for its rule, and its descriptor breaks a second one: `evidence = "{core}"` with no lookup `as = "core"` (`foundation-owns-engine` has it). One golden, one rule. |
| `manifests/invalid/engine-undeclared-mismatch.toml` | `engine-undeclared`, then `descriptor-invalid` `rule[0].evidence` | the same `{core}`. |
| `manifests/valid/runner-builtin.toml` | accepted as first-party; as community, `reserved-id` | `vectors.json` has no field for provenance; add `"provenance": "first-party"` to its record (the tests read it once it is there). |
| `spec/games/reserved.yaml` `profile_file_names` | the launcher also reserves `mods-choice.json` | the mods choice (0.4.x), which no manifest may place. Add it. |
| `spec/games/reserved.yaml` `engine_codes` | the launcher reserves by rule | `engine_codes` lists five codes; `is_reserved_code` also refuses their look-alikes folded with digits read as letters (`un1ty-mono`, `g0dot`), the prefixes `unknown` and `astra`, every built-in family name as a prefix (`unity`, `unreal`, `godot`), and the infix `ambiguous`. State the rule. |

Also for the spec, not pinned by a golden:

- **`gi-probe check` raises `engine-code-reserved`.** `manifest.md` §2.1 says it does not (only
  ingest can); §4 lists it among `check`'s findings. `check` runs `[integration.engine]` through
  Pass B's admission, so it does. Say so in §2.1.
- **`[integration.foundation]` keys.** The schema has `additionalProperties: false`; the
  launcher never refuses a manifest for a key it does not know there (a claim acts on nothing),
  and `lint` reports it as `unknown-key`.
- **`[integration.capabilities]` kinds.** A kind other than `companion` is `needs-newer-astra`,
  as an unknown id is.
- **`keep_if_present`** names end in `.ini .cfg .json .toml .txt .xml .yml .yaml`; a `.dll`,
  `.asi` or `.exe` there is `loader-policy-conflict{reason=loader}`, as a name also in `proxies`
  is `{reason=proxy}`.
- **Link names.** Every tool's, trigger's and event's `name` matches `^[a-z][a-z0-9_]{0,31}$`
  and is unique in its list (the schema patterns tools' names only); a trigger's `label`, like a
  tool's `description`, is at most 200 characters.
- **`capability-undeclared`** counts `${bridge_token}` anywhere the manifest hands it on:
  `[[config_writes]]`, `[launch] args` and `env`.
- **`[integration] sdk`** is an `https://` URL, at most 2,048 bytes, with no space or control
  character.
- **`code_changed`** (`gi-probe check --previous`) counts `.dll .so .dylib .jar .lua .py .js .exe`
  and also `.asi .pyd .pyc` and a versioned `.so.N`, in `[[files]]` and `[[game_files]]` alike.
- **`reserved-name`'s fold** is not NFKC (row 17).

### The grammar

These must read the same in `AstraPlugins/spec/games/engine-descriptor-v1.md` and its goldens:

- **Display names.** Characters: printable ASCII, Latin-1 and Latin Extended-A letters, and
  Cyrillic letters (`descriptor::name_char_allowed`); anything else is `descriptor-invalid`.
  Reserved: from any word on, the rest of the name with its words run together and folded
  (`fold_confusables`, which first strips the marks from Latin letters) begins with a reserved
  word (`name_is_reserved`). The reserved words are `RESERVED_NAME_WORDS` plus the built-in
  family names.
- **Codes.** Reserved codes are compared by `fold_confusables`, which reads digits as letters
  (`un1ty`). Ownership is compared by `code_identity`, which does not, so `id-tech-6` and
  `id-tech-8` are two codes.
- **`strong-fallback`.** `passes` defaults to false.
- **JCS.** A control with no short escape is written as `\u` followed by four lower-case hex
  digits.
- **Goldens.** Pin the outcome of each case as (phase, first_party) → code, plus `field` where
  there is one. Never pin the reason text.
- **L2's grammar** (the built-ins in `src/engines/builtin/*.toml` are its worked examples):
  - `walk.list_at_limit` (first-party; built-in only, row 7): a folder exactly at the walk's
    depth bound whose path ends with one of these, ignoring ASCII case, is still listed, and
    never recursed past.
  - `bootstrap`: `listed` and `hint_stem` are each optional; at least one.
  - `binary = "by-ext"`: `.exe` is windows, any other program linux.
  - `child`: the exact spelling, else the least (byte order) of the names that match ignoring
    ASCII case among the first 5,000 a `read_dir` returns, links counted — 0.4.1's `child()`.
  - First-party only, and `descriptor-unsupported` in Pass B (row 7): `scope = "anywhere"` (weak
    rules; the first match in walk order answers, unranked; `programs.first` counts the programs
    it selects) and `scope = "named"` (weak rules; asked only about a program the caller names,
    never in detection); `programs = "none"` or `{ none = true, skip_wrappers, skip_dirs }`, only
    with `scope = "anywhere"`, its first check a `listed` below `{project}` — the first walk entry
    it names gives `{project}` (never the root, never below a skipped folder), and its capture
    stands for every entry it names below that project, in walk order, so `binary.from_name`
    takes the first that maps; `skip_wrappers = "folders"` (only the folders on the program's
    way); the `name` check (a `*` glob, at most two stars, over the program's own name ignoring
    ASCII case; no placeholder; needs a program).
