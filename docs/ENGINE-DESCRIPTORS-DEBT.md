# Engine descriptors: what L1 and L2 leave open

This lists what launcher 0.5.0's L1 (engine descriptors and Pass B) and L2 (the built-in rules as
descriptors, the 0.4.1 detector deleted) knowingly do not do yet. Each row gives the step that
owes it and the test or line that pins it. A row comes off this file only in the commit that
closes it.

| # | What | Why it is not done | Owed by | Pinned by |
|---|---|---|---|---|
| 1 | **C1 has no recordings.** Plan B §2's C1 also replays `bin/list-shape` recordings of real game folders (`tests/corpus/recorded/`). The 20,000-tree corpus, `tests/corpus/divergence.txt` and running 0.4.1 in a child process with a timeout are done (L2: `tests/divergence.rs`). | A recording is a capture of a real Steam library's game folders: names, kinds, sizes, and the few bytes the built-ins read. L2 reads no real game folder, and the capture tool is L3's `gi-probe capture`. | L3 (the tool), then the owner (capturing his libraries) | `tests/identity.rs` module doc |
| 3 | **The Windows branch of `src/engines/reads.rs` has no test.** That branch is `FILE_FLAG_OPEN_REPARSE_POINT` with `FILE_TYPE_DISK`. Three cases are owed: a junction (`mklink /J`), a symlinked file, and an AppExecLink-style reparse file. Since L2 the built-ins read through it too. | It needs the Windows VM. | before 0.5.0 is tagged | none yet |
| 4 | **One lookup pool per game, shared by every standing.** Descriptors whose anchors the game does hold can drain it. That wipes Pass B's answer for their standing and every one below it, so it also breaks developer mode's "Test on this game". Plan B §3 says only "that descriptor yields nothing". | A pool per standing changes plan B's numbers ("per game 1,024"), so the plan owner decides. Absent anchors already cost nothing on both paths. The built-ins have no budget at all. | plan owner, then L3 | `tests/engines.rs::a_game_over_its_budget_gets_no_pass_b_answer` |
| 5 | **Two publishers may give two codes the same display name.** | Refusing it needs a new code (`engine-name-taken`) that the spec must name first. Codes are owned as read (`descriptor::code_identity`). | spec, then L3 | none yet |
| 6 | **`engine-ambiguous` carries codes only.** It has no descriptor digest, program or evidence per candidate. | This changes `Detection`'s JSON shape. D1 needs the per-candidate data to resolve the result through a moderator's `games[]` `engine` row or the player's pick, so D1 decides the shape. | D1 | `Detection::engine_candidates` |
| 7 | **Pass B refuses the built-ins' own grammar.** From a first-party descriptor in an `EngineSet`, `walk.list_at_limit`, `scope` (`anywhere`, `named`), `programs = "none"`, the `name` check and `skip_wrappers = "folders"` are `descriptor-unsupported`; the evaluator runs them under the built-in policy only. (`weak_order`, `skip_wrappers = false` and anchorless rules Pass B admits, as in L1.) | `walk.list_at_limit` shapes the one walk Pass A has already made, so it can never be Pass B's. For the rest, no first-party Pass B descriptor needs one yet, and Pass B's choosing (standings, ambiguity, the hint) has not been decided for an unranked first match or a planning-only rule. | the first first-party descriptor that needs one | `tests/engines.rs::first_party_only_and_unsupported_parts_are_refused_as_such` |
| 9 | **A file swapped for a device node between the look and the open is opened.** Since L2 a probe opens only what is a regular file when looked at, and refuses what it opened unless it is one; a swap in between still opens the device (and is then refused). | Closing the race needs an open that cannot open a device at all, which no portable flag gives. Making the swap needs write access to the game folder and `mknod` (CAP_MKNOD). | none (accepted) | `src/engines/reads.rs` `open_regular` |
| 10 | **Planning asks no descriptor yet.** `plan_install` checks `[target] engine` with `engine_of_exe_with(&EngineSet::empty(), …)`: the built-ins alone, exactly 0.4.1's answer. | `plan_install_in(…, &EngineSet, Provenance)` and its refusals are L3's. The descriptor half of `engines::engine_of_program` (first standing with a match, one code or none) has a unit test only. | L3 | `src/plan.rs` (the engine check) |

## For the daemon (D1)
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

## For the specification (K13)

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
