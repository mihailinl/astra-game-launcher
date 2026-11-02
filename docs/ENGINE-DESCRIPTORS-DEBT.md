# Engine descriptors: what launcher 0.5.0 leaves for after it

Launcher 0.5.0 — L1 (engine descriptors and Pass B), L2 (the built-in rules as descriptors, the
0.4.1 detector deleted) and L3 (planning that asks the descriptors, `plan_install_in` and
`Provenance`; the `[integration.*]` tables, `Manifest::lint` and the one reserved list; the
community limits, the loader policy, step 10a and the proxy-folder baseline; the restore rules,
`restore_game_files` and the 0600 token writes; every anti-cheat signal and Steam's store
categories; `gi-probe compat`, `check`, `validate-descriptor --corpus` and `capture`; C1 over
real recordings) — knowingly leaves the rows below open. **Every row is for after 0.5.0 is
tagged**: none blocks the tag. Each gives who owes it and the test or line that pins it. A row
comes off this file only in the commit that closes it.

| # | What | Why it is not done | Owed by | Pinned by |
|---|---|---|---|---|
| 1 | **C1 replays this desk's main Steam library only: the owner's other libraries are not committed.** `tests/corpus/recorded/` holds the 8 folders of `~/.local/share/Steam/steamapps/common` on the desk L3 was built on (Valve's runtimes, Proton, Steamworks Shared, Astra's own install, an empty Half-Life 2 Deathmatch, 2026-10-31), each captured by `gi-probe capture`; C1 replays them against 0.4.1 and holds each to what the folder answered when it was captured. None of them is a game a built-in names, so no committed recording reaches a version header or an Unreal pairing. The desk's two other libraries (`/mnt/data/SteamLibrary`, 33 folders; `/mnt/Storage/SteamLibrary`, 6) were captured and replayed the same way locally, never committed (`AGL_C1_RECORDED`): 47 recordings, every one held to its capture and equal to 0.4.1, 13 named by a built-in (Unity Mono and IL2CPP, Unreal). The Windows VM has no Steam library. | The repository is public, and those folders are the owner's: which games he has, and whatever he keeps in them (his mods, a work in progress). Which recordings may be published is his to say. | the owner: he runs `gi-probe capture` on his libraries (`AGL_C1_RECORDED=<folder> cargo test --test identity` replays them without committing), and commits the ones he chooses; nothing waits on it | `tests/identity.rs` `RECORDINGS`, `c1_detection_and_planning_are_launcher_0_4_1s_over_the_recordings` |
| 4 | **One lookup pool per game, shared by every standing.** Descriptors whose anchors the game does hold can drain it. That wipes Pass B's answer for their standing and every one below it, so it also breaks developer mode's "Test on this game". Plan B §3 says only "that descriptor yields nothing". | A pool per standing changes plan B's numbers ("per game 1,024"), so the plan owner decides. Absent anchors already cost nothing on both paths. The built-ins have no budget at all. | plan owner, then L (after 0.5.0) | `tests/engines.rs::a_game_over_its_budget_gets_no_pass_b_answer` |
| 5 | **Two publishers may give two codes the same display name.** | Refusing it needs a new code (`engine-name-taken`) that the spec must name first. Codes are owned as read (`descriptor::code_identity`). | spec, then L (after 0.5.0) | none yet |
| 6 | **`engine-ambiguous` carries codes only.** It has no descriptor digest, program or evidence per candidate. | This changes `Detection`'s JSON shape. D1 needs the per-candidate data to resolve the result through a moderator's `games[]` `engine` row or the player's pick, so D1 decides the shape. | D1 | `Detection::engine_candidates` |
| 7 | **Pass B refuses the built-ins' own grammar.** From a first-party descriptor in an `EngineSet`, `walk.list_at_limit`, `scope` (`anywhere`, `named`), `programs = "none"`, the `name` check and `skip_wrappers = "folders"` are `descriptor-unsupported`; the evaluator runs them under the built-in policy only. (`weak_order`, `skip_wrappers = false` and anchorless rules Pass B admits, as in L1.) | `walk.list_at_limit` shapes the one walk Pass A has already made, so it can never be Pass B's. For the rest, no first-party Pass B descriptor needs one yet, and Pass B's choosing (standings, ambiguity, the hint) has not been decided for an unranked first match or a planning-only rule. | the first first-party descriptor that needs one | `tests/engines.rs::first_party_only_and_unsupported_parts_are_refused_as_such` |
| 9 | **A file swapped for a device node between the look and the open is opened.** Since L2 a probe opens only what is a regular file when looked at, and refuses what it opened unless it is one; a swap in between still opens the device (and is then refused). | Closing the race needs an open that cannot open a device at all, which no portable flag gives. Making the swap needs write access to the game folder and `mknod` (CAP_MKNOD). | none (accepted) | `src/engines/reads.rs` `open_regular` |
| 12 | **C1b never meets a real file of an older Astra.** `src/former.rs` knows one only by the shipped bytes' digests (RE-UE4SS 3.0.1's `dwmapi.dll`, ReShade 6.8.0, our add-ons), and those bytes are not committed. C1b's "older Astra" change (`tests/identity.rs` `Change::OlderAstra`) lays look-alikes, which every launcher must leave alone, and does; so `InstallPlan::former_files`, `UninstallReport::former_removed` and `not_restored` are held to 0.4.1's only while empty (`c1b_former_removed`, printed, 0, no floor). The rules themselves are pinned by the unit tests in `src/former.rs` and by `tests/former.rs`. | Integration tests cannot stand test digests in: `former::STAND_INS` is `cfg(test)`, the library's own unit tests only. Reaching it needs the three upstream files committed as test fixtures (MIT, BSD-3-Clause, ours), or a test seam in the library. | the owner (whether to commit the upstream binaries), then L (after 0.5.0) | `tests/identity.rs` `c1b_former_removed` |
| 13 | **`validate-descriptor` speaks the launcher's codes, not the spec's.** The spec (`engine-descriptor-v1.md` §5, AstraPlugins db52804) names `descriptor-anchor-invalid` for a missing or ubiquitous anchor and `E_DESCRIPTOR_TOO_BROAD` for a negative-folder claim; `gi-probe` reports what `Descriptor::check_pass_b` returns, `descriptor-too-broad` with `reason` = `no-anchor`, `ubiquitous-runtime`, `program`, … , and `descriptor-too-broad{reason=negative:<folder>}` or, for the claims over labelled recordings (`--corpus`), `{reason=two-engines}` (merged plan R32: kebab-case wire codes). The spec says the launcher wins until one of the two is fixed. | K17 (L3-3, `tests/goldens.rs`) holds the spec's goldens to the launcher's parser, and the spec says the launcher wins: the launcher's spelling stays. The disagreement is recorded where it shows, `grief-unknown-games-no-anchor` in `SPEC_DEFECTS_AT_PIN` (below, "For the specification"). | the spec | `tests/common/spec_goldens.rs` `SPEC_DEFECTS_AT_PIN`; `tests/gi_probe.rs` `validate_descriptor_refuses_the_anchorless_and_the_runtime_anchored` |
| 14 | **`compat` launches on Windows only.** Steps 3 and 4 lay the synthetic game as `Platform::Windows` (the brief's choice), so launcher 0.3.1's Proton half of `prepare_launch` — the prefix check and `proton::apply_prefix` writing `[launch] proton_dll_overrides` into `user.reg` — is never run on a release. A release whose overrides 0.3.1 parses but cannot apply would pass. | Laying a Proton prefix (`steamapps/compatdata/<appid>/pfx/user.reg`) is a second synthetic game per release; no first-party release has needed it yet. | L (after 0.5.0, with `check`), or accepted | `src/bin/gi-probe/trees.rs` `lay` |
| 15 | **`gi-probe check` does not validate a Link's schema files.** It checks that every `params`/`data` file and the `skill` `[integration.link]` names is in the zip and within 4 KiB / 16 KiB, and Link v1's counts, names and lengths (`Manifest::lint`). Whether a schema keeps to `link-v1.md` §6's restricted profile (flat, ≤8 properties, local `$ref`, linear-time patterns) is not checked; the report says so (`link.schemas_validated: false`, notice `link-schemas-unvalidated`). | `link-v1` is a draft and may freeze as late as 0.2.8.x; a validator written against a draft would hold authors to rules that may change. | L (after 0.5.0), once `link-v1` freezes | `src/bin/gi-probe/check.rs` step 3 |
| 17 | **`reserved-name` folds less than NFKC.** `reserved::name_words` reads full-width forms, mathematical letters and digits, circled and parenthesised letters, superscript and subscript digits, `ⁱ`, `ⁿ` and the Latin ligatures as ASCII, then `fold_confusables`, word by word (a separator splits, a zero-width character, a combining mark or an apostrophe does not; a run of one- and two-letter pieces is one word); any other compatibility form (a modifier letter, another superscript letter) is not folded, and a word split into pieces of three or more letters (`Min ice`, `Off icial`) is two words, as the spec's fold reads it. The spec says "NFKC plus a Unicode confusables fold". | An `[integration] name` may hold any script, so `name_char_allowed` cannot close it as it does for a descriptor's display name; full NFKC needs a Unicode normalisation dependency this crate does not have. A catalogue entry's name is also read by a moderator. | the spec (whether NFKC is required), then L (after 0.5.0) | `src/reserved.rs` `names_are_reserved_through_every_fold` |
| 18 | **The CLI plans as first-party.** `astra-game-launcher plan`/`install` (`src/bin/astra-game-launcher.rs:469`) call `plan_install`, which is `plan_install_in` with no engine descriptors, `Provenance::FirstParty` and no pick: none of the community limits, and a descriptor's engine is `engine-mismatch`. | It is a developer's own tool on their own machine, kept with first-party semantics (0.4.1's, but for step 10a); the daemon is what plans for players, with the provenance its seed vouches for. A `--provenance` flag would make the tool a second place that decides provenance. | none (accepted), or whoever needs the CLI to plan a community release | `src/bin/astra-game-launcher.rs` `"plan" \| "install"` |
| 20 | **The proxy-folder baseline sees names, one level deep.** `uninstall` keeps a hijack-named file we added when its folder holds a name now that was not there before and can carry a loader — a folder, a link, a program, a library, an ASI plugin, a ReShade add-on; never a config, a log or a screenshot, which is what our own ReShade and UE4SS write there (merged plan R42) — and never UE4SS's proxy with no UE4SS beside it: a loader that only writes into a folder that was there already (their `Mods/` from before us), or replaces a file by a name that was there, or arrives as a lone config file, is not seen. A ledger written before 0.5.0 has no baseline: its uninstall removes the file as 0.4.1 did, and an update over it measures the folder then, so what arrived between that install and the update counts as there before. | A deeper baseline (sizes, digests, a recursive listing) costs a walk of the game folder at every install and holds far more in the ledger; a game update that touches the folder would keep the proxy more often. The pre-0.5.0 ledgers hold nothing to measure against. | plan owner (accepted unless a deeper baseline is asked for) | `src/plan.rs` `proxy_folders`, `proxy_folder_gained` |
| 21 | **`restore-partial` cannot name a replaced file that is gone.** When a game file we replaced is missing at uninstall or restore, the player's file is not put back (M2: a backup goes back only over our file) and the report warns `restore-partial`, but `UninstallReport::kept` names files left in place, and there is none. | Naming it needs another report field, which the daemon reads and C1b holds to 0.4.1's report (row `restore-partial`); what the person is told is the same either way ("verify the game's files in Steam"). | D1 (if the daemon shows the name), then L | `tests/restore.rs` `a_replaced_file_that_is_gone_is_not_put_back_and_is_partial` |
| 22 | **The link-in-our-place restore case runs on Unix only.** `tests/restore.rs` lays a symbolic link where a file we replaced was (`Left::LinkInPlace`) under `cfg(unix)`, as every link test here does; on Windows the same `existing_target`/`file_state` refusal (`symlink_metadata`, which reports symbolic links and junctions as links) is reached by no restore test, only the folder-in-place case. | A file symlink on Windows needs Developer Mode or an elevated session, and a junction is a folder link only; no test here stands either up yet. | L (a junction or elevated-symlink fixture on the Windows VM) | `tests/restore.rs` `Left::LinkInPlace` |
| 23 | **The extended anti-cheat signatures are verified from public file lists, not real installs.** `ANTI_CHEAT_SIGNATURES` (`src/detect.rs`) adds EA Javelin (`EAAntiCheat.*.exe`), ACE (`AntiCheatExpert/`, `AceAntibotClient/`), EQU8 (`equ8_conf.json`) and Denuvo Anti-Cheat (`denuvo-anti-cheat*`, `Denuvo Anti-Cheat Installer.exe`), each name beside its source: SteamDB's file detection rules (from Steam depot file lists, 243cf74), BinaryObjectScanner's Denuvo checks (from Doom Eternal's depot 782332, 0c23b23), and, for EA Javelin, Steam's own launch entries in this desk's `appinfo.vdf` (Apex Legends, 1172470, launches `EAAntiCheat.GameServiceLauncher.exe`). A§4.5 asks the S1a builder to verify them on real installs; none of these games is installed on this desk or the VM. A name a real install spells otherwise is a miss, never a false refusal of another game. | No such install is on hand. | the owner (a `gi-probe capture` of one game per vendor), then L | `tests/divergence.rs` `extended_anti_cheat`; `tests/anti_cheat.rs` `the_extended_signatures_see_the_names_their_sources_give` |
| 24 | **For the spec (games-index-v1 §2.3, §2.5): a service belongs to a game by its image path.** §2.5's "a system-installed kernel anti-cheat service" and §2.3's `service-name` marker do not say how a service is tied to a game. The launcher's reading (`src/anti_cheat_services.rs`): a kernel-driver service (`Type` 1 or 2) whose `ImagePath` resolves inside the game's folder is `kernel-anti-cheat`, and a service a `service-name` marker names counts (`listed-anti-cheat`) only the same way. A system-wide service elsewhere (Vanguard, FACEIT, EAC's or BattlEye's service under `Program Files`) refuses nothing: counting it would refuse every game on any machine where one is installed. Such anti-cheats ship a folder signature in the games that use them, which refuses those. Every detection and every plan reads the Services key once (bounded: 16,384 keys, 32 KiB values). | The spec should say it, or say otherwise; until then the launcher's reading stands. | the spec | `src/anti_cheat_services.rs` `tests::only_a_service_whose_image_is_in_the_game_folder_counts`; `tests/anti_cheat.rs` `a_service_is_tied_to_the_game_by_its_image_path` (Windows) |
| 25 | **Steam's anti-cheat disclosure is not read: it is not in `appinfo.vdf`.** Merged plan §2.4 lists "Steam's disclosure in appinfo" as a signal, conditional on its key being found. This desk's real `appinfo.vdf` (v29, 683 apps, 2026-10-31) holds PUBG: BATTLEGROUNDS (578080), whose store discloses kernel-mode anti-cheat (`IStoreBrowseService/GetItems` with `include_extra_details`: `extra_details.anticheat = {"kernel_mode": true, …}`), and Apex Legends (1172470, `{"kernel_mode": true, "name": "Easy Anti-Cheat"}`); neither entry carries any anti-cheat key under `common` or `extended`, nor does any of the 683. The one anti-cheat key appinfo has, `extended/anti_cheat_support_url` (8 apps, an EasyAntiCheat or BattlEye game-ban support link, sometimes empty), is a support link, not a disclosure, and is not read. No key was guessed. | The disclosure lives in the store web API, which the launcher, offline and reading local files only, does not call. | the daemon (D2: the catalogue client already talks to the network; it may read `GetItems` and add a refusal), or accepted | `src/steam_appinfo.rs` module doc |
| 26 | **The signature table names A§4.5's vendors only.** SteamDB's rules (the source of row 23) name more folder-visible anti-cheat: BlackCipher (`BlackCat64.sys`), NetEase's (`NeacSafe64.sys`), PunkBuster, Ricochet (`Randgrid.sys`), TenProtect (`TP3Helper.exe`), HackShield, AnyBrain, and further names of three already in the table (`gameguard.des`, `*.xem`, `EasyAntiCheat_Setup.exe`, `EasyAntiCheat_x64.dll`). Each one added is a declared C1 divergence of its own. A kernel driver among them (`*.sys`) is still `anti-cheat-unknown` on a Pass B claim. | Outside L3-7's brief (A§4.5's list); the catalogue's `anticheat[]` can add any of them without a launcher release. | L (a row each), or the catalogue's moderators | `ANTI_CHEAT_SIGNATURES` |
| 28 | **A recording is the folder as detection lists it, not every byte below it.** `gi-probe capture` records the walk's folders and, around each program and Steam's hint, the program's folder, the three above it and two levels below each (`src/bin/gi-probe/capture.rs` `UP`, `DOWN`); a file's size, but the bytes only of what the rules read. So a tie Pass A breaks by the bytes below two candidates' evidence (`evidence_size`, sixteen folders deep) replays with the recorded part of each, a program found only in a folder listed around another gets no folders listed around itself, and a recording made on one OS is held to its capture only when replayed on that OS (Windows resolves a name ignoring case where Linux does not; its services are its own). None of these is silent where it matters: C1 holds every recording replayed on its own OS to what the folder answered, so a recording that does not replay as its folder fails before it can be committed. | Recording every byte, or every folder to the size walk's depth, would put file content or megabytes into a public repository for a tie no recorded game has yet; the 47 recordings on this desk replay as their folders (row 1). | L (a wider capture), when a recording fails its capture check | `tests/identity.rs` `c1_detection_and_planning_are_launcher_0_4_1s_over_the_recordings` (the capture check) |
| 29 | **A file an update's undo keeps is recorded nowhere afterwards.** Installing over an integration undoes its ledger first; a file that undo leaves (one of ours that changed, a proxy whose folder gained a loader-capable name, `kept_for_their_loader`) is dropped from every record when the new ledger replaces the old, and so is one the rollback of a failed update leaves. After a game update moved the program, the old folder's kept proxy is such a file too (L3 fix for the moved-exe update: `install` now undoes the old ledger in `placed_root`). An uninstall that keeps a file deletes the ledger in the same way, with `restore-partial` said once. | Recording kept files needs a ledger field the daemon reads and C1b holds to 0.4.1's (an update's ledger is compared field for field). | D1 (if the daemon shows them), then L | `src/install.rs` `install`, `uninstall_ledger` |
| 30 | **An uninstall deletes a backup it could not put back.** `restore_game_files` now keeps the backup of a file we replaced that could not be put back while it still holds what was backed up (the player's own, step 10a's loader), recorded in the `restored` ledger; the uninstall that follows applies the same rule (a backup goes back only over our file, M2) and then removes the profile, that backup with it. The person was told `restore-partial` ("verify the game's files in Steam"), which cannot bring back a loader of theirs. | Putting a backup back where nothing, or another file, is now is what M2 forbids; offering the file instead needs a report field and the daemon to show it. | D1 (offer it), then L | `src/install.rs` `restore_game_files`, `undo_game`; `tests/restore.rs` `a_restore_keeps_the_backup_it_could_not_put_back` |
| 31 | **On Windows a token file is private only by its folder's ACL.** `write_atomic_private` sets mode 0600 on Unix; on Windows it is `write_atomic` (attributes carried over), and the `[[config_writes]]` file holding `${bridge_token}` is as private as the ACL the profile folder inherits from below the user's own data folder, which the launcher neither sets nor checks. | Setting an explicit owner-only DACL needs `SetSecurityInfo` and a test that reads it back, for a token the same user's game reads anyway. | L (after 0.5.0), or accepted | `src/paths.rs` `write_atomic_private` |
| 32 | **The Proton unit tests race the fake wineservers they spawn.** `src/proton.rs`'s tests start a copy of `sleep` named `wineserver` (`fake_wineserver`); between its kill and its reaping it is a zombie of ours whose `cwd` cannot be read, which `wineserver_alive_in` counts as alive (it fails closed), so a test in the same binary that applies or restores a prefix at that moment is refused `game-running{reason=wineserver}`. Seen twice in the L3 fix lane's 37 mutation runs on a desk at load 80 (`a_record_of_another_section_is_kept_and_both_are_restored`, `two_spellings_of_one_prefix_are_one_record`), never in 40 reruns at rest. A zombie wineserver of a real game would block an uninstall the same way until Steam reaps it. | Not an L3 change: either `wineserver_alive_in` skips a zombie (state `Z` in `/proc/<pid>/stat`: it serves nothing and writes no registry), a change to the fail-closed rule that wants its own review, or the tests that spawn one hold a lock the prefix-editing tests take. | L (after 0.5.0) | `src/proton.rs` `wineserver_alive_in`, `fake_wineserver` |
| 33 | **A rule can name the negative folders and step around them.** `validate-descriptor` step 3 and `check` step 4 lay the spec's four negative folders wherever a rule looks for a program (`src/bin/gi-probe/descriptor.rs` `places`: the folder `programs.at` names, each program its `ext` or `suffix` selects, names the rule does not skip, Steam's hint for a place below the walk), but the folders are fixed and published. A rule whose `none` names one of their files — anchored on `winhttp.dll`, with `none = [ { listed = "dwmapi.dll", kind = "file" } ]` — is claimed by none of them and still names every real game that holds `winhttp.dll` and not `dwmapi.dll` beside its program; so is one that names the probe's own folder and program names. Only a moderator, or `--corpus` over recordings of real games, sees it. | Any fixed set of folders can be recognised by a rule written against it, and laying random ones would make the lint's verdict a draw. Refusing a `none` over the fixtures' names, or a second set of folders a descriptor cannot name, is the spec's to define (`engine-descriptor-v1.md` §5 names the four folders and nothing else). | the spec, then L | `tests/gi_probe.rs` `a_rule_that_names_a_negative_folder_steps_around_it` (passes today; flips when this closes) |
| 34 | **An entry with bit 3 must end on a SIGNED descriptor; AstraPlugins' `testdata/zip-cloaking/README.md` calls the signature optional.** `src/zip_consistency.rs` check (5), like Astra's own installer (`ops::install_plugin::refuse_inconsistent_headers` (4)), refuses a Stored or Deflated entry with a data descriptor (general-purpose bit 3) unless a descriptor WITH its signature, the record's CRC and sizes, sits exactly at its recorded end (`zip-inconsistent{reason=descriptor-missing}`). A release a reader following the README accepts can be refused here. No writer of `tests/fixtures/zip-writers/` writes an unsigned one (Info-ZIP, Python, bsdtar and jar sign theirs), and no first-party release carries a descriptor at all (57 zips on this desk, 4,816 entries, 2026-11-01). Since the L3b fix a Stored entry with bit 3 whose data holds the signature ANYWHERE is refused too (`descriptor-inside`, whatever follows it, for libarchive's skip path): stored data that is itself a zip written to a pipe (its own descriptors signed) is such data, so a release that stores one, written to a pipe, is refused; written to a file, or deflated, it is not. | A scanning reader (libarchive on a pipe) cannot find a descriptor without its signature, so an unsigned one keeps the differential the check exists for. What every reader refuses is the README's to say. | the spec (AstraPlugins `testdata/zip-cloaking/README.md`) | `tests/zip_consistency.rs` `a_stored_entry_needs_its_signed_descriptor_at_its_end`, `a_deflated_entry_with_bit_3_needs_its_signed_descriptor_at_its_end` |
| 35 | **The zip checks are the differentials known here, not a proof: what follows the central directory is not read.** `src/zip_consistency.rs` holds the entries and the directory to a tight layout (check 7, `zip-inconsistent{reason=gap}`: the first local header at offset 0, nothing between two entries, the directory right after the last entry and its descriptor), since L3b's fix: a prefix or a gap was no entry for zip 9's scan, yet libarchive's streaming reader ended the archive at a `PK\x01\x02` there (as a prefix, it listed no entry) and Java's `ZipInputStream` at one zero byte, hiding every installed entry behind it. What follows the directory — bytes between it and the end record, or in the archive comment — is still left to each reader's own search for the end: a fake second directory or end record there is read by whichever reader finds it first. | Readers of local headers stop at the directory, so those bytes are not theirs; a reader of the directory that took a fake end record would read another directory, and no vector here shows one doing so. Refusing them ("the end records right after the directory, nothing else") is a second definition of where an archive ends, which the spec, Astra's installer and the catalogue's ingest would each have to adopt. | the spec, then L and Astra together | `src/zip_consistency.rs`'s module doc; `tests/zip_consistency.rs` `bytes_outside_every_entry_are_refused` |
| 36 | **Dependabot's critical zip alert stays open on this repository: `Cargo.lock` still carries zip 8.6.0.** This crate reads zips with zip 9.2 since L3b step 2, but `launcher_031` (v0.3.1, 9aa7874) and `launcher_legacy` (0.4.1, 73bf11e) are pinned revisions that require zip 8. Neither reaches the daemon or the command-line program: `launcher_legacy` is a dev-dependency, `launcher_031` a dev-dependency and `gi-probe`'s (never a default feature; `cargo tree -e normal --no-default-features` names neither). In `gi-probe compat`, 0.3.1's zip 8 reads only a zip this launcher has found one archive: compat's step 1 asks `GiPackage::open` first and stops at `zip-inconsistent`. (Astra's own `Cargo.lock` carries zip 8.6.0 for launcher 0.4.1 only; launcher 0.5.0 takes Astra's zip 9.2.) | The pins are canaries (C1, C1b, compat): a moved pin compares against itself. No zip 8 carries the fix (9.0.5 is the first). | the owner: dismiss the alert for `Cargo.lock`'s zip 8.6.0 as "vulnerable code is not used in production" (GitHub, his account); nothing waits on it | `tests/gi_probe.rs` `compat_refuses_a_zip_that_reads_as_two_archives_before_031_reads_it` |
| 40 | **Strict JSON and TOML are not Python's.** serde_json refuses `NaN`, `Infinity` and a lone surrogate, which Python's `json` accepts: such a GLB, `.gltf` or `astrachar.json` is `content-format{reason=json-invalid}` (`bundle-manifest` for an `astrachar.json`) here and judged by the spec's checker. Nesting is the spec's own since AstraPlugins #129 (a glTF's JSON at most 128 deep, `json-too-deep`), but an `astrachar.json` nested deeper than 128 is `bundle-manifest` here, where the reference reads one up to Python's recursion limit (about 1,000) and fails past it as unreadable (`content-format`). A bundle's `character.toml` is read by the toml crate (TOML 1.1, at most 80 nested tables and arrays) where the reference uses Python's `tomllib` (TOML 1.0): a settings file using 1.1 syntax passes here and is refused there, one nested 80 deep is refused here only. The Unicode tables of step 4's names (NFC, the case folding) are the crates' and Rust's, the reference's Python's, which may be of another Unicode version: a newly assigned character may fold differently. | Failing closed where it matters (a value no exporter writes); the settings file and the Unicode tables are the narrow differences two independent readers of one contract have. Run over every content golden and our 146 judged fixtures and near misses (2026-11-01; the 8 refused at parse are not the content check's), the launcher and `check-games-goldens.py` agree on all. | the spec (say RFC 8259 JSON, TOML 1.0), else accepted | `src/content.rs` `strict_value`; `src/content/bundle.rs` `ingest` |
| 41 | **A pack for `${astra_pack_inbox}` is held to its uid only.** `GiPackage::open` reads each `.astrapack`'s `animpack.toml` (at its root or in its one top-level folder, as the engine's `find_pack_root` looks) for `[animpack] uid = "<integration id>/<tail>"`, the claim the tombstone is named after (`pack-inbox-invalid{reason=uid}` otherwise). The other claims the engine's importer holds an inbox archive to (astra-companion `config/animpack/ops/inbox.rs` `check_claim`: `owner = "integration:<id>"`, `games = ["<id>"]`, a dotted `version`, no `patches` or `follows`, no `captured-` row; `IMPORT_CAPS`) are not asked: a release that breaks one installs, and the engine sets the archive aside in the inbox's `.refused/` on every machine. | Each copy of an engine rule here is a second definition that drifts from the engine's; only the uid is needed to act (the tombstone). Asking the rest belongs to `gi-probe check`, where an author sees it, once the engine's rules are a published contract. | L (after 0.5.0), with the engine's owner: the claim rules as data both read, or `gi-probe check` running the engine's own check | `src/pack_inbox.rs` `read_uid` |
| 42 | **A pack's hold is a profile path.** An uninstall writes a pack's tombstone only when no other profile's hold in the inbox folder names it and that profile still holds a ledger (`src/pack_inbox.rs` `held_elsewhere`). A profile on a drive that is not mounted, or moved by hand, reads as gone, so another profile's uninstall tombstones a pack it still uses (a reinstall or repair of it delivers the pack again); a moved profile's old hold is never removed (it holds nothing). And two launchers in two profiles of one integration at once — one uninstalling, one installing — can interleave so the install's archive lands before the uninstall's tombstone: the companion imports, then trashes it. | The launcher sees one profile at a time; the profiles of an integration are the daemon's to know. The race needs two Plays of one integration's install and uninstall within milliseconds. | D (the daemon serialises installs per integration, or passes the profiles it holds), else accepted | `tests/pack_inbox.rs` `a_pack_another_install_holds_gets_no_tombstone` |
| 43 | **A restore leaves the packs in the companion.** `restore_game_files` (a withdrawn integration) puts the game's files back and keeps the profile, its `pack_inbox` record and its hold: the packs the integration handed the companion stay installed there until the uninstall, which writes their tombstones. | Whether a withdrawn integration's animations leave the companion at once is the withdrawal policy's (marketplace plan A§4.4), which names game files only. | plan owner, then L | `src/install.rs` `restore_game_files` |
| 44 | **The companion's design says `<uid>.remove` (§2), its engine reads `<uid tail>.remove`.** `docs/animation-packs.md` §2 lists "`<uid>.remove` tombstones" in `inbox/packs/<integration>/`, and a uid holds a `/` (`astra.peak/climb`); §5 (b) ("the launcher writes `astra.peak/climb.remove` there") and the engine (`inbox.rs`: `<tail>.remove` trashes `<integration>/<tail>`) agree on the tail, which is what the launcher writes. The tail is the uid's as the archive's `animpack.toml` says it, never the archive's file name: §5 (b)'s own example is `peak-climb.astrapack` with uid `astra.peak/climb`. | Not the launcher's text. | astra-companion (one word in §2) | `tests/pack_inbox.rs` `uninstall_writes_the_tombstone_into_the_inbox` |
| 45 | **Astra passes no companion root yet: every release that uses `${astra_pack_inbox}` is `pack-inbox-unavailable` in the daemon.** The daemon plans with `plan_install` / `plan_install_in`, which name none. | Lane D's: the daemon must call `plan_install_with_user_pack_root` with `CompanionLocation`'s pack root (below, "For the daemon"). Nothing first-party uses the placeholder yet. | D | `tests/pack_inbox.rs` `without_a_root_the_plan_is_refused` |
| 47 | **The content check bounds its work by the content catalogue's caps alone, never the release's.** Since 0.5.1 a release's every file is read by its bytes, each embedded bundle walked within the content contract's limits (4,096 entries, 128 MiB) and each game archive within §2.7.1's (65,536 entries, 512 MiB, at most 100 : 1 per deflated entry), the glTF JSON of the whole release within 32 MiB and the per-character sniff within 64 MiB. 0.5.0 also counted every `.astrachar` it unpacked against the release's own 1 GiB (`Limits::max_uncompressed_bytes`, `GiPackage::open_with_limits`); no longer: a release of many archives each up to 512 MiB makes the check inflate each of them in turn (one at a time, so memory stays one archive's), as the spec's own checker does. | One budget across a release would refuse a release the catalogue accepts. Every file inside is read once and dropped, so only the time grows with the number of archives; `gi-probe check` runs on the author's machine, the daemon on a developer's own import. | plan owner (accepted unless a release-wide cap is asked for, which the spec would have to adopt) | `src/content.rs` `judge`; `src/content/zipwalk.rs` `BUNDLE`, `GAME_ARCHIVE` |
| 48 | **The built-in BepInEx data reads a profile otherwise than 0.5.0 did, in fifteen declared rows.** Since 0.5.1 C the player's mods are found through the built-in fragments (`src/mods/builtin/*.mods.toml`, held member for member to the specification's), so where `unity.mods.toml` and §7 read otherwise than 0.5.0's hand-written code the data wins. Fewer or more mods: `BepInEx/patchers` is not the mods root (a profile of patchers alone is no source); a plugins folder holding no `*.dll` (textures another mod reads, an empty folder, and outside a manager's profile one switched off as `*.old`, a manager's profile picked from a custom data folder included, since a picked path a manager's name is not on is read as `folder`) is a mod (`dir-per-mod` says "a folder directly in `root`: it is there"), and where that is all a source holds, **a source appears** that 0.5.0 never listed: offered when the integration can, so a Play with no answer yet asks (`mods-choice-needed`) where 0.5.0 played without asking. No source: a BepInEx 6 IL2CPP that never ran (no `BepInEx/interop/` yet), a BepInEx of the other Unity flavour (`codes`), one beside a game whose flavour nothing names, a preloader without its library, the game folder's BepInEx beside a program below the folder detection runs on (`{source}` is that folder, §7.3); on a file system that tells case apart (Linux), **a `BepInEx/` or `BepInEx/core/` spelled in another case** (`bepinex/`, `BepInEx/Core/`: `stat` is the exact spelling, §7.4, where 0.5.0 and Proton ignore case); and **a link** (§7.3 refuses every one): a mod folder or loose dll that is a link is no mod, a `BepInEx/` that is one recognises nothing. Another loader or version: in an IL2CPP game, **a profile holding both BepInEx 5's and BepInEx 6 IL2CPP's cores** is read as BepInEx 6 (its version and verdict; 0.5.0 read BepInEx 5, never offered there); **a `BepInEx.dll` (`BepInEx.Core.dll`) with no `ProductVersion`, or an empty one, is read by its `FileVersion`** (`key = ["ProductVersion", "FileVersion"]`; 0.5.0's version was unknown, so its verdict was `loader-unsupported` and is now the version's); **a version holding control characters is shown without them** (and `have` too). Another verdict: **more than 1,024 mods in the root** (§7.8; or a cut the discovery's own budget makes, row 49) lists nothing and says `not-supported{reason=over-budget}`, where 0.5.0 counted them (and 0.5.0 listed only the first 4,096 entries of a folder, so a folder of 4,097–5,000 entries counts otherwise; a folder past 5,000 is over budget); and **an IL2CPP integration declaring a build other than 788** offers no BepInEx 6 of the player's, whatever its build (`not-supported{reason=outside-versions}`: the data's `versions = "=6.0.0-be.788"` does not hold for the declaration's own build, which may narrow it, never widen it, §7.4), where 0.5.0 offered the declared build. Inside a manager's profile the adapter reads a folder switched off as the manager's own state (§7.7), as 0.5.0 did. **A source 0.5.0 listed and 0.5.1 does not (letter case, links, the rest above), or now refuses (over budget, another build), is refused at Play when the player chose it "with" under 0.5.0** (`mods-source-missing{reason=no-preloader}`, `mods-unsupported{reason}`). | The fragments are the specification's (§7.11): the launcher may not widen them, and §7.3/§7.4 say a link is refused and `stat` is the exact spelling. The interop row costs the most: a player who installed BepInEx 6 and never started the game modded is not offered "with your mods" until one start without Astra writes the folder; the letter-case row costs Linux players whose profile a tool wrote in lower case. | the spec: say whether `dir-per-mod` should name what a mod folder must hold (a `*.dll` below it), whether `BepInEx/patchers` is a second root, whether a never-run BepInEx 6 (no `interop/`) is recognised, and whether the BepInEx paths should be `child` lookups (case ignored, as Proton and Wine resolve them) rather than `stat`; then L re-vendors, copies the new data into `src/mods/builtin/`, and drops the rows it closes | `tests/mods_identity.rs` `Row`, `mods_discovery_answers_as_launcher_0_5_0_over_the_corpus` (each row counted, with a floor; `CaseVariant` where the file system tells case apart, `Link` on Unix); `tests/mods.rs` `an_il2cpp_integration_at_a_build_outside_the_data_is_offered_with_no_source` |
| 49 | **Discovery lists every profile a manager keeps for the game; §7.8 evaluates the active one per start.** The install sheet's list is 0.5.0's: every r2modman, Thunderstore Mod Manager and Gale profile found for the game, each evaluated on its own: each within its own §7.8 bounds, and every entry each lists and byte each reads charged to the discovery's one budget (`src/mods.rs` `BUDGET_ENTRIES`, `BUDGET_BYTES`: 200,000 entries, 256 MiB, as 0.5.0's), so a source the budget cuts short is shown `not-supported{over-budget}` and none after it is looked at (`mods::tests::every_evaluation_is_charged_to_the_discoverys_one_budget`). A start evaluates the one source the player chose. §7.3 and §7.8 speak of "the one profile folder that a launcher adapter reports as the manager's **active** profile"; no adapter reads which profile a manager has active. | Reading a manager's active profile is new adapter code per manager, and listing only it would hide the other profiles the player has today. | plan owner (accept the list, or ask for the active-profile adapters), then L | `tests/mods.rs` `thunderstores_table_names_each_managers_folder_for_the_game` |
| 50 | **Only the built-in descriptors' `mods` is asked, and a placement binds no `as` capture.** Discovery reads the built-in data for a game a built-in rule names (`engines::builtin_placement`); a game Pass B places gets no loader from its own descriptor's `mods` (§7.1 allows exactly that one), and `builtin_placement` binds `{program}`, `{dir}`, `{name}`, `{stem}` and, from a strong rule, the project placeholders, never a rule's `as` captures (`{data}`). No built-in `mods` path names a capture. | A Pass B game's mods need the descriptor that placed it handed to discovery (the daemon holds the `EngineSet`; `discover_mods_in` takes none) and the evaluator to return a placement's captures. | L (with the daemon passing its `EngineSet`) | `src/engines/eval.rs` `builtin_placement`; `tests/mods.rs` `a_game_a_weak_rule_places_binds_no_project_and_finds_no_unreal_mods` |
| 51 | **An Unreal source is shown; its start is the runner's own.** UE4SS 3 is chained by astra-reshade 0.3.1's loader itself, which finds the player's UE4SS and sets `UE4SS_MODS_PATHS` in-process: the launcher hands it no `entry` and no `tell` from the data, an Unreal runner declares no `[integration.mods]`, so its sources say `integration-without-mods` and the choice of "with" is not asked. UE4SS's own log is not read (`last_played` is `None`). | The runner's chain predates the data; moving its UE4SS lookup onto the launcher's evaluation is a runner change (astra-reshade) and a manifest table for Unreal. | astra-companion-5f (runner), then L | `tests/mods.rs` `a_ue4ss_3_of_the_players_is_a_source_astra_chains` |
| 52 | **MelonLoader and three BepInEx 6 builds are recognised by code, to be shown.** MelonLoader, BepInEx 6 for Mono and for CoreCLR, and BepInEx 6 IL2CPP under its preloader's older name (`BepInEx.IL2CPP.dll`) are not in the built-in data; `mods::other_bepinex_at` and `melonloader_at` recognise them exactly as 0.5.0 did, only so the player is told why "with your mods" is not offered. None is ever offered, chained or read for a version that decides anything. | §7 has no closed value for "recognised only to refuse" beyond `coexist = []`, and MelonLoader is "Open" (§7.11). | the spec (a loader with `coexist = []` for each, in the built-in fragments), then L deletes the code | `tests/mods.rs` `a_bepinex_in_the_game_folder_is_the_manual_source_and_melonloader_is_shown_not_offered`; `tests/mods_identity.rs` (kinds `bepinex6-mono`, `bepinex6-coreclr`, `melonloader`, floors) |

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
`AntiCheatMarker`s (they deserialise as they are) and hand them to `detect_with_engines`,
`plan_install_in` AND `install_in` (in place of `install`) through
`EngineSet::with_anti_cheat_markers`; a set built without them loses `listed-anti-cheat`
silently, and `install` alone never sees a marker added after the consent sheet. Planning a
descriptor's code also refuses what its claim carries (`anti-cheat-unknown`, the descriptor's own
markers), so a plan from a pick — developer mode's "Test on this game", a moderator's `games[]`
row — is refused where `Detection::anti_cheat` names one. `Detection::anti_cheat` then carries every signal for the game's
row; each code the launcher emits is `ANTI_CHEAT_CODES`, and `listed-anti-cheat` and
`kernel-anti-cheat` are new codes in `codes::ALL` (K15: words in every locale). The online-game
offer rule reads `online_game(appinfo, appid)` (`Some([])` known and none, `None` not known), or
`store_categories_of` + `online_codes` for a library in one pass; the daemon spells no category
itself (K16).

