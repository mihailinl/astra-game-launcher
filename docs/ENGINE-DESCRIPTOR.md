# Engine descriptors (`astra.engine/1`) in this launcher

An engine descriptor tells the launcher what a game folder made with one engine looks like:
which file is the program, and what sits beside it. It is data, never a list of games. This
page says where the format is defined, where to find worked examples, and how this launcher
reads the places the definition leaves open. It is not a second copy of the definition.

## The definition

The normative text is AstraPlugins'
[`spec/games/engine-descriptor-v1.md`](https://github.com/mihailinl/AstraPlugins/blob/db52804689bb7ed785c67cac9dc31b15da590477/spec/games/engine-descriptor-v1.md)
at commit **`db52804689bb7ed785c67cac9dc31b15da590477`**, with its schema,
[`spec/games/schemas/engine-descriptor-v1.schema.json`](https://github.com/mihailinl/AstraPlugins/blob/db52804689bb7ed785c67cac9dc31b15da590477/spec/games/schemas/engine-descriptor-v1.schema.json),
and its goldens, `testdata/games/descriptors/`. That commit is the one in
`tests/goldens/astraplugins/PIN`: the launcher's copy of the goldens is vendored from it, and
`tests/goldens.rs` holds the commit named here equal to `PIN`. A re-pin
(`tools/vendor-astraplugins-goldens.sh <checkout> <commit>`) changes this page with it.

Where the specification and this launcher disagree, the specification says the launcher wins
until one of the two is fixed. Every disagreement known at the pin is one entry of
`SPEC_DEFECTS_AT_PIN` (`tests/common/spec_goldens.rs`), which fails once the golden it names is
fixed; [`ENGINE-DESCRIPTORS-DEBT.md`](ENGINE-DESCRIPTORS-DEBT.md) ("For the specification")
lists them with what the specification should change.

## Worked examples

- **The built-in engines** are descriptors in this grammar:
  [`src/engines/builtin/unity.toml`](../src/engines/builtin/unity.toml),
  [`godot.toml`](../src/engines/builtin/godot.toml) and
  [`unreal.toml`](../src/engines/builtin/unreal.toml) (also
  `astra_game_launcher::engines::BUILTIN`). Each rule names, in a comment, the function of
  launcher 0.4.1 it replaces, and canary C1 holds them to that detector over 20,000 generated
  folders and the real recordings in `tests/corpus/recorded/`. They use parts of the grammar
  only Astra's own descriptors may use (below).
- **A community descriptor**: Source 2, plan B §1's, as `SOURCE2` in `tests/gi_probe.rs`, and
  the specification's own foundation `manifests/valid/foundation-owns-engine.toml`, installed
  end to end in `tests/source2.rs`.

## Checking a descriptor

```
gi-probe validate-descriptor engine.toml [--first-party] [--corpus <folder of recordings>]
gi-probe capture <game folder> --out game.json [--label <engine code>] [--appid N] [--hint REL]
```

`validate-descriptor` is the specification's §5 lint: the descriptor parses, Pass B admits it,
it claims none of the negative folders, and with `--corpus` it claims no recordings of two or
more engines (`descriptor-too-broad`, `reason` = `two-engines`). `capture` makes such a
recording of a real game folder (`astra.list-shape/1`, `tests/corpus/replay.rs`): the names,
kinds and sizes detection lists, and only the bytes the rules read. See the README's
`gi-probe` section.

In the library: `EngineSet::build` admits descriptors, `detect_with_engines` asks them (Pass B,
only where the built-ins found nothing), and `plan_install_in` asks the same ones when it
plans.

## `mods`: the player's own mod loaders

`mods` is the optional top-level member of `astra.engine/1` that the specification defines in
[§7](https://github.com/mihailinl/AstraPlugins/blob/db52804689bb7ed785c67cac9dc31b15da590477/spec/games/engine-descriptor-v1.md#7-mods-the-players-own-mod-loaders-optional)
at the pin above, with `$defs/mods` in its schema: how the player's own mod loader is
recognised, where its mods are, which of them are enabled and how Astra runs beside it.
Launcher 0.5.0 accepted it and read nothing in it; 0.5.1 reads it (`engines::mods`).

- **Accepted** inline or as a `[mods]` table in a file, in the catalogue's JSON, and as
  `[integration.engine.mods]` in a foundation's manifest. It must be a table: anything else is
  `descriptor-invalid`, `field` = `mods`, and skips the whole descriptor (§7.1), as do the value
  rules every member keeps to (no float, datetime or `null`, no integer past 2^53 − 1). It is
  part of the canonical form, so of the digest (the catalogue's copy without it is another
  descriptor: `descriptor-mismatch`) and of the 16 KiB size limit.
- **Validated** (`Descriptor::mods_refusals`): everything wrong inside it is
  `mods-invalid{field, reason}`, `field` the member (`mods.loader[1].root`), `reason` one of
  §7.9's (`engines::mods::reasons`), each derived from the grammar as the specification's checker
  derives it (`tools/check-games-goldens.py` `mods_reasons`): the closed sets, the shapes and
  counts, the jail and the component rules (8.3 short names and device names included), the
  placeholders (§2.4's and the descriptor's own captures), the loader anchor (never a proxy, a
  ubiquitous runtime, the program or a name the descriptor's own detection looks up; a
  root-anchored loader only when passive: no `chain`, no `via-manager`, no `tell`), the chain
  entry (a `.dll`, one of the loader's own top-level file lookups), `codes`, unique ids, the
  version template and `versions`, §7.8's per-member budget and §7.1's nesting, and no mod
  manager named anywhere (§7.7).
- **Dropped alone when refused.** A descriptor whose `mods` is refused inside detects, offers and
  plans exactly as without it; `Descriptor::mods` is then `None`, and a client says so once.
  Ingest refuses it: `Manifest::lint` (and so `gi-probe check`, step 2) and `gi-probe
  validate-descriptor` (step 1) report each `mods-invalid`.
- **Never read by detection** (§7.1): the validated model (`Descriptor::mods`) is kept beside
  the model the evaluator reads, never in it, so detection, Pass B's admission, the offers,
  planning and narrow-only are exactly what they are without it. Canary C1c holds that over every
  folder the tests detect, twice: with the hostile set's descriptors each carrying a `mods`
  refused inside that spells the grammar's own keys, and each carrying a valid one that uses every
  member §7 has (`tests/common/identity.rs` `hostile_mods_set`, `hostile_valid_mods_set`).
- **Evaluated** for one source at a time (`engines::mods::evaluate`, `evaluate_one`): the game
  folder, the active profile of the one manager an adapter reports (never another of its
  profiles), the folder the player picked — the caller chooses each, a descriptor never does.
  `{source}` is the jail: a path is resolved one component at a time, never through a link,
  and a probe opens only a regular file. `listed` and `child` read one listing per folder per
  evaluation; `stat` is the exact spelling. A placeholder takes the value the placing rule bound
  (`engines::mods::Bindings`), and a value that does not substitute to a §7.3 path makes the
  loader yield nothing. §7.8's per-evaluation bounds are counted, never timed: 5,000 entries
  in any one folder, 1,024 mods in a root, 2,048 lookups (every `require` lookup, every folder
  listed, one marker check per mod folder whatever the list says), 128 KiB read (a `version`
  probe 32 KiB, a list 16 KiB). Over any, that loader yields nothing for that source
  (`LoaderOutcome::OverBudget`), never a partial list. A loader recognised whose `versions` do
  not hold, unknown included, is recognised but unsupported, and the player is told what Astra
  needs. Nothing is opened for writing, and nothing found is logged (§7.10): what an evaluation
  returns is for the player's install sheet and that one start, and a runner is handed a chain's
  `entry` and its `tell`, never the list of mods.
- **What "with your mods" asks** (0.5.1 C, `src/mods.rs`): the built-in loaders are this data.
  The specification's own fragments (`testdata/games/descriptors/mods/builtin/*.mods.toml` at the
  pin) are GPL test data and never built in: `src/mods/builtin/` is the launcher's own copy,
  held member for member to them (`tests/mods_identity.rs`
  `the_builtin_mods_are_the_specifications_fragments`), each riding on the one built-in
  descriptor it is valid on — BepInEx 5 and BepInEx 6 IL2CPP (exactly `6.0.0-be.788`) on Unity;
  UE4SS 3, UE4SS 2 and the `~mods` paks on Unreal — and parsed apart from the descriptors Pass A
  asks, whose digests and answers stay what they were. Discovery (`discover_mods_in`) and a Play
  with mods (`resolve_play`) judge a folder through one function, `mods::judge`: the built-in
  rule that names the game's program gives its code and its bindings
  (`engines::builtin_placement`: `{project}` only from a strong rule; a game a weak rule places
  binds none, and its Unreal loaders yield nothing), and `evaluate_one` says which loader is
  there, its version, its mods root, which mods are enabled and how Astra runs beside it
  (`ModSource::coexist`, new: `chain` for BepInEx and UE4SS 3, `side-by-side` for the paks,
  empty for UE4SS 2 and for every loader the data does not describe). A chain's target is the
  data's `chain` entry. What stays code: the adapters (r2modman, Thunderstore Mod Manager, Gale:
  where their profiles are, and a mod they switched off by renaming its files `*.old`, which an
  adapter reads as the manager's own enable state, §7.7), Astra's own items and Astra's own loader
  in the game folder (never the player's), the loaders the data does not describe (MelonLoader;
  BepInEx 6 for Mono or CoreCLR, the IL2CPP preloader's older name) recognised only to be shown
  and never offered, exactly as 0.5.0 did, and when the loader last ran (BepInEx's
  `LogOutput.log`). `tests/mods_identity.rs` holds every Unity answer to launcher 0.5.0's
  (v0.5.0, a dev-dependency) over a seeded corpus of 600 game folders, mod-manager homes and
  Astra profiles, but for fifteen declared rows, each the data's reading, applied to 0.5.0's
  answer and counted with a floor (debt row 48): `BepInEx/patchers` is not the mods root; a
  plugins folder holding no `*.dll` is a mod (`dir-per-mod`), so a source of such folders alone
  appears; a BepInEx 6 IL2CPP with no `BepInEx/interop/` yet, a BepInEx of the other Unity
  flavour, one beside a game whose flavour nothing names, a preloader without its library, the
  game folder's BepInEx beside a program below the folder detection runs on, a `BepInEx/` spelled
  in another letter case (Linux) and a `BepInEx/` that is a link are not recognised, and a mod
  that is a link is no mod; an IL2CPP game's profile holding both cores is BepInEx 6; a library
  with no `ProductVersion` is read by its `FileVersion`; a version is shown without control
  characters; a root of more than 1,024 mods is over budget; and an IL2CPP integration declaring
  a build other than 788 offers no source (`not-supported{reason=outside-versions}`). Unreal is found for the first time (`tests/mods.rs`): a UE4SS 3 of the player's
  beside the real program is a source Astra chains, a UE4SS 2 is `loader-unsupported` with
  `need` = `UE4SS 3` whatever is installed, the paks are listed and run beside Astra. An Unreal
  runner declares no `[integration.mods]` (it chains a UE4SS 3 by itself), so its sources say
  `integration-without-mods`: the choice of "with" is not asked. Nothing in `src/mods.rs`,
  `src/mods/**` or `src/engines/**` writes anything but Astra's own profile files, or logs
  (`tests/mods_read_only.rs`, bound to call expressions).

The specification's `mods` goldens (`testdata/games/descriptors/mods/`, `vectors.json`
`descriptors.mods`, §7.11) hold the launcher to it: every hostile and compat golden is refused
for exactly the `reasons` it records and the valid one is not
(`tests/goldens.rs` `every_mods_golden_is_refused_for_exactly_its_reasons`), each places a Source 2
folder exactly as the same descriptor without `mods` (`the_mods_goldens_change_no_detection`),
`not-a-table` skips the descriptor (`a_mods_that_is_not_a_table_skips_the_descriptor`), the
built-in fragments are valid on the launcher's own built-ins
(`the_builtin_mods_fragments_ride_on_the_launchers_builtins`), `paths.json` is §7.3's
(`the_mods_paths_and_substitutions_are_the_specs`), and `tests/gi_probe.rs`
`validate_descriptor_refuses_each_mods_golden_for_exactly_its_reasons` holds
`validate-descriptor` to the same reasons. `hostile/mods-deep-nesting.toml` nests `any` in `any`
39 deep, one level under the toml crate's fixed recursion limit (80 nested tables and arrays;
AstraPlugins #133); a `mods` nested past it makes the whole file unreadable, as §7.1 says.

§7.1 keeps `mods` in the shape launcher 0.5.0 checks: a table (never `[[mods]]`), at the top
level only, within the value rules above, nesting at most 11 levels, so a 0.5.0 still detects
with every descriptor written for 0.5.1. Launcher 0.3.1 drops `[integration.engine]` unread (the
specification's §1), so a manifest it reads is unaffected, and no first-party manifest carries
`[integration.engine]` at all.

## Launcher-only notes

How this launcher reads what the specification leaves open, or says otherwise. Each is also a
request to the specification to say the same.

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
- **JCS.** A control character with no short escape is written as `\u` followed by four
  lower-case hex digits.
- **Lint codes.** `validate-descriptor` reports the launcher's own refusals:
  `descriptor-too-broad` with `reason` = `no-anchor`, `ubiquitous-runtime`, `program`, …, for
  what §5 calls `descriptor-anchor-invalid`, and `descriptor-too-broad` with `reason` =
  `negative:<folder>` or `two-engines` for its `E_DESCRIPTOR_TOO_BROAD` (merged plan R32:
  kebab-case wire codes). A recording labelled `unknown` names no engine, so it never counts
  towards `two-engines`.
- **Anchors.** A Pass B rule's anchor is a top-level `listed` check whose pattern is wholly
  literal: no `*` and no placeholder. §2.4 also counts a pattern that holds a placeholder
  (`{stem}_Data`, `{program}.pdb`) once it is substituted per candidate; the launcher does not —
  its one-lookup prefilter (a game folder's name set, before any rule runs) needs the name
  itself — so a rule whose only top-level `listed` checks hold a placeholder is
  `descriptor-too-broad{reason=no-anchor}`. Give such a rule a literal anchor beside it (a file
  every game of the engine ships under one name).
- **Goldens.** Pin the outcome of each case as (phase, first_party) → code, plus `field` where
  there is one. Never pin the reason's text.
- **The parts only Astra's own descriptors use.** The built-ins in `src/engines/builtin/*.toml`
  are their worked examples; Pass B refuses them as `descriptor-unsupported` (debt row 7):
  - `walk.list_at_limit` (first-party; built-in only): a folder exactly at the walk's depth
    bound whose path ends with one of these, ignoring ASCII case, is still listed, and never
    recursed past.
  - `bootstrap`: `listed` and `hint_stem` are each optional; at least one.
  - `binary = "by-ext"`: `.exe` is windows, any other program linux.
  - `child`: the exact spelling, else the least (byte order) of the names that match ignoring
    ASCII case among the first 5,000 a `read_dir` returns, links counted — 0.4.1's `child()`.
  - `scope = "anywhere"` (weak rules; the first match in walk order answers, unranked;
    `programs.first` counts the programs it selects) and `scope = "named"` (weak rules; asked
    only about a program the caller names, never in detection).
  - `programs = "none"` or `{ none = true, skip_wrappers, skip_dirs }`, only with
    `scope = "anywhere"`, its first check a `listed` below `{project}`: the first walk entry it
    names gives `{project}` (never the root, never below a skipped folder), and its capture
    stands for every entry it names below that project, in walk order, so `binary.from_name`
    takes the first that maps.
  - `skip_wrappers = "folders"` (only the folders on the program's way).
  - The `name` check: a `*` glob, at most two stars, over the program's own name ignoring ASCII
    case; no placeholder; needs a program.
- **Budgets.** One pool of lookups per game, shared by every standing (debt row 4); an
  `engine-ambiguous` result carries the codes only (debt row 6).
- **`mods`, read more strictly than the checker.** Each of these is refused where the
  specification's checker (`mods_reasons`) names no reason:
  - a loader's `name` keeps to the characters a display name may hold (§2.1, as
    `descriptor::name_char_allowed` reads it), trimmed (`shape`); the schema bounds only its
    length;
  - a `require` entry that is not a table, and an `any`/`all`/`none` that holds anything beside
    its one list, or an empty list (`shape`, `unknown`), as the schema refuses them;
  - a `tail` without `equals` (1 to 16 bytes, `bytes` its length when given; `shape`), which
    the schema leaves optional although a `tail` compares nothing without it;
  - a code, id, template or `versions` followed by a line break, which the checker's Python `$`
    lets through (`shape`, `version-invalid`) and the schema's patterns refuse;
  - a component longer than 255 BYTES, as §7.3 says (`path-invalid`), where the checker and the
    schema count characters.
- **`mods`, names compared as §7.4 says.** A detection name's `*` and placeholders match any
  run of characters, and nothing else does, ignoring ASCII case: the checker's `fnmatch` also
  reads `?` and `[…]` as wildcards (no detection name the launcher parses holds a `?`; a `[…]`
  it may), and its `.lower()` folds case beyond ASCII (`Ä` and `ä` are one name to it, two
  here). Where the checker's `re.IGNORECASE` folds a non-ASCII letter onto an ASCII one
  (`İ` and `ı` onto `i`, `ſ` onto `s`, the Kelvin sign onto `k`), so does the launcher, in the
  manager names (§7.7) and the ubiquitous runtime names (§2.4); its `$` stands before one final
  line break there too.
- **`mods`, at evaluation.** A `version` probe that would read past its 32 KiB is over a
  per-evaluation bound (the loader yields nothing for that source), not a version unknown: the
  player is told why there are no mods, never that their loader is too old. The loader's
  version is the value of the first key in `key` that reads in the template. A marker is checked
  in every mod folder, enabled by the list or not, and each check counts one lookup.
