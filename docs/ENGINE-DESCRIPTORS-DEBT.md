# Engine descriptors: what L1 leaves open

This lists what launcher 0.5.0's L1 (engine descriptors and Pass B) knowingly does not do yet.
Each row gives the step that owes it and the test or line that pins it. A row comes off this
file only in the commit that closes it.

| # | What | Why it is not done in L1 | Owed by | Pinned by |
|---|---|---|---|---|
| 1 | **C1 is not yet plan B §2's whole C1.** The 20,000-tree corpus is here. Three parts are missing: the `bin/list-shape` recordings in `tests/corpus/recorded/`, `tests/corpus/divergence.txt`, and running 0.4.1 in a child process with a timeout. | In L1, Pass A is 0.4.1's code byte for byte (`src/detect.rs`: `pass_a` extracted, nothing else changed). There is no divergence yet, and nothing can hang here that does not also hang in 0.4.1. L2 replaces the detector and needs all three before its gate. | L2 | `tests/identity.rs` module doc |
| 2 | **A FIFO named like a top-level program blocks Pass A.** `has_embedded_pck` and `unity_version_in` use a plain `File::open`, as 0.4.1 does. | This is 0.4.1's behaviour, and C1 compares against it. When L2 moves the built-ins to Pass B's reads, this becomes a declared divergence. | L2 | `tests/engines.rs::a_top_level_fifo_never_blocks_detection` (ignored until L2) |
| 3 | **The Windows branch of `src/engines/reads.rs` has no test.** That branch is `FILE_FLAG_OPEN_REPARSE_POINT` with `FILE_TYPE_DISK`. Three cases are owed: a junction (`mklink /J`), a symlinked file, and an AppExecLink-style reparse file. | It needs the Windows VM. | before 0.5.0 is tagged | none yet |
| 4 | **One lookup pool per game, shared by every standing.** Descriptors whose anchors the game does hold can drain it. That wipes Pass B's answer for their standing and every one below it, so it also breaks developer mode's "Test on this game". Plan B §3 says only "that descriptor yields nothing". | A pool per standing changes plan B's numbers ("per game 1,024"), so the plan owner decides. Absent anchors already cost nothing on both paths. | plan owner, then L2 | `tests/engines.rs::a_game_over_its_budget_gets_no_pass_b_answer` |
| 5 | **Two publishers may give two codes the same display name.** | Refusing it needs a new code (`engine-name-taken`) that the spec must name first. Codes are owned as read (`descriptor::code_identity`). | spec, then L3 | none yet |
| 6 | **`engine-ambiguous` carries codes only.** It has no descriptor digest, program or evidence per candidate. | This changes `Detection`'s JSON shape. D1 needs the per-candidate data to resolve the result through a moderator's `games[]` `engine` row or the player's pick, so D1 decides the shape. | D1 | `Detection::engine_candidates` |
| 7 | **The evaluator has only Pass B's policy.** It always checks the program's magic, takes Pass B's extensions, requires `first`, drops a classify `unknown`, makes two different codes ambiguous before `best()`, and treats the hint as matching every tier. `walk.list_at_limit` does not reach the walk. | L2 writes the built-ins as data. They need a built-in policy, which has to be decided with them. | L2 | `Descriptor::check_pass_b` (`descriptor-unsupported`) |
| 8 | **A walk of exactly 5,000 entries reads as cut at its cap.** | `walk()` does not report truncation; it is inferred from the count. This errs safe: a Pass B claim adds `anti-cheat-unknown`. | L2 (with the walk) | `eval.rs` `Game::anti_cheat` |
| 9 | **Pass B opens a file before checking its type.** The S_ISREG check after the open refuses a device node, but the open itself has already happened. | No unprivileged fixture can tell "opened, then refused" from "refused before opening" (`mknod` needs CAP_MKNOD). A game folder holding a device node already takes root. | L2 (with the reads) | `src/engines/reads.rs` |

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
