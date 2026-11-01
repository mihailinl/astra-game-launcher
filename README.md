# astra-game-launcher

Puts a game-integration mod into a game and launches the game with it. Think
r2modman/Thunderstore, minimal, with no UI: one integration per game, installed from a
verified `astra-gi.zip`, launched with the arguments its manifest declares, and removed
without a trace.

It is a Rust library (`astra_game_launcher`) and a command-line program
(`astra-game-launcher`). **Used by [Astra](https://github.com/mihailinl/Astra)**, the desktop AI
companion, to bring her into games. The manifest format is specified in astra-bepinex's
[`docs/GAME-INTEGRATION-MANIFEST.md`](https://github.com/mihailinl/astra-bepinex/blob/main/docs/GAME-INTEGRATION-MANIFEST.md).

This first version supports **Unity games** (Mono, via BepInEx + Doorstop) on Windows and on
Linux under Steam's Proton. It detects Unity IL2CPP, Unreal and Godot games but installs
nothing engine-specific itself: everything game-specific is data in the manifest.

## Command line

```
astra-game-launcher detect <game-dir> [--hint <rel-exe>] [--appid N] [--steam-root <dir>] [--json]
astra-game-launcher fetch <url> --sha256 <hex> -o <file> [--json]
astra-game-launcher inspect <zip> --sha256 <hex> [--json]
astra-game-launcher plan <zip> --sha256 <hex> --game <dir> --exe <rel> [--appid N]
                         [--proton-prefix P] [--platform windows|linux-proton|linux-native]
                         --profile <dir> [--json]
astra-game-launcher install <same as plan> --yes [--json]
astra-game-launcher launch --profile <dir> --game <dir> --exe <rel> [--appid N]
                           [--proton-prefix P] [--steam <path>] [--var k=v]… [--vars-stdin]
                           [--dry-run] [--json]
astra-game-launcher uninstall --profile <dir> --game <dir> --exe <rel> [--proton-prefix P] [--json]
astra-game-launcher mods --profile <dir> --game <dir> --exe <rel> [--appid N]
                         [--choose without | --choose with --source <root>] [--json]
```

- `detect --appid N` asks Steam's `appcache/appinfo.vdf` which program Steam launches for the
  app and uses it when it passes the engine rules below; `--hint` names that program directly.
- `plan` prints the consent sheet and changes nothing. `install` needs `--yes`.
- On Linux, a game whose exe ends in `.exe` runs under Proton; its prefix defaults to
  `<library>/steamapps/compatdata/<appid>/pfx`.
- `launch --dry-run` prepares everything (the INI keys, the Proton override) and prints the
  command without starting it. `--vars-stdin` reads `k=v` lines or a JSON object, so a token
  never shows in `ps`.
- A Steam game starts as `steam -applaunch <appid> <args…>`; any other game starts as its exe.
- Every error prints `{"code": …, "params": {…}}` on stdout and exits 1. The codes are stable
  (`astra_game_launcher::codes`); the English text on stderr is for people only.

Example:

```
astra-game-launcher fetch https://github.com/mihailinl/astra-bepinex/releases/download/v0.5.0/astra-gi.zip \
  --sha256 7b58b749517c2ed745b2942f2e860df5f4e5af729185371a7a7aebcb9f25ae67 -o astra-gi.zip
astra-game-launcher plan astra-gi.zip --sha256 7b58b7… --game ~/.steam/steam/steamapps/common/PEAK \
  --exe PEAK.exe --appid 3527290 --profile ~/.local/share/astra/games/3527290
```

## Library

```rust
let pkg = GiPackage::open(&zip_bytes, sha256)?;                  // verify, check, read manifest
let plan = plan_install(&pkg, &game, &profile_dir)?;             // the consent sheet, as data
install(&pkg, &plan, &game, &Ctx::silent())?;                     // exactly the plan, or nothing
let cmd = prepare_launch(&profile_dir, &game, &vars, steam)?;     // INI keys, Proton override
launch(&cmd, &StdSpawn)?;                                         // or the caller's own `Spawn`
uninstall(&profile_dir, &game)?;                                  // everything back
```

HTTP (`Fetch`) and process spawning (`Spawn`) are injected. Build with
`default-features = false` to drop the command line and its HTTP client. Nothing is async.

Every refusal is a `LauncherError { code, params }`; the codes are stable
(`astra_game_launcher::codes`) and a caller puts them in its own words. Added in 0.3.1:

| code | from | params | what the person can do |
|---|---|---|---|
| `switched-off-loader` | `plan_install`, `install` | `file` (the `doorstop_config.ini`), `target` (as that file writes it) | Another mod loader (BepInEx) in the game folder is switched off, but its settings file still switches it on: installing would bring its mods back on every start of the game. Set `enabled = false` in that file, or delete it, then install again. |
| `shared-loader-changed` | `prepare_launch` | `file` (the loader proxy) | The mod loader shared with another mod manager was replaced by one that does not take this integration's launch options (it used to be `foreign-loader`). Update that mod manager, or remove its loader and install again. |

`foreign-loader` (`file`) now also comes from `plan_install` / `install` for another Doorstop
beside the game under another name than ours (`version.dll`, `winmm.dll`).

`UninstallReport::kept` can now name a loader proxy we added that another BepInEx runs through,
and `UninstallReport::proton_overrides_kept` the Wine overrides left for it. `Ledger` gains
`loader_baseline` (absent from older ledgers, ignored by older launchers; the schema number does
not change).

## Playing together with the player's own mods (0.4.0)

An integration that declares `[integration.mods]` (astra-bepinex's manifest doc, §7) can run
Astra *beside* the player's own BepInEx — their mod manager's profile or a BepInEx they installed
in the game folder — instead of in Astra's separate one. The table lives inside `[integration]`,
which every launcher reads with no key check, so a launcher before 0.4.0 installs the same
release and plays Astra on its own. This one never refuses a manifest for that table either: one
it cannot use (an unknown key, an unknown loader, a path outside the profile) only means "not
with mods" (`Manifest::mods`).

```rust
let sources = discover_mods(&game, &profile_dir);                 // pure reads, newest first
let pick = default_mods_source(&sources);                         // the newest one offered
set_mods_choice(&profile_dir, Choice { choice: ChoiceKind::With,
    source: pick.map(ChoiceSource::of), added_roots: vec![] })?;  // asked once per game
```

- **Where.** r2modman (`%APPDATA%\r2modmanPlus-local`, `$XDG_CONFIG_HOME` or `~/.config`),
  Thunderstore Mod Manager (`%APPDATA%\Thunderstore Mod Manager\DataFolder`), Gale
  (`%APPDATA%\com.kesomannen.gale`, `$XDG_DATA_HOME` or `~/.local/share`), every Flatpak's
  own `~/.var/app/*/config|data/…`, and folders the player added (`Choice::added_roots`: a
  profile, a folder of profiles, or a manager's custom data folder). The game's folder in each is
  `src/mods/games.tsv` (Steam appid → r2modman's folder, Gale's community), generated from
  Thunderstore's ecosystem schema by `tools/gen-mods-games.py`; for a game newer than the table,
  the Steam install folder's name, or a profile whose `BepInEx/LogOutput.log` names the game's
  program in its first five lines. Never a guess beyond that. `ModsEnv` says which data folders
  are looked in (`discover_mods_in` takes one).
- **What counts.** A profile whose `BepInEx/core` holds a known preloader; a BepInEx in the game
  folder no ledger of ours placed (`manual`); MelonLoader, shown and never offered. Mods are the
  first-level entries of `BepInEx/plugins` and `BepInEx/patchers` with at least one `*.dll`
  (`*.dll.old` is disabled; an entry holding `astra-item.json` is Astra's own); a source with none
  is not listed. `last_played` is the log's modification time.
- **Whether.** `ModSource::playable`: their BepInEx's `ProductVersion` (the version resource of
  `BepInEx.dll` / `BepInEx.Core.dll`) against the declaration — 5.4.x for `bepinex5`, exactly
  `6.0.0-be.<bepinex_build>` for `bepinex6-il2cpp` — or why the installed integration cannot:
  `IntegrationWithoutMods` (no table: listed, "only without mods for now"), `NotSupported`
  (`not-installed`, `platform`, …).
- **The choice.** `<profile>/mods-choice.json`
  (`{"schema":1,"choice":"with"|"without","source":{…},"added_roots":[…]}`), written atomically
  under the profile lock by `set_mods_choice` and `add_mods_root` only; its name is reserved like
  the ledger's. An update keeps it, `uninstall` removes it, so a reinstall asks again. 0.4.1:
  `add_mods_root` remembers a folder the person picked before they answered (`"choice": null`:
  no answer, Play still asks, discovery looks there; launcher 0.4.0 cannot read that file, which
  its own rule makes harmless — an answer it cannot read is no answer), and an answer keeps the
  folders already remembered; past 32 the oldest goes, so adding one never fails. Every folder —
  added, a source's root — must be an absolute drive-letter path on Windows (a mapped network
  drive or `subst` behind the letter is the user's own setting and is followed): a share
  (`\\host\share`, `\\?\UNC\…`), a device (`\\.\pipe\…`) or another verbatim namespace is
  refused (`path-escapes`, `reason` = `unc`, `verbatim-unc`, `device`, `verbatim`,
  `not-absolute`), judged on the spelling given before any filesystem call, since on Windows
  merely asking about a share opens an SMB session with the user's credentials
  (`check_local_path`). Folders are remembered without `\\?\`. The paths in the player's pack's
  `doorstop_config.ini` are judged the same way, by their text as the Windows game reads them
  (`//host/…` and `/\host\…` are shares too): a refused one is left out of the Play, which says
  so with the warning `mods-pack-path-refused`.

- **Play.** `prepare_launch` reads the choice (its signature is unchanged; `prepare_launch_with`
  takes the `ModsEnv`). Not asked yet while something could be played together:
  `mods-choice-needed`. "Without", or an integration with no table: the Play of 0.3.2, argument
  for argument — and an integration with no table never asks and plays Astra alone whatever was
  chosen. "With": the proxy beside the game *now* (ours, shared, or relinked by their
  manager) decides the spelling by its generation — Doorstop 4 `--doorstop-enabled true
  --doorstop-target-assembly <chain>`, Doorstop 3 `--doorstop-enable true --doorstop-target
  <chain>` — followed by their pack's own settings from the source's `doorstop_config.ini` (the
  Mono search path override; `boot_config_override`; for IL2CPP `--doorstop-clr-corlib-dir` and
  `--doorstop-clr-runtime-coreclr-path`, their `dotnet/` by default), a relative path looked up
  below their root and then the game folder, a Windows path passed as written, `Z:\…` under
  Proton; then today's pins (no Mono debugger; `boot.config` when the game folder's ini is not
  ours and theirs sets none). The manifest's own Doorstop options give way; its other arguments
  stay. Each path of theirs is one argv element (`steam -applaunch` is spawned with an argv,
  never a shell string; that Steam hands each one to the game whole is a live-check item, as for
  Astra's own profile path). Then `chain.ini` (`[Chain] target/source/profile`, UTF-8 without a
  BOM) at `[integration.mods].chain_config` — which must be `chain.ini` beside the chain, the one
  file the chain reads — resolved before anything is written and written under the profile lock
  once nothing can refuse the Play: the only file a Play with mods adds. Uninstall removes it and
  the chain's `last-run.txt`. Nothing of theirs is ever written: not their profile, not their
  `doorstop_config.ini`, not their proxy. A `mods-choice.json` that cannot be read (not JSON, too
  new, not a file) is no answer: `mods-choice-needed`, or Astra alone with the warning
  `mods-choice-unreadable`, never a failed Play.
- **Readers.** `mods_last_run` (the chain's `last-run.txt`: `LastRun::Ok`, `Fallback`,
  `HooksFailed`) and `loader_status` (the Unreal loader's `astra-loader-status.json`, typed; a
  `reason` or `armed_by` it does not know is `Other`, never refused).
- **Steam launch options.** `ProcessPlan::warnings` holds `steam-launch-options-doorstop` when a
  Steam user's own launch options for the game pass `--doorstop-…` (`localconfig.vdf`,
  `steam_user_launch_options`): Steam adds them to Astra's start and Doorstop may take them over
  Astra's. Both modes; never a refusal.

| code | from | params | what the person can do |
|---|---|---|---|
| `mods-choice-needed` | `prepare_launch` | `sources` (how many could be played together); `choice_unreadable` when `mods-choice.json` could not be read | Answer once (`set_mods_choice`: with which mods, or without), then Play again. |
| `mods-source-missing` | `prepare_launch` | `profile`, `manager`, `root`; `reason` = `no-preloader`, or `file` (their .NET runtime) | Their mod profile is gone or broken: play without mods, or choose another. |
| `mods-loader-unsupported` | `prepare_launch` | `have`, `need` (`5.4`, `6.0.0-be.788`), `profile` | Their BepInEx is too old (update it in their manager), or another build than the integration's: play with Astra alone, or with their mods without Astra. |
| `mods-melonloader` | `prepare_launch` | `profile` | Their mods run through MelonLoader: Astra plays together with BepInEx only. |
| `mods-unsupported` | `prepare_launch` | `reason` = `platform`, `no-doorstop` | The installed integration declares the table and still cannot play with mods here: choose "without". |

A Play with mods can also answer an existing code: `unknown-key` / `manifest-invalid` /
`path-escapes` for an `[integration.mods]` this launcher cannot use, `needs-reinstall` (`file`)
for a chain loader file that changed, `shared-loader-changed` for a proxy that cannot start the
chain (Doorstop 3 with IL2CPP). The CLI's `mods` command lists the sources and remembers a choice.

## What an older Astra left beside a game (0.4.0)

The Unreal runner 0.2.x (astra-reshade v0.2.0 … v0.2.11) put files beside the game's program that
a lost profile or a deleted ledger no longer names, and that a launcher before 0.4.0 took for the
player's. Each of them is also, byte for byte, an upstream release a player may install, so they
are recognised by exact rules only (`FormerKind`):

1. `dwmapi.dll` with RE-UE4SS 3.0.1-1164's digest (`3f11fad9…e6cc`), only when neither
   `UE4SS.dll` nor `ue4ss/UE4SS.dll` is beside it — the state in which every plain Steam start
   fails with "Failed to load UE4SS.dll". With UE4SS beside it, it is the player's: replaced with
   a backup and put back on uninstall, as always.
2. `dxgi.dll` with ReShade 6.8.0 Addon's digest (`0cee63f9…`), only when an `astra.addon64`
   beside it has the digest of one of the twelve releases' add-ons. Then that `dxgi.dll`, that
   `astra.addon64` and the `astra-reshade/` folder beside them are one set, Astra's — except
   `astra-reshade/AstraPreset.ini` (any letter case), where our add-on points ReShade's
   `PresetPath` and ReShade saves the player's tuning: theirs, like `ReShade.ini`, so it stays and
   keeps the folder. No other DLL name is ever ReShade here; a ReShade beside no add-on of ours
   (or one we never shipped), or a `dxgi.dll` the player updated, is theirs.

The digests' named source is `tests/fixtures/astra-reshade-unreal/`, every release's own
`astra-gi.toml` verbatim from its GitHub release asset (`tools/fetch-astra-reshade-manifests.sh`);
a unit test checks each constant against those files. A file the current ledger names is never
classified: the ledger governs its own files.

- **Install and update.** A game file the package places where one sits is an `add` over it
  (`existing_sha256` = its digest): no backup, and an uninstall puts nothing back. The others are
  listed in `InstallPlan::former_files` (warning `former-astra-files`) and removed by the install.
- **Uninstall.** A `replace` whose backup is one of them (a ledger a launcher before 0.4.0 wrote)
  removes ours and does not put the backup back (`UninstallReport::not_restored`); the ones beside
  the exe the ledger does not name are removed too (`UninstallReport::former_removed`).
- **Every removal** hashes the file (or the folder's whole tree) again right before it and removes
  only what is exactly what was judged (`plan-stale` for an install, left alone by an uninstall);
  nothing larger than 64 MiB is ever one of these.
- **Known limit (order).** Whether a backup is put back is judged beside the file as the
  uninstall finds it at that moment, so a ledger a launcher before 0.4.0 wrote that `replace`d
  `dwmapi.dll` (backup: UE4SS's proxy) AND `add`ed `ue4ss/UE4SS.dll` itself gets an answer that
  depends on the ledger's order (whether its own `UE4SS.dll` is gone yet). No released package
  makes that ledger; it is named here, not fixed.
- **A lost profile or ledger.** Play says `not-installed`. Installing again cleans the files above
  as described; the Unity runner's own `winhttp.dll` and `doorstop_config.ini`, found with our
  very bytes, are reused (`reuse-known-proxy`: never removed, as nothing can tell them from a
  BepInEx's own). The mods choice went with the profile, so the next Play asks again.

## Which program is the game

`detect_with(dir, hint)` (and `detect(dir)`, without a hint) decides by what sits beside a
program, never by which game it is. There is no per-game rule.

1. **Steam's launch config first.** `launch_executable(appinfo, appid, os)` reads the program
   Steam launches from `appinfo.vdf` (v28 and v29; `steam_roots()` says where Steam lives). Given
   as the hint, it wins outright when it is inside the folder and passes rule 2 or 5; otherwise it
   is ignored, and `Detection::hint_used` says which happened.
2. **Unity.** A program with `<stem>_Data/` and `UnityPlayer.dll` (or `.so`) in its own folder:
   `MiSideFull.exe` with `MiSideFull_Data`.
3. **Unity's flavour, from that folder.** `GameAssembly.dll`/`.so` beside it: IL2CPP.
   `<stem>_Data/Managed/Assembly-CSharp.dll`: Mono. The Unity version the game was built with is
   read from the head of `<stem>_Data/globalgamemanagers` (or `data.unity3d`) into
   `Detection::unity_version`, since some loaders break on some Unity lines;
   `unity_version_parts` gives its `(major, minor)`.
4. **Several pass.** The shallowest, then the one Steam launches, then the largest evidence
   folder (a bounded walk).
5. **The same idea per engine.** Unreal: `<Project>/Binaries/Win64/<Name>-Win64-Shipping.exe`
   with `<Project>/Content/Paks/`. A project that ships no `-Win64-Shipping.exe` (GTA III – The
   Definitive Edition: `Gameface/Binaries/Win64/LibertyCity.exe`) has its program in that same
   `Binaries/Win64/`: the one Steam launches when the hint names one, else the largest there
   (`Detection::exe_pick` says which). Only that folder is weighed, so a bootstrapper at the top
   (`PlayGTA3.exe`) or an installer in `Redistributables/` is never the game, and the engine's
   own `Engine/Binaries/Win64/` never counts. Godot: `<stem>.pck` beside the program.

Weaker signs answer at Low confidence: a pre-2017.2 Unity game (no `UnityPlayer`; ranked with
the others by depth), half an Unreal pairing (paks with no program take their build's flavour
from the paks' names: `-WindowsNoEditor.pak` is Windows, `-LinuxNoEditor.pak` Linux), a Godot
pck embedded in the program.
`Detection::evidence` names what decided, relative to the folder: the `_Data` folder, the
Unreal project folder, the `.pck`. [`docs/detection-survey.md`](docs/detection-survey.md) shows
the rules on real Steam libraries (`cargo run --example detection_survey`).

The engine a package is checked against at `plan` is the target program's own, by the same
rules: a launcher beside a Unity game is not a Unity game.

Since 0.5.0 these rules are data: the built-in engine descriptors
[`src/engines/builtin/`](src/engines/builtin) (`unity.toml`, `godot.toml`, `unreal.toml`), read
by the same evaluator as any descriptor below and held to launcher 0.4.1's answers by canary C1.
Where 0.5.0 answers otherwise — it follows no link, waits on no FIFO, and follows no Steam hint
naming a Windows device (`COM¹.exe`), where 0.4.1 did all three — is listed in
[`tests/corpus/divergence.txt`](tests/corpus/divergence.txt).

## Engines learned from data (0.5.0, in progress)

An engine the rules above do not know is described by data, an **engine descriptor**
(`astra.engine/1`, `astra_game_launcher::engines`): which program, and what sits beside it.
The descriptor is JSON in the signed games catalogue, or the same object as TOML under a
foundation's `[integration.engine]`; its digest is `sha256(JCS(object))`, so both spellings
have one digest.

```rust
let engines = EngineSet::build(offered);                 // (Origin, Descriptor::from_json(..)) pairs
let d = detect_with_engines(&dir, hint, &engines)?;      // Pass A, then perhaps Pass B
let plan = plan_install_in(&pkg, &game, &profile_dir, &engines, Provenance::Community, pick)?;
```

- `Detection::engine` is an `EngineCode` (`^[a-z][a-z0-9-]{1,31}$`), serialised exactly as the
  old `Engine` was: `"unity-mono"`, `"unity-il2cpp"`, `"unreal"`, `"godot"`, `"unknown"`, or a
  descriptor's code. `engine.builtin()` gives the `Engine` back for the built-in ones.
- **Narrow-only.** Pass A is `detect_with`: the built-in descriptors. Pass B (the descriptors
  in the set) runs only when Pass A says `unknown` with no evidence and no hint used, so a
  descriptor can name a game nothing recognised and never change one that was.
  `detect_with_engines` with an empty set is `detect_with`; both are held to launcher 0.4.1's
  answers by the tests (canary C1).
- Built-in codes, `unknown` and anything that reads as them are reserved
  (`engine-code-reserved`); a code belongs to the first publisher admitted with it
  (`engine-code-taken`); Pass B asks at most 64 descriptors, 4 per publisher, moderated before
  unchecked before local, and a lower standing never replaces or ambiguates a higher one.
  Two of one standing that disagree leave `engine` `unknown` and list
  `Detection::engine_candidates` (`engine-ambiguous`).
- Every community rule names a literal anchor beside the program that is neither a program nor
  a runtime every game ships (`descriptor-too-broad` otherwise); a rule whose anchor is absent
  costs nothing, on Steam's hint as in the folder search. Pass B follows no link and blocks on
  no FIFO; every lookup and byte it makes is counted (64 lookups and 8 probes per descriptor,
  1,024 lookups and 64 KiB per game), choosing its programs is linear in the walk, and over
  budget means no answer, never a slow one. Pass A reads the same way, without the budgets:
  the built-ins are Astra's own, bounded by the walk alone.
- A Pass B answer carries `Detection::descriptor_sha256`, perhaps `Detection::version`, and
  `anti-cheat-unknown` in `anti_cheat` when the walk stopped at its cap or a `*.sys` ships.
  None of these fields appear in a Pass A detection's JSON. Pass B never names a program a
  built-in rule names when asked about it alone (planning asks the built-ins first), nor one
  whose path the launcher refuses on some platform.
- **Planning asks them too: `plan_install_in`.** `[target] engine` = a descriptor's code plans
  where the descriptor Pass B would choose among the game program's own matches names it —
  never in a folder the built-ins decided — asked with the program Steam launches for the
  trusted appid as the hint, as the daemon detects; `engine-mismatch{want, got}` elsewhere. A
  program descriptors of one standing disagree about plans only with the player's `pick` of
  the manifest's code (`engine-ambiguous{want, codes}`). A foundation's own
  `[integration.engine]` must be that very descriptor (`descriptor-mismatch{want, got}`). A
  built-in code is checked as before, whatever the set holds. The plan pins the descriptor
  (`InstallPlan::engine_descriptor`, a `PinnedDescriptor`) and records the `Provenance` the
  caller vouches for (first-party: id and repository in Astra's compiled seed; community:
  anyone else's), and `install` plans again with the pin alone, its digest checked
  (`plan-stale` otherwise); the ledger records both (`provenance`, `descriptor_sha256`).
  `plan_install` is `plan_install_in` with no descriptors, first-party and no pick: 0.4.1's
  planning but for step 10a below, and a first-party plan of a built-in engine's manifest
  serialises exactly as 0.4.1's.
- **Mod loaders, for every game and every integration.** A file beside the game by a name mod
  loaders hijack-load by (`HIJACK_NAMES`: `winhttp`, `version`, `dwmapi`, `dxgi`, `d3d11`,
  `dinput8`, the XInput DLLs, `winmm`, Ultimate ASI Loader's system-DLL names, `opengl32`; or a
  name the manifest's `[integration.loader] proxies` declare) that Astra did not place is never
  replaced: `share-loader` where a Doorstop of ours can run through it, `foreign-loader`
  otherwise. So a player's UE4SS `dwmapi.dll` beside a GTA III or Unreal-runner game is
  `foreign-loader` where 0.4.1 backed it up and replaced it (step 10a); a ledger that already
  holds it as `replace` keeps that on update, with the plan warning `foreign-loader-in-backup`.
  `[integration.loader] keep_if_present` adds config names left alone, for that install only.
  Play, the Doorstop sharing and the BepInEx keep at uninstall still read the four Doorstop
  names alone.
- **What a community integration may do** (`Provenance::Community`, plan B §3): no `replace`
  arm (`foreign-file`); a loader's name only when `[integration.loader] proxies` declares it
  (`loader-undeclared`); game files only beside the program or below a folder named after its
  id, never below `BepInEx/`, `MelonLoader/`, `ue4ss/`, `Mods/`, `~mods/`; a program the built-in
  walk lists; no `[launch] env`; Proton overrides only for DLLs it places, `native,builtin` or
  `n,b`; path arguments only under `${profile}/` or `${game}/` (`community-limit{rule}`); and an
  id and name that are not Astra's (`reserved-id`, `reserved-name`).
- **A loader's identical bytes survive the uninstall.** The install records the listing of the
  folder of each hijack-named file it adds (`Ledger::proxy_folders`, at most 5,000 names);
  the uninstall leaves such a file of ours when its folder gained a name since that the ledger
  does not place (a UE4SS installed by hand after us ships the very same `dwmapi.dll`), with its
  Proton override, and says why: `UninstallReport::kept_reasons` = `folder-gained-files`. A game
  update that adds a file there keeps it too.

## `gi-probe`: checking a release before anybody installs it (0.5.0)

A second program, built only with `--features gi-probe` (it links launcher 0.3.1, which neither
the CLI nor Astra's daemon ever builds):

```
cargo build --features gi-probe --bin gi-probe
gi-probe compat --zip astra-gi.zip --source astra.unreal --tag v0.2.1 [--asset NAME]
                [--prerelease] [--sha256 HEX] [--previous PREV.zip] [--notes NOTES.md]
gi-probe check astra-gi.zip [--provenance first-party|community] [--foundation F.zip]
               [--previous PREV.zip] [--loaders LOADERS.json]
gi-probe validate-descriptor engine.toml [--first-party]
gi-probe pick --source astra.unreal --listing releases.json [--prerelease]
```

- **`compat`** holds a game integration's release to what Astra 0.2.7 users run: launcher 0.3.1
  and 0.2.7's own rules (`src/bin/gi-probe/v027.rs`, copied from tag v0.2.7 and held to it token
  for token by `tests/gi_probe.rs`). Every first-party release script runs it on the real zip.
  Its six steps:
  1. 0.3.1 opens the zip and parses its manifest;
  2. 0.2.7's `check_manifest` passes for the source (`--source`, an `[integration] id` from
     `src/bin/gi-probe/compat-0.2.7.json`, which canary K4 holds equal to 0.2.7's `pins.rs`):
     its id and repository, a runner's `[target] engine` with no appid or a game's own appid,
     the tag's version; and 0.2.7's tag rule (the source's prefix, an optional `v`, semver,
     stable unless `--prerelease`) and asset (`--asset`, else the zip's file name);
  3. 0.3.1 plans the zip exactly as it plans the zip rebuilt without its `[integration.*]`
     tables (but `[integration.mods]`), on a synthetic Steam game that fits the source — so a
     new table is only ever data 0.3.1 ignores;
  4. 0.3.1 installs that plan and prepares a launch with exactly `{bridge_port, bridge_token}`,
     the variables 0.2.7 passes (a new `${var}` fails here);
  5. with `--previous`: every file under a `**` `[[files]]` entry is the previous release's, or
     the release notes (`--notes`) name each one added or gone;
  6. with `--previous`: what 0.2.7's `consent_needed_for` weighs (id, repository, license,
     authors, `[target]`, `[launch]`, `[[config_writes]]`, the update's game files) is unchanged.
     A change is the notice `consent-sheet-expected`: 0.2.7 will show its sheet again.

  The digest and canonical-URL halves of 0.2.7's choice need GitHub's published asset, so
  `compat` checks the digest only when given `--sha256`. `pick` is that choice over a releases
  listing; `tools/fetch-first-party-manifests.sh` uses it to keep
  `tests/fixtures/first-party/` (each source's manifest as 0.2.7 would install it today) fresh.
- **`check`** is what a catalogue's ingest asks of a release before it is listed (AstraPlugins
  `spec/games/manifest.md` §4), with this launcher's own parser. Its seven steps:
  1. the launcher opens the zip (its hygiene, the manifest, every file it names, no reserved
     profile name placed — `src/reserved.rs`, the one list, canary K13);
  2. `Manifest::lint`: a community release's id and name are not Astra's own (`reserved-id`,
     `reserved-name`) and what it may place and launch (`loader-undeclared`, K18;
     `community-limit`) — `--provenance first-party` skips these —, `${bridge_token}` only with
     `[integration.capabilities] companion = ["presentation"]` (`capability-undeclared`), Astra
     Link v1's counts, names and lengths (`link-invalid`), and `[integration.engine]` a
     descriptor Pass B admits;
  3. every file `[integration.link]` names is in the zip (schemas ≤4 KiB, the skill ≤16 KiB);
     holding the schemas to `link-v1.md` §6's restricted profile waits for `link-v1` to freeze;
  4. the descriptor claims none of the spec's negative folders (as `validate-descriptor`);
  5. with `--foundation`: `built_on{id, version, verified}`, verified only when every file the
     foundation places this release places alike (destination and digest);
  6. with `--previous`: `code_changed`, every placed code file (`.dll .so .dylib .jar .lua .py
     .js .exe .asi .pyd .pyc`) new or changed;
  7. `loaders`: each game file `none`, `allowlisted` (a loader's name whose digest `--loaders`,
     the catalogue's `loaders[]`, lists as off by default) or `other` (a notice,
     `loader-other`). Labels only: the allowlist never permits what the manifest does not
     declare.

  Given a bare `astra-gi.toml` instead of a zip, it makes the steps that need no zip and says
  so.
  `tests/goldens/astraplugins/` holds the spec's goldens at the pinned commit (`PIN`,
  `SHA256SUMS`; re-pin with `tools/vendor-astraplugins-goldens.sh`), and `tests/goldens.rs` and
  `tests/gi_probe.rs` hold the launcher and `check` to them (K17), every known disagreement in
  one list, `SPEC_DEFECTS_AT_PIN`.
- **`validate-descriptor`** is the lint of the engine descriptor specification
  (AstraPlugins `spec/games/engine-descriptor-v1.md` §5): the descriptor (TOML, JSON, or a
  foundation's `astra-gi.toml` holding `[integration.engine]`) parses, Pass B admits it, and it
  claims none of the spec's negative folders (empty, an exe alone, an exe among bare system
  DLLs, `steam_api64.dll` beside an exe), each detected with it alone
  (`descriptor-too-broad`, `reason=negative:<folder>`). The report gives its digest,
  `sha256(JCS(descriptor))`.

**The report** is one JSON object on stdout:

```json
{"schema": "astra.gi-probe/1",
 "tool": {"version": "0.5.0", "source_tree_sha256": "<hex>"},
 "subcommand": "compat", "verdict": "pass",
 "input": {}, "source": {}, "against": {"astra": {}, "launcher": {}},
 "steps": [{"step": 1, "check": "…", "status": "pass"}],
 "notices": [{"code": "consent-sheet-expected", "step": 6, "detail": "…"}],
 "findings": [{"code": "release-mismatch", "step": 2, "field": "integration.version",
               "detail": "…", "params": {}}]}
```

`verdict` is `pass` or `fail`; a step's `status` is `pass`, `fail`, `notice` or `skipped` (with
its `why`). `step`, `field` and `params` appear where they apply; a finding's `code` is the
launcher's or 0.2.7's own refusal code where one refused. **Exit 0**: no findings (notices
allowed). **Exit 1**: findings. **Exit 2**: a wrong command line or an unreadable file; the
report then carries `error: {code, detail}` instead, and nothing was concluded.

## Safety rules the code enforces

- **Verified input.** A zip is used only when its SHA-256 matches the one the caller expects.
  A game file the manifest pins by digest must match it too.
- **The zip.** At most 256 MiB compressed, 1 GiB uncompressed, 20 000 entries. Symbolic links,
  absolute paths, `..`, drive letters, encrypted entries and duplicate names are refused
  outright, whether the manifest names them or not. Only the files the manifest names are
  extracted, and an entry is never read past the size it declares.
- **The manifest.** An unknown key in a section that acts (`[target]`, `[[files]]`,
  `[[game_files]]`, `[launch]`, `[[config_writes]]`, or a new top-level section) is refused, so a
  newer feature is never silently ignored. A newer schema is refused with its own code.
- **The path jail.** Files go only to `${profile}/…` and `${game}/…` (the exe's folder),
  relative, with no `..`, no drive or stream colon and no Windows device name. The jail is checked
  again on disk: parent folders are created one by one, a link anywhere on the way is refused,
  and the resolved folder must still be inside the root. Every write is a temporary file plus a
  rename. `[[config_writes]]` may only write inside the profile; a caller's variable can never
  choose a path.
- **Beside the exe.** A file already there with our digest is reused and never rewritten. A
  `doorstop_config.ini` or `ReShade.ini` (any letter case) we did not write is left as it is, and
  uninstall neither removes nor restores it: ReShade rewrites its INI while the game runs, so
  ours becomes the user's settings the first time it is saved. A foreign `winhttp.dll`,
  `version.dll`, `dxgi.dll` or `d3d11.dll` is another mod loader, and it is never replaced. When
  it is a Unity Doorstop that loads into the game's process (same PE machine as the exe) and
  understands every `--doorstop-*` option the manifest's launch passes — an option that switches
  it on and one that names the target, so the command line alone decides what it loads — it is
  **shared** (`share-loader`): the game runs through it, it is never written, backed up or
  removed, and Play checks again that the one there then still does the job rather than pinning
  its digest, since the mod manager that owns it (r2modman, Thunderstore Mod Manager) relinks it
  whenever it likes (the same holds for a `reuse-known-proxy` loader, one that was there before
  us with our very bytes). Gone: `needs-reinstall`, and installing again adds ours. Still there
  and no longer able: `shared-loader-changed` (installing again would answer `foreign-loader`,
  so Play does not send the person there). r2modman and Gale copy their profile's proxy and
  `doorstop_config.ini` over the ones WE added at every modded start, and ReShade saves the
  player's settings into the `ReShade.ini` we added: Play then records the proxy as shared when
  the one there does the job (`shared-loader-changed` when it does not; only for an integration
  that launches through Doorstop, so a changed ReShade `dxgi.dll` of ours is `needs-reinstall`),
  and a changed `doorstop_config.ini` or `ReShade.ini` we added or found with our very bytes as
  theirs (`keep-existing`, never rewritten), each with
  the digest there now, so uninstall leaves them and Play never asks for an install that would
  only write ours back. A Doorstop config that is theirs is kept out of the session by the
  options below; `ReShade.ini` has no say in those. Any other file we added that changed, or
  that is gone, is `needs-reinstall`. This is how those
  managers run every profile through one proxy, and it puts no new code into the game: Windows
  loads that DLL on every start of the game anyway. A foreign proxy that cannot do the job is
  refused (`foreign-loader`). Any other existing file is backed up into the profile
  and replaced. Uninstall removes or restores a file only while it still has our digest; a file
  the game updated since is the game's.
- **Someone else's BepInEx.** A BepInEx installed by hand after ours puts the very same
  `winhttp.dll` there, so a digest cannot tell whose it is. The ledger therefore keeps, for each
  Doorstop proxy we add, what sat beside it before Astra first added it (`loader_baseline`: the
  `doorstop_config.ini`'s digest and the `BepInEx/core` names, carried unchanged by every update).
  Uninstall leaves our proxy (and says so in `kept`) only when something not in that baseline and
  not ours sits beside it: another `doorstop_config.ini`, or a `BepInEx/core` name. A BepInEx the
  person had switched off before we came is in the baseline, so our proxy never stays behind for
  it; one they switch back on (its config changed) does run through ours, and keeps it. Under
  Proton the Wine override that loads a kept proxy stays too (`proton_overrides_kept`). An update
  removes our proxy and places the new one, so it stays our `add`; an install that fails takes
  back what it placed, unless another loader runs through it. A ledger from before 0.3.1 has no
  baseline: then every config and `BepInEx/core` entry that is not ours counts as theirs.
- **Their loader switched off.** A plan that would ADD our Doorstop beside a mod loader of the
  person's is refused. Another Doorstop in that folder under another name (BepInEx as
  `version.dll` or `winmm.dll`, judged by its bytes) is `foreign-loader`, naming it. A
  `doorstop_config.ini` we do not write that switches Doorstop on with a target that exists is
  `switched-off-loader`: a mod loader the person switched off by removing its proxy, whose mods
  ours would load again on every plain start of the game. The file is read as Doorstop reads it
  (`GetPrivateProfileString`: a missing `enabled` is `true`, a missing target `Doorstop.dll`); a
  relative target is looked up below the game's folder in any letter case, an absolute one
  through Proton's drive letters. A target that cannot be looked up counts as there: the check
  fails closed, with a refusal the person can lift by switching that file off. A target that is
  not there (r2modman's and Thunderstore Mod Manager's file names one in their own profile)
  loads nothing and is no reason to refuse. Windows and Proton only: a native Linux game loads no
  proxy. A proxy of ours already there (an update) changes nothing about how the game starts, and
  is not refused.
- **Doorstop's settings in the session.** A launch whose manifest passes Doorstop 4's
  `--doorstop-enabled` and `--doorstop-target-assembly` also passes
  `--doorstop-mono-debug-enabled false` and `--doorstop-mono-debug-suspend false`, so a
  `doorstop_config.ini` that is not ours cannot hold the game at a debugger. When the config where
  the game starts is not our own unchanged one, it also passes
  `--doorstop-boot-config-override <exe name>_Data/boot.config`: the game's own file, relative to
  the working folder exactly as Doorstop compares it, which it then redirects to itself. Not
  otherwise, since on 4.3+ naming an existing file switches Doorstop's file-open hooks on for the
  whole session. None of these is passed when the manifest sets it, and none is asked of a shared
  loader: each value is Doorstop's own default, and a Doorstop that does not know an option
  ignores it and reads no such setting from its file either (boot-config came to both in 4.3).
  `dll_search_path_override` is left to that file: it has no neutral value (the game's own
  `Managed` folder is not one: Doorstop would then load each assembly from disk instead of the
  bytes the game hands Mono). These are the launcher's, not the integration's: the plan's
  `launch_args` stay the manifest's. Every argument is one argv element, never a shell string: a
  path with spaces is one element and carries no quotes (Rust quotes it for `steam -applaunch` on
  Windows just as r2modman and Thunderstore Mod Manager do).
- **Steam's appinfo.** `appinfo.vdf` is someone else's binary file, often 100+ MB: it is read
  buffered, skipping to the one app by its size fields, with every length and the nesting depth
  bounded; anything corrupt answers `None`, never a panic. What it names is only a hint, which
  detection checks against the folder (relative, no `..`, no link on the way).
- **Anti-cheat.** A game folder carrying EasyAntiCheat, BattlEye, GameGuard, XIGNCODE or HoYoverse protection is
  refused, and so is a manifest whose `anti_cheat` is not `"none"`. A Steam app is also refused
  (`anti-cheat`, kind `valve-anti-cheat`) when Steam's `appinfo.vdf` lists `category_8` ("Valve
  Anti-Cheat enabled") in its `common/category`, at plan time and again at every launch. EVERY
  appinfo that answers is read (`GameTarget::steam_appinfo`, then every Steam install), and any
  one flagging VAC refuses. The game counts as checked only for a TRUSTED appid: the caller's, or
  the one the game folder proves through its library's `steamapps/appmanifest_*.acf` whose
  `installdir` is this folder (both present and different: `game-mismatch`; for a folder several
  apps share, such as GoldSrc's `common/Half-Life`, the caller's appid must be one of them). The integration
  manifest's `steam_appid` can only refuse (a mismatch, or that app is VAC), never clear. When no
  trusted appid is known or nothing answers, the plan is not refused on that ground alone, but
  `InstallPlan::vac_checked` is false, it warns `vac-unchecked`, and `install` refuses with
  `vac-unchecked` unless the caller passes `Accept { vac_unchecked: true }` (the CLI's
  `--accept-vac-unchecked`; `--yes` alone does not cover it). The ledger records that acceptance;
  a launch whose check is still unread is refused without it. A launch is also refused unless the
  game folder and program are the ones the ledger recorded.
- **Proton.** Every Wine DLL override in `[launch].proton_dll_overrides` (as many as the game
  needs, e.g. `{ dxgi = "n,b", d3d11 = "n,b" }`) goes into `<prefix>/user.reg`, in the game's own
  `AppDefaults\<exe>\DllOverrides` section only, in key order, in one atomic write with every
  other byte kept; what was there before is recorded per DLL and put back on uninstall. The
  ledger learns of the change before `user.reg` is written, and `user.reg` is read back after.
  Two spellings of one prefix (`~/.steam/steam/...`, `~/.local/share/Steam/...`) are one prefix
  (canonical path); records of another prefix or section are kept and restored too, and
  a value counts as the launcher's only through its own record of that prefix, section and DLL:
  a user's identical value is recorded and restored as theirs. Nothing is applied or restored
  while a wineserver serves the prefix (`game-running`, reason `wineserver`: a `wineserver`
  process in `/proc` whose working directory is `server-<dev>-<ino>` of the prefix, which also
  finds one with a private `/tmp`), since Wine writes its registry back over the file when it
  exits. It fails closed: an unreadable `/proc`, or a `wineserver` of this user whose working
  directory cannot be read, counts as running. A ledger with several records is written as schema 2, which
  an older launcher refuses with `schema-too-new`. A Steam
  launch carries no environment, so the registry is the mechanism; the plan also shows the same
  set as one `WINEDLLOVERRIDES` value (`d3d11=n,b;dxgi=n,b`, same order) for anyone who has to
  paste it. A DLL name is a bare name (letters, digits, `_ - .`, starting with a letter or digit,
  at most 64), never a path, and a name may appear once whatever its letter case; a value is one
  of `native`, `builtin`, `native,builtin`, `builtin,native`, their one-letter forms, or empty
  (disabled). Nothing is installed, launched
  or uninstalled while a process with the game's exe name runs (a best-effort check).
- **The ledger.** `<profile>/astra-launcher-ledger.json` records every file placed, its digest
  and what it replaced. It is written before the first file is placed, under a file lock; an
  install that fails or is cancelled is rolled back. Install re-plans under the lock and refuses
  with `plan-stale` when the game folder changed since the plan was shown. A ledger is written as
  the lowest schema that carries it (3 when a loader is shared) and its schema is read before its
  shape, so from this version on a ledger newer than the launcher is `schema-too-new` rather than
  a parse failure.
- **The play-arm file.** `launch` writes `<profile>/play-arm.json` atomically, under the profile
  lock, immediately before it asks Steam to start a game:
  `{"schema":1,"appid":<steam appid>,"started_unix_ms":<ms since the UNIX epoch>}`. Steam starts
  the game, so this is how the runner tells a start Astra asked for (and when) from any other.
  `started_unix_ms` is when Astra asked, not when the game's process started (Steam may update
  the game first). `prepare_launch` writes none, so a refusal between the two leaves no fresh arm;
  a failed write fails the launch before anything starts, and a failed spawn puts back what was
  there. A game without a Steam appid gets none. It is not a file of the ledger's, uninstall
  leaves it (removing the profile folder is the caller's; the one deletion is
  `remove_play_arm_if_older`, below), and like the ledger, its lock and the
  backups its name is reserved: no manifest file may be named `play-arm.json`. Known gap, shared
  by all four names: a Windows 8.3 short name (`PLAY-A~1.JSO`) is not recognised as one of them.
  `remove_play_arm_if_older(profile, decided_unix_ms)` (0.4.1) is the one way to take an arm
  away: under the profile lock, read without following a link or waiting on a FIFO, removed only
  when its `started_unix_ms` is older than `decided_unix_ms`; a link in its place (on Windows a
  directory link or junction too) is removed as a link. An arm it cannot parse stays (`false`);
  one it cannot read for any other reason is an error, never a silent `false`.
- **What Play records.** It is written to the ledger last, once nothing can refuse the Play any
  more: a refused Play records nothing. Uninstall keeps the Proton override that loads a shared
  proxy (`share-loader`) still beside the game, as it does for a proxy kept for another loader.
- **Nothing runs.** The launcher never elevates, never runs a program from a package, and passes
  launch arguments as an argv array, never through a shell.

## Building and testing

```
cargo build
cargo test        # every fixture is built by the tests; the first build fetches launcher
                  # 0.4.1 (main 73bf11e), the dev-dependency canary C1 compares against
cargo test --all-features   # also gi-probe and its tests (canary K4); what scripts/release.sh runs
```

## Verifying a release against what Astra compiles in

Astra does not bundle a binary of this crate: `astra-rs/astra-daemon/Cargo.toml` pins it as a
git dependency by commit (`rev = "<sha>"`), and Cargo builds it into the daemon from source at
that exact commit. So the comparable thing between "what Astra built" and "what GitHub built
from source" is not a binary hash — it's a hash of the **source tree**, which anyone can
recompute from a clone.

Every build computes `SOURCE_TREE_SHA256` (`build.rs`, see its doc comment for the exact
algorithm) and bakes it in:

```
astra-game-launcher --version
# astra-game-launcher 0.4.1 (source tree sha256 <hex>)
```

Astra's own release notes / About screen show the same hash for the commit it pinned. A GitHub
release of this repo (`scripts/release.sh`) publishes it too, alongside the commit SHA, so three
things can be compared: the hash Astra shipped with, the hash the tagged release published, and
a hash you compute yourself. Recompute it from a clone of a tag with one line (requires `git`
and `sha256sum`; `scripts/tree-hash.sh` is this same command, runnable against any directory):

```bash
V=0.4.1 REPO=https://github.com/mihailinl/astra-game-launcher
rm -rf /tmp/astra-gl-verify && mkdir -p /tmp/astra-gl-verify && \
git clone --quiet --depth 1 --branch "v$V" "$REPO" /tmp/astra-gl-verify/src && \
cd /tmp/astra-gl-verify/src && git archive "v$V" | tar -x -C /tmp/astra-gl-verify && \
cd /tmp/astra-gl-verify && \
find . -mindepth 1 \( -name .git -o -name .cargo-ok -o -name target \) -prune -o -type f -print \
  | sed 's|^\./||' | LC_ALL=C sort \
  | while IFS= read -r f; do sha256sum -- "$f"; done | sha256sum | cut -d' ' -f1
```

If that hex matches the `SOURCE_TREE_SHA256` the release published (and the one Astra's build
shows for the commit it pinned), the three agree: what you cloned, what the tag names, and what
got compiled in are the same bytes.

**A local edit and the build not noticing:** `build.rs` watches every top-level file and
directory that existed at the last build (so any edit inside `src/`, `tests/`, `docs/`,
`examples/`, … is always picked up). The one case it cannot see is a brand-new file or directory
dropped directly at the repo root — nothing was watching that spot before it existed. If
`--version` still shows the old hash after adding one, run `cargo clean -p astra-game-launcher`.

## License

[Mozilla Public License 2.0](LICENSE).
