# Engine descriptors: what L1, L2 and L3 so far leave open

This lists what launcher 0.5.0's L1 (engine descriptors and Pass B), L2 (the built-in rules as
descriptors, the 0.4.1 detector deleted) and the L3 steps committed so far (C1b over installs,
`gi-probe compat` and `validate-descriptor`, the `[integration.*]` tables and `gi-probe check`,
planning that asks the descriptors: `plan_install_in`; the community limits, the loader policy,
step 10a and the proxy-folder baseline; the restore rules, `restore_game_files` and the 0600
token writes; every anti-cheat signal and Steam's store categories) knowingly do not do yet. Each row gives the step that owes it and the test or line that pins it.
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
| 17 | **`reserved-name` folds less than NFKC.** `reserved::fold_name` reads full-width forms, mathematical letters and digits and circled letters as ASCII, then `fold_confusables`, and keeps only letters and digits; any other compatibility form (a superscript, a parenthesised letter) is not folded. The spec says "NFKC plus a Unicode confusables fold". | An `[integration] name` may hold any script, so `name_char_allowed` cannot close it as it does for a descriptor's display name; full NFKC needs a Unicode normalisation dependency this crate does not have. A catalogue entry's name is also read by a moderator. | the spec (whether NFKC is required), then L3 | `src/reserved.rs` `names_are_reserved_through_every_fold` |
| 18 | **The CLI plans as first-party.** `astra-game-launcher plan`/`install` (`src/bin/astra-game-launcher.rs:469`) call `plan_install`, which is `plan_install_in` with no engine descriptors, `Provenance::FirstParty` and no pick: none of the community limits, and a descriptor's engine is `engine-mismatch`. | It is a developer's own tool on their own machine, kept with first-party semantics (0.4.1's, but for step 10a); the daemon is what plans for players, with the provenance its seed vouches for. A `--provenance` flag would make the tool a second place that decides provenance. | none (accepted), or whoever needs the CLI to plan a community release | `src/bin/astra-game-launcher.rs` `"plan" \| "install"` |
| 19 | **Play keeps only the compiled keep names.** A config file a manifest's `[integration.loader] keep_if_present` names is kept at plan and install when it is there already, but one this install added that the game rewrites since is `needs-reinstall` at Play — Play relinks only `doorstop_config.ini` and `ReShade.ini` to `keep-existing` (`src/launch.rs` `keep_if_present`). | The step's rule: Play stays as it is. Reading the manifest's keep names at Play is a change to Play's semantics for every installed integration that declares them, which no first-party one does yet. | the first integration that declares a keep name it also places, or L3 | `src/launch.rs` `prepare_launch_with`, the relink of a rewritten config |
| 20 | **The proxy-folder baseline sees names, one level deep.** `uninstall` keeps a hijack-named file we added when its folder holds a name now that was not there before (merged plan R42): a loader that only writes into a folder that was there already (their `Mods/` from before us), or replaces a file by a name that was there, is not seen. A ledger written before 0.5.0 has no baseline: its uninstall removes the file as 0.4.1 did, and an update over it measures the folder then, so what arrived between that install and the update counts as there before. | A deeper baseline (sizes, digests, a recursive listing) costs a walk of the game folder at every install and holds far more in the ledger; a game update that touches the folder would keep the proxy more often. The pre-0.5.0 ledgers hold nothing to measure against. | plan owner (accepted unless a deeper baseline is asked for) | `src/plan.rs` `proxy_folders`, `proxy_folder_gained` |
| 21 | **`restore-partial` cannot name a replaced file that is gone.** When a game file we replaced is missing at uninstall or restore, the player's file is not put back (M2: a backup goes back only over our file) and the report warns `restore-partial`, but `UninstallReport::kept` names files left in place, and there is none. | Naming it needs another report field, which the daemon reads and C1b holds to 0.4.1's report (row `restore-partial`); what the person is told is the same either way ("verify the game's files in Steam"). | D1 (if the daemon shows the name), then L | `tests/restore.rs` `a_replaced_file_that_is_gone_is_not_put_back_and_is_partial` |
| 22 | **The link-in-our-place restore case runs on Unix only.** `tests/restore.rs` lays a symbolic link where a file we replaced was (`Left::LinkInPlace`) under `cfg(unix)`, as every link test here does; on Windows the same `existing_target`/`file_state` refusal (`symlink_metadata`, which reports symbolic links and junctions as links) is reached by no restore test, only the folder-in-place case. | A file symlink on Windows needs Developer Mode or an elevated session, and a junction is a folder link only; no test here stands either up yet. | L (a junction or elevated-symlink fixture on the Windows VM) | `tests/restore.rs` `Left::LinkInPlace` |
| 23 | **The extended anti-cheat signatures are verified from public file lists, not real installs.** `ANTI_CHEAT_SIGNATURES` (`src/detect.rs`) adds EA Javelin (`EAAntiCheat.*.exe`), ACE (`AntiCheatExpert/`, `AceAntibotClient/`), EQU8 (`equ8_conf.json`) and Denuvo Anti-Cheat (`denuvo-anti-cheat*`, `Denuvo Anti-Cheat Installer.exe`), each name beside its source: SteamDB's file detection rules (from Steam depot file lists, 243cf74), BinaryObjectScanner's Denuvo checks (from Doom Eternal's depot 782332, 0c23b23), and, for EA Javelin, Steam's own launch entries in this desk's `appinfo.vdf` (Apex Legends, 1172470, launches `EAAntiCheat.GameServiceLauncher.exe`). A§4.5 asks the S1a builder to verify them on real installs; none of these games is installed on this desk or the VM. A name a real install spells otherwise is a miss, never a false refusal of another game. | No such install is on hand. | the owner (a `gi-probe capture` of one game per vendor), then L | `tests/divergence.rs` `extended_anti_cheat`; `tests/anti_cheat.rs` `the_extended_signatures_see_the_names_their_sources_give` |
| 24 | **For the spec (games-index-v1 §2.3, §2.5): a service belongs to a game by its image path.** §2.5's "a system-installed kernel anti-cheat service" and §2.3's `service-name` marker do not say how a service is tied to a game. The launcher's reading (`src/anti_cheat_services.rs`): a kernel-driver service (`Type` 1 or 2) whose `ImagePath` resolves inside the game's folder is `kernel-anti-cheat`, and a service a `service-name` marker names counts (`listed-anti-cheat`) only the same way. A system-wide service elsewhere (Vanguard, FACEIT, EAC's or BattlEye's service under `Program Files`) refuses nothing: counting it would refuse every game on any machine where one is installed. Such anti-cheats ship a folder signature in the games that use them, which refuses those. Every detection and every plan reads the Services key once (bounded: 16,384 keys, 32 KiB values). | The spec should say it, or say otherwise; until then the launcher's reading stands. | the spec | `src/anti_cheat_services.rs` `tests::only_a_service_whose_image_is_in_the_game_folder_counts`; `tests/anti_cheat.rs` `a_service_is_tied_to_the_game_by_its_image_path` (Windows) |
| 25 | **Steam's anti-cheat disclosure is not read: it is not in `appinfo.vdf`.** Merged plan §2.4 lists "Steam's disclosure in appinfo" as a signal, conditional on its key being found. This desk's real `appinfo.vdf` (v29, 683 apps, 2026-10-31) holds PUBG: BATTLEGROUNDS (578080), whose store discloses kernel-mode anti-cheat (`IStoreBrowseService/GetItems` with `include_extra_details`: `extra_details.anticheat = {"kernel_mode": true, …}`), and Apex Legends (1172470, `{"kernel_mode": true, "name": "Easy Anti-Cheat"}`); neither entry carries any anti-cheat key under `common` or `extended`, nor does any of the 683. The one anti-cheat key appinfo has, `extended/anti_cheat_support_url` (8 apps, an EasyAntiCheat or BattlEye game-ban support link, sometimes empty), is a support link, not a disclosure, and is not read. No key was guessed. | The disclosure lives in the store web API, which the launcher, offline and reading local files only, does not call. | the daemon (D2: the catalogue client already talks to the network; it may read `GetItems` and add a refusal), or accepted | `src/steam_appinfo.rs` module doc |
| 26 | **The signature table names A§4.5's vendors only.** SteamDB's rules (the source of row 23) name more folder-visible anti-cheat: BlackCipher (`BlackCat64.sys`), NetEase's (`NeacSafe64.sys`), PunkBuster, Ricochet (`Randgrid.sys`), TenProtect (`TP3Helper.exe`), HackShield, AnyBrain, and further names of three already in the table (`gameguard.des`, `*.xem`, `EasyAntiCheat_Setup.exe`, `EasyAntiCheat_x64.dll`). Each one added is a declared C1 divergence of its own. A kernel driver among them (`*.sys`) is still `anti-cheat-unknown` on a Pass B claim. | Outside L3-7's brief (A§4.5's list); the catalogue's `anticheat[]` can add any of them without a launcher release. | L (a row each), or the catalogue's moderators | `ANTI_CHEAT_SIGNATURES` |
| 27 | **Install plans again without the catalogue's markers.** `install` replans with the descriptor the plan pinned alone (`InstallPlan::engine_descriptor`), not the daemon's `EngineSet`, so a folder or file a marker names that appears between the plan and the install is not seen then; the folder signatures and the services are. | The plan carries no marker list, and adding one changes `InstallPlan`'s JSON, which C1b holds to 0.4.1's; the window is the seconds between the consent sheet and the install. | L (if the plan learns to carry them), or accepted | `src/install.rs` `install`, the replan |

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

Anti-cheat (L3-7, merged plan §2.4): parse the signed index's `anticheat[]` rows into
`AntiCheatMarker`s (they deserialise as they are) and hand them to BOTH `detect_with_engines` and
`plan_install_in` through `EngineSet::with_anti_cheat_markers`; a set built without them loses
`listed-anti-cheat` silently. `Detection::anti_cheat` then carries every signal for the game's
row; each code the launcher emits is `ANTI_CHEAT_CODES`, and `listed-anti-cheat` and
`kernel-anti-cheat` are new codes in `codes::ALL` (K15: words in every locale). The online-game
offer rule reads `online_game(appinfo, appid)` (`Some([])` known and none, `None` not known), or
`store_categories_of` + `online_codes` for a library in one pass; the daemon spells no category
itself (K16).

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
| `manifests/valid/game-specific-link.toml` | `loader-undeclared` (`file` = `winhttp.dll`) | it places `winhttp.dll`, a name mod loaders hijack-load by, with no `[integration.loader] proxies` naming it; a community release must declare it (merged plan §2.5, K18). Add `[integration.loader] proxies = ["winhttp.dll"]`. |
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
  is `{reason=proxy}` — and so is a `proxies` name every install keeps (`doorstop_config.ini`,
  `ReShade.ini`).
- **The hijack names.** "Any other DLL-hijack-named file" (`games-index-v1.md` §2.4, A§4.5) is
  `HIJACK_NAMES` (`src/plan.rs`): A§4.5's names with `xinput*` as the six XInput DLLs, the
  Doorstop names (`winmm`), Ultimate ASI Loader's system-DLL names (`dinput dsound d3d8 d3d9
  d3d10 d3d12 ddraw msacm32 msvfw32 wininet xlive`), and `opengl32`. `gi-probe check`'s step 7
  labels against it and the manifest's `proxies`; say the list in the spec, or point at it.
- **What a community release may place and launch** (plan B §3, `loader-undeclared` and
  `community-limit{rule}`): `[[game_files]]` only as `${game}/<name>` or `${game}/<id>/…`, no
  component `BepInEx`, `MelonLoader`, `ue4ss`, `Mods`, `~mods` (ASCII case ignored); no `[launch]
  env`; overrides only for a DLL its own `[[game_files]]` place, `native,builtin` or `n,b`; an
  argument holding `/`, `\` or `${` only as `${profile}/…` or `${game}/…`, no `..` and no other
  placeholder after it (so `${bridge_token}` is handed on through `[[config_writes]]`, never the
  command line); and a planned program the built-in walk lists. Neither code is in
  `manifest.md` yet; add them, and a golden each.
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
