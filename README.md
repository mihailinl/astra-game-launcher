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
  and no longer able: `foreign-loader`, as installing again would answer. This is how those
  managers run every profile through one proxy, and it puts no new code into the game: Windows
  loads that DLL on every start of the game anyway. A foreign proxy that cannot do the job is
  refused (`foreign-loader`). Any other existing file is backed up into the profile
  and replaced. Uninstall removes or restores a file only while it still has our digest; a file
  the game updated since is the game's.
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
- **Nothing runs.** The launcher never elevates, never runs a program from a package, and passes
  launch arguments as an argv array, never through a shell.

## Building and testing

```
cargo build
cargo test        # no network; every fixture zip is built by the tests
```

## License

[Mozilla Public License 2.0](LICENSE).
