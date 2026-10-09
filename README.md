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
   with `<Project>/Content/Paks/`. Godot: `<stem>.pck` beside the program.

Weaker signs answer at Low confidence: a pre-2017.2 Unity game (no `UnityPlayer`; ranked with
the others by depth), half an Unreal pairing, a Godot pck embedded in the program.
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
  `doorstop_config.ini` we did not write is left as it is. A foreign `winhttp.dll`,
  `version.dll`, `dxgi.dll` or `d3d11.dll` is another mod loader: the install is refused. Any
  other existing file is backed up into the profile and replaced. Uninstall removes or restores
  a file only while it still has our digest; a file the game updated since is the game's.
- **Steam's appinfo.** `appinfo.vdf` is someone else's binary file, often 100+ MB: it is read
  buffered, skipping to the one app by its size fields, with every length and the nesting depth
  bounded; anything corrupt answers `None`, never a panic. What it names is only a hint, which
  detection checks against the folder (relative, no `..`, no link on the way).
- **Anti-cheat.** A game folder carrying EasyAntiCheat, BattlEye, GameGuard, XIGNCODE or HoYoverse protection is
  refused, and so is a manifest whose `anti_cheat` is not `"none"`.
- **Proton.** The Wine DLL override goes into `<prefix>/user.reg`, in the game's own
  `AppDefaults\<exe>\DllOverrides` section only, written atomically with every other byte kept;
  what was there before is recorded and put back on uninstall. Nothing is installed, launched
  or uninstalled while a process with the game's exe name runs (a best-effort check).
- **The ledger.** `<profile>/astra-launcher-ledger.json` records every file placed, its digest
  and what it replaced. It is written before the first file is placed, under a file lock; an
  install that fails or is cancelled is rolled back. Install re-plans under the lock and refuses
  with `plan-stale` when the game folder changed since the plan was shown.
- **Nothing runs.** The launcher never elevates, never runs a program from a package, and passes
  launch arguments as an argv array, never through a shell.

## Building and testing

```
cargo build
cargo test        # no network; every fixture zip is built by the tests
```

## License

[Mozilla Public License 2.0](LICENSE).