`Manifest::parse` refuses only what would make an install wrong (`engine-undeclared`,
`needs-newer-astra`, `loader-policy-conflict`, `unknown-key` in `[integration.loader]`). What a
release must also pass before anyone is offered it — a community id or name reading as Astra's
(`reserved-id`, `reserved-name`), `${bridge_token}` handed on without the capability
(`capability-undeclared`), Link v1's limits (`link-invalid`), an `[integration] sdk` that is no
usable URL (`manifest-invalid{key=integration.sdk}`; parse drops it, `Manifest::sdk` is then
`None`), the descriptor's Pass B admission — is `Manifest::lint(first_party)`, which a catalogue entry has passed at ingest (`gi-probe check`).
A developer-mode folder import has passed nothing: call `Manifest::lint(false)` on it and show
what it returns. Every code the launcher can return is `codes::ALL` (K15).

`zip-inconsistent` (L3b step 2) is new in `codes::ALL`: a release zip that two readers read as two
archives, refused by `GiPackage::open` before anything is read out of it (`src/zip_consistency.rs`;
`reason` says which check). K15 needs its words in every locale. It is what Astra's own installer
refuses the same archives for (`ops::install_plugin::refuse_inconsistent_headers`), so the two
may share a sentence.

`[[integration.content]]` (L3b step 3, `crate::content`): seven new codes in `codes::ALL`, the
spec's `E_CONTENT_*` (`content::SPEC_CODES`), K15 words for each. Since 0.5.1 (AstraPlugins #129)
fifteen more, the content catalogue's own codes for an embedded bundle and a game's own archive
(`content-too-large`, `content-file-outside-parts`, `zip-end-record`, `zip-headers-inconsistent`,
`archive-too-many-entries`, `archive-encrypted`, `archive-method`, `archive-link`,
`archive-name-unsafe`, `archive-too-large`, `archive-ratio`, `archive-must-be-stored`,
`bundle-manifest`, `bundle-file-unlisted`, `bundle-file-digest`), K15 words for each; a part in a
game's archive is named `<archive path>!<inner path>`. `Manifest::parse` (so
`GiPackage::open` and every install) refuses only the table's shape (`manifest-invalid`,
`unknown-key`), a path declared twice (`content-duplicate`) and the reserved `[[integration.uses]]`
(`unknown-key{reason=reserved}`); the rest (`content-undeclared`, `content-missing`,
`content-format`, `content-licence-refused`, `content-licence-conflict`,
`content-licence-undeclared`) is `GiPackage::content()`'s, which a catalogue entry has passed at
ingest. A developer-mode folder import has passed nothing: call `GiPackage::content()` on it, as
`Manifest::lint(false)`, and show its `refusals`. Its two flags (`content::FLAG_VRM_META_UNKNOWN`,
`FLAG_ARCHIVE_UNSCANNED`, which since 0.5.1 replaces the retired `content-astrachar-unreadable`)
are notes, not codes of `codes::ALL`. The install sheet lists
`Manifest::content()` (each part's `title`, `credit` and `kind`), and a report may name a part
by its `path`. The catalogue entry's `content_confirmed` (dotted text ids, `market.rights.art`)
and the games keys in `trust.json`'s `kind_keys` (`games-index-v1.md` §3 at this pin) are the
catalogue client's; the launcher reads neither.

`${astra_pack_inbox}` (L3b step 4, `crate::pack_inbox`; astra-companion `docs/animation-packs.md`
§5 (b)): a `[[files]]` destination for the integration's own `.astrapack`s, which go to
`<user root>/inbox/packs/<integration id>/`. Plan with `plan_install_with_user_pack_root(…,
Some(<CompanionLocation's pack root>))` — the `ASTRA_USER_PACK` the daemon pairs with the engine,
`<config>/companion/pack` — wherever it plans with `plan_install_in` now; without it such a release
is `pack-inbox-unavailable{reason=no-user-root, files}` (row 45). The root need not exist (the
install makes it). Two new codes in `codes::ALL`, K15 words for each: `pack-inbox-unavailable`
(`reason` = `no-user-root` | `not-a-directory`) and `pack-inbox-invalid` (`reason`: a file that is
no pack the companion reads, its uid not the integration's). The plan carries `pack_inbox{dir,
files[{name, uid, tombstone}]}` (the install sheet may list it: "for the desktop companion"); the
install reports the progress stage `pack-inbox`; `UninstallReport::pack_inbox` says what it did
there (`tombstones`, `held` for another profile's install, `removed`, `skipped`). A ledger with
packs is schema 5. The engine imports the packs at its start and on its watcher; nothing needs
sending to it.

`mods` (0.5.1 B, `engines::mods`; `ENGINE-DESCRIPTOR.md`, "`mods`"): `mods-invalid` is new in
`codes::ALL`, K15 words for it (its `reason` is one of §7.9's, `engines::mods::reasons::ALL`; its
`field` the member). A catalogue entry has passed it at ingest (`Manifest::lint`, `gi-probe`); a
developer-mode folder import has not, and `Manifest::lint(false)` shows it. Wherever the daemon
holds a descriptor, `Descriptor::mods_refusals` non-empty means it carries no `mods` for this
client: say so once, and keep detecting, offering and planning with it. Since 0.5.1 C the
daemon's `discover_mods_in` evaluates the built-in `mods` (`docs/ENGINE-DESCRIPTOR.md`, "What
\"with your mods\" asks"), so §7.10 binds the daemon now: what an evaluation finds (which loaders, their versions, the mods, how many, the folders, the
picked one) goes to the player's install sheet and that one start only — never a log a problem
report or a support bundle attaches (not even "with mods" or "without"), never a rating, Link, the
companion's bridge, an AI tool or another plugin; a runner gets a chain's `entry` and its `tell`
alone; and the picked folder and the "with your mods" choice stay in Astra's profile on this PC,
never synced.

`ModSourceRow` (0.5.1 C; additive — every field and code a 0.5.0 row had is unchanged):

- **New field `ModSource::coexist`** (`Vec<Coexistence>`, codes `chain`, `side-by-side`,
  `via-manager`; serde `[]` when absent): how Astra runs beside the player's loader, from the
  built-in data. Add `repeated string coexist` to `ModSourceRow` and both shells' payloads.
- **New `LoaderKind` codes** `ue4ss3`, `ue4ss2`, `unreal-paks` (the data's loader ids): name each
  in the Workshop (`UE4SS 3`, `UE4SS 2`, "Unreal `~mods` paks"). `loader_version` for UE4SS is
  `UE4SS.dll`'s own; the paks have none, and no `preloader`.
- **Discovery for Unreal games**: `mods_fields` calls `discover_mods_in` for Unity only
  (`unity: bool`); call it for an Unreal row too. A UE4SS 3 source says `integration-without-mods`
  under astra-reshade's runners (they chain it by themselves, row 51): word it as "your UE4SS 3
  and its mods run beside Astra", never "Astra plays alone". `declared` stays Unity-only.
- **Verdicts with new parameters, no new code**: `loader-unsupported` for UE4SS 2 carries
  `need` = `UE4SS 3` (a loader's name, not a version: "Astra needs UE4SS 3"); `not-supported`
  carries the new `reason` `over-budget` (the folder is past one evaluation's bounds, §7.8: more
  than 1,024 mods, a folder of more than 5,000 entries; nothing of it is listed). A Play with mods
  refuses such a source `mods-unsupported{reason=over-budget}`.
- **`not-supported` has a third new `reason`, `outside-versions`** (no new code): the integration
  declares a version of the player's loader the built-in data does not hold (an IL2CPP foundation
  at a build other than 788), so no source is offered with it; a Play with mods refuses
  `mods-unsupported{reason=outside-versions}`. `over-budget` also says the discovery's own budget
  cut the look short (row 49). Word both as the generic "cannot play together with mods here".
- **A source can appear** (row 48): a profile whose only mods are folders holding no `*.dll` is
  listed now, and when it is offered a Play with no answer yet asks (`mods-choice-needed`).
- **What counts as a mod changed** (row 48): a test of the daemon that lays a plugins folder with
  no `*.dll`, or `BepInEx/patchers`, counts as the data does. Its `BepInEx.dll` fixtures must be
  real PE images with a version resource (the data's probe walks the resource directory; the
  launcher's `tests/common` `pe_with_resource` builds one), and a BepInEx 6 IL2CPP fixture needs
  `dotnet/` and `BepInEx/interop/`, in an IL2CPP game (a `GameAssembly.dll` beside the program).

## For the specification (K13)

### The goldens at db52804: `SPEC_DEFECTS_AT_PIN`

`tests/goldens.rs` (the library) and `tests/gi_probe.rs` (`gi-probe check`) hold AstraPlugins'
goldens, vendored at `tests/goldens/astraplugins/` (`PIN` db52804689bb7ed785c67cac9dc31b15da590477,
a commit of AstraPlugins master), to this launcher's parser (K17) and `reserved.yaml` to
`src/reserved.rs` (K13). These are the goldens the launcher answers otherwise than they are
labelled. Each is an entry of `SPEC_DEFECTS_AT_PIN` (`tests/common/spec_goldens.rs`) that
asserts the disagreement still holds, so a golden fixed at a new pin turns the tests red until
its entry goes. An answer is the refusals `(code, field)` in order; a reason's text is never
pinned. Re-pin with `tools/vendor-astraplugins-goldens.sh <checkout> <commit>`.

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

- **§5's hostile corpus has no too-broad rule that looks below the top.** The one nested rule in
  `testdata/games/descriptors/hostile/` (`hijack-updates-code-squatting`, `{project}/bin/win64`)
  is anchored on `engine2.dll` and refused for its code; a too-broad one whose `programs.at` names
  a nested folder (the spec's own Source 2 example's) passed `gi-probe` at launcher 0.5.0 L3,
  which laid the negative folders flat. `tests/descriptors/hostile/negative-folders-nested.toml`
  is one, refused since L3b step 1: copy it, and say in §5 that a negative folder is asked
  wherever a rule looks for a program.
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
  character: metadata, so another value never refuses the manifest (parse drops it); `gi-probe
  check` reports it (`manifest-invalid`, key `integration.sdk`).
- **`code_changed`** (`gi-probe check --previous`) counts `.dll .so .dylib .jar .lua .py .js .exe`
  and also `.asi .pyd .pyc` and a versioned `.so.N`, in `[[files]]` and `[[game_files]]` alike.
- **Pass B's anchor is wholly literal** (`ENGINE-DESCRIPTOR.md`, "Anchors"): §2.4 counts a
  `listed` pattern holding a placeholder (`{stem}_Data`) as an anchor once substituted; the
  launcher refuses a rule with no literal one (`descriptor-too-broad{reason=no-anchor}`,
  `src/engines/descriptor.rs` `check_pass_b`). Say the launcher's reading in §2.4, or the
  launcher learns to key its prefilter on a placeholder pattern's literal part.
- **`testdata/zip-cloaking/README.md` lists three vectors; the launcher refuses more**
  (`src/zip_consistency.rs`, as Astra's installer does): a descriptor without its signature
  (row 34); a Deflated entry with bit 3 not followed by its signed descriptor; a stored entry
  with bit 3 whose data holds a descriptor signature ANYWHERE, whatever follows it; the Unicode
  Path field in either header whatever its CRC (zip keeps it only when the CRC matches); a
  non-ASCII name not flagged UTF-8 in both headers, or flagged and not UTF-8 (zip 9 reads
  unflagged UTF-8 as UTF-8, zip 8 and Windows in a code page); a deflated entry whose output's
  CRC is not the record's; and any byte outside the entries before the central directory (a
  prefix, a gap between two entries or before the directory: libarchive's streaming reader ends
  the archive at a `PK\x01\x02` there, Java's `ZipInputStream` at a zero byte). Each is
  `zip-inconsistent{reason}` here. Add them to the README's table, with a vector each, so every
  reader refuses the same archives. **The README's `stored-dd` rule is not enough**: "a
  descriptor EXACTLY at the entry's recorded end whose CRC and sizes equal the central record"
  leaves a bare `PK\x07\x08` inside the data, with any CRC and sizes behind it, and libarchive
  3.8.10's skip path (`tar -t`, every entry a selective extract passes over) ends a stored
  length-at-end entry at the first one whatever follows it (its read path asks for the CRC; the
  launcher's first rule did too). A local header behind it can then hide an installed entry from
  that reader (`tests/zip_consistency.rs`
  `a_bare_signature_that_hides_an_installed_dll_from_a_streaming_reader_is_refused`). The rule
  every reader can keep: no signature anywhere in a stored bit-3 entry's data.
- **`reserved-name`'s fold** is not NFKC (row 17), and it reads a name word by word: the spec's
  fold keeps the separators, so `Mini Cell` and `Gemini Celestial` are not reserved, while a run
  of one- and two-letter pieces (`O f f i c i a l`, `Mi-ni-ce`) is read as one word. Say how
  separators fold.
- **`[[integration.content]]`'s codes are this launcher's spelling** (row 13's rule, merged plan
  R32): `E_CONTENT_UNDECLARED` is `content-undeclared`, and so on for the spec's and the content
  catalogue's (`E_ZIP_HEADERS_INCONSISTENT` is `zip-headers-inconsistent`, …;
  `content::SPEC_CODES`, held to every code and flag `vectors.json` names); the two flags are
  `content-vrm-meta-unknown` and `content-archive-unscanned`. `manifest.md` §2.7's own table
  already spells one so (`content-duplicate`). `gi-probe check` raises `content-duplicate` at
  step 1 (the launcher's parse refuses it) and the rest at step 8.
- **Where the reference's own reader raises, the gate refuses `E_CONTENT_FORMAT`** (§2.7.3: "any
  bundle shape that reader still raises on"), and which shapes those are is Python's: a truthy
  `body` or `character` that is no object, a `files` that is a list or a string, an
  `extensionsUsed` holding an object, a directory record cut short, a deflate stream that is no
  deflate stream. The launcher answers each as the reference does
  (`content-format{reason=bundle-unreadable}`, `{reason=archive-unreadable}` for a game archive),
  never a crash; say them in `ingest-v1.md` as refusals of their own.
- **A part's `path` pattern lets `..` through after a newline**: `^(?!/)(?!.*\.\.)[^:\0]+$`'s
  `.` does not cross a line, so `a\n../b` passes the schema. The launcher refuses a `..` anywhere.
- **The reserved `[[integration.uses]]`'s golden writes `version_req = "^1.0"`**, a caret range
  the content catalogue's contract (AstraPlugins PR #127: canonical ranges only, `=1.0.0` for an
  exact version) refuses. When `uses` is specified, its `version_req` should be that grammar, and
  its `id` the index entry id (`games-index-v1.md` §3a). At this pin no other table of a game
  integration's manifest names a catalogue item: `[integration.foundation]` stays a claim by
  `[integration] id` (§2.4), so the launcher has no reference to read yet.
- **`manifest.md` §1 says "No new `${var}` placeholder"; `${astra_pack_inbox}` is one** (L3b
  step 4): a `[[files]]` destination, not a caller's variable (as one it is `unknown-placeholder`
  here too), which launcher 0.3.1 refuses as it parses the manifest (`unknown-placeholder{name}`,
  so before Play), so a release that uses it needs `min_astra` 0.2.8 (`games-index-v1.md`'s entry)
  and must not reach Astra 0.2.7's users; `gi-probe compat` fails it at step 1 with
  `needs-newer-astra{min_astra=0.2.8, placeholder=astra_pack_inbox}`. Say so in §1, and add the
  destination to `manifest.schema.json` and a golden: what may go there (one `.astrapack`
  straight in the folder, its `[animpack] uid` the integration's, an id the companion reads,
  `[a-z0-9._-]{1,64}` not dot-led) and the codes, `pack-inbox-invalid{reason}` and
  `pack-inbox-unavailable`.
- **The built-in Unity fragment (`unity.mods.toml`), read against real profiles** (0.5.1 C, debt
  row 48; each a declared row of `tests/mods_identity.rs`). Its comment says r2modman and
  Thunderstore Mod Manager disable a plugin by renaming it `*.dll.old`, "which the glob simply
  does not match": that holds for a loose dll only. They switch a FOLDER mod off by renaming the
  files inside it, and the folder stays, so `dir-per-mod` reads it as enabled; so does a folder
  holding only textures another mod reads, or nothing. The launcher reads the switched-off folder
  through the adapter (§7.7) inside a manager's profile, and as the data says anywhere else.
  `BepInEx/patchers` (preloader patchers, loaded by the same BepInEx) is no root of any loader. And
  `{ stat = "{source}/BepInEx/interop", kind = "dir" }` refuses a BepInEx 6 IL2CPP that never ran:
  the interop assemblies are generated at its first start, so a profile r2modman just made is not
  recognised. Say which of these the fragment means; a `dir-per-mod` member naming what a mod
  folder must hold (`holds = "*.dll"`, below it) would close the first two. Its `require` looks
  the BepInEx files up with `stat`, the exact spelling (§7.4), where Proton and Wine resolve a
  name ignoring case: on Linux a profile whose `BepInEx/` or `BepInEx/core/` a tool wrote in
  another case loads under Proton and is not recognised. Say whether those lookups should be
  `child` (case ignored, a listing per folder, as `version`'s `pe` and `root` already resolve).
- **`mods` (§7): what L3b asked for is in the spec at db52804** (§7.1, `$defs/mods`, the compat
  goldens). What the launcher reads otherwise than the checker (`tools/check-games-goldens.py`
  `mods_reasons`), each a launcher-only note in [`ENGINE-DESCRIPTOR.md`](ENGINE-DESCRIPTOR.md):
  the checker counts a component's 255 in characters where §7.3 says bytes; its `$` lets a code,
  id, template or `versions` end in a line break; it names no reason for a loader `name` outside
  §2.1's characters, a `tail` without `equals`, a `require` entry that is no table, or an
  `any`/`all`/`none` holding more than its list; and its `fnmatch` reads `?` and `[…]` in a
  detection name as wildcards, and its `.lower()` folds case beyond ASCII, where §7.4 says only
  `*` and placeholders are wildcards and case is ignored as ASCII's. Say each in §7, and
  give the checker the same reading; a differential over 28,514 mutated goldens at 0.5.1 B
  found no reason the checker names that the launcher does not.

### The grammar

The launcher's reading of the parts of the grammar the specification leaves open (display names,
codes, `strong-fallback`, JCS, how goldens pin, the lint's codes, the parts only Astra's own
descriptors use) is in [`ENGINE-DESCRIPTOR.md`](ENGINE-DESCRIPTOR.md), "Launcher-only notes":
each is also a request to the specification to say the same.
