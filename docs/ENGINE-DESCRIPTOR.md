# Engine descriptors (`astra.engine/1`) in this launcher

An engine descriptor tells the launcher what a game folder made with one engine looks like:
which file is the program, and what sits beside it. It is data, never a list of games. This
page says where the format is defined, where to find worked examples, and how this launcher
reads the places the definition leaves open. It is not a second copy of the definition.

## The definition

The normative text is AstraPlugins'
[`spec/games/engine-descriptor-v1.md`](https://github.com/mihailinl/AstraPlugins/blob/4976d60e0cf34ce06436b85e65e1228a5d58c4c5/spec/games/engine-descriptor-v1.md)
at commit **`4976d60e0cf34ce06436b85e65e1228a5d58c4c5`**, with its schema,
[`spec/games/schemas/engine-descriptor-v1.schema.json`](https://github.com/mihailinl/AstraPlugins/blob/4976d60e0cf34ce06436b85e65e1228a5d58c4c5/spec/games/schemas/engine-descriptor-v1.schema.json),
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

## Reserved for launcher 0.5.1: `mods`

`mods` is an optional top-level member of `astra.engine/1`, a table, reserved for launcher
0.5.1's mods section: how the player's own mod loader is recognised, where its mods are and
which of them are enabled. That section is still to come in AstraPlugins'
`spec/games/engine-descriptor-v1.md` (a draft against `spec/games/`); the specification at the
pin above does not name `mods`. It is reserved now because the specification's §0 rule 5 skips
the whole of a descriptor holding a field the reader does not know: a mods section that arrived
after 0.5.0 would make every 0.5.0 drop every foundation that carries one.

Launcher 0.5.0:

- **accepts it** — inline or as a `[mods]` table in a file, in the catalogue's JSON, and as
  `[integration.engine.mods]` in a foundation's manifest. It must be a table (anything else is
  `descriptor-invalid`, `field` = `mods`), only at the top level, and what it holds keeps to the
  rules every member keeps to (no float, datetime or `null`, no integer past 2^53 − 1). No key
  inside it is checked.
- **reads nothing in it.** It is part of the canonical form, so of the digest (the catalogue's
  copy without it is another descriptor: `descriptor-mismatch`) and of the 16 KiB size limit,
  and of nothing else: it never reaches the model the evaluator reads
  (`Descriptor::carries_mods` only says it is there), so detection, Pass B's admission, the
  offers, planning and narrow-only are exactly what they are without it. Canary C1c holds that
  over every folder the tests detect, with the hostile set's descriptors each carrying a `mods`
  table that spells the grammar's own keys (`tests/common/identity.rs` `hostile_mods_set`).
- **says so**: `gi-probe validate-descriptor` reports it as the notice `mods-not-evaluated`
  ("mods: not evaluated by this version"), never a finding, and `gi-probe check` gives the same
  notice at step 4.

For this to hold, the coming section must keep `mods` a table (never `[[mods]]`) whose values
keep to the rules above. Launcher 0.3.1 drops `[integration.engine]` unread (the
specification's §1), so a manifest it reads is unaffected, and no first-party manifest carries
`[integration.engine]` at all. Debt row 46.

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
