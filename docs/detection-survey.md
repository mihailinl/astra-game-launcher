# Detection survey

The rules of [`detect_with`](../src/detect.rs) on real Steam libraries: one Linux machine, three
libraries, 2026-10-08, crate 0.2.0. Every game was read, nothing was written. Regenerate the
second table with

```
cargo run --example detection_survey
```

Game folder names only; every path is relative to the game's folder.

## What Steam launches

`launch_executable(appinfo, appid, os)` against the machine's own `appcache/appinfo.vdf` (v29).
A `-` is an app with no launch entry for that OS.

| Game | App ID | windows | linux |
|---|---|---|---|
| MiSide | 2527500 | `MiSideFull.exe` | `MiSideFull.exe` |
| Lethal Company | 1966720 | `Lethal Company.exe` | `Lethal Company.exe` |
| PEAK | 3527290 | `PEAK.exe` | `PEAK.exe` |
| Content Warning | 2881650 | `Content Warning.exe` | - |
| R.E.P.O. | 3241660 | `REPO.exe` | `REPO.exe` |
| EXTERMINATION SHIP Demo | 3619560 | `Extermination ship.exe` | - |
| S.T.A.L.K.E.R. 2: Heart of Chornobyl | 1643320 | `Stalker2.exe` | - |
| Cyberpunk 2077 | 1091500 | `redprelauncher.exe` | `bin/x64/Cyberpunk2077.exe` |

- MiSide, Lethal Company and R.E.P.O. carry one entry with no OS list, so it answers for both.
- PEAK has no `default` entry for Windows, only the launch options `option1`–`option3` (DX12,
  DX11, Vulkan), all `PEAK.exe`; its only `default` is a Linux entry on a beta branch. Both
  answers come from the documented fallbacks.
- Cyberpunk 2077's Windows default is the CD Projekt prelauncher. Its Linux answer is the
  beta-branch entry with no OS list (Cyberpunk ships no Linux build), the last fallback.

## Detection, with Steam's program as the hint

The hint is what Steam launches for the OS the game runs as: `windows` when the game has a
Proton prefix, else `linux`. When that OS's program is not on disk, the other OS's answer is
used. Rows without an App ID have no app manifest (leftovers, or tools Steam does not list).
Some games appear twice because two libraries hold a folder of that name.

| Game folder | App ID | Steam launches | Engine | Confidence | Exe | Evidence | Hint used | Anti-cheat |
|---|---|---|---|---|---|---|---|---|
| Astra Your Virtual Assistant | 3853440 | `astra-daemon` | unknown | low | - | - | no | - |
| Half-Life 2 Deathmatch | - | - | unknown | low | - | - | no | - |
| Proton - Experimental | 1493710 | - | unknown | low | - | - | no | - |
| Steam Controller Configs | - | - | unknown | low | - | - | no | - |
| SteamLinuxRuntime | 1070560 | - | unknown | low | - | - | no | - |
| SteamLinuxRuntime_4 | 4183110 | - | unknown | low | - | - | no | - |
| SteamLinuxRuntime_sniper | 1628350 | - | unknown | low | - | - | no | - |
| Steamworks Shared | 228980 | - | unknown | low | - | - | no | - |
| Counter-Strike Global Offensive | - | - | unknown | low | - | - | no | - |
| Geometry Dash | 322170 | `GeometryDash.exe` | unknown | low | - | - | no | - |
| Half-Life 2 | - | - | unknown | low | - | - | no | - |
| Portal Prelude RTX | - | - | unknown | low | - | - | no | - |
| Red Dead Redemption 2 | 1174180 | `PlayRDR2.exe` | unknown | low | - | - | no | - |
| dota 2 beta | - | - | unknown | low | - | - | no | - |
| Content Warning | 2881650 | `Content Warning.exe` | unity-mono | high | `Content Warning.exe` | `Content Warning_Data` | yes | - |
| Counter-Strike Global Offensive | 730 | `game/cs2.sh` | unknown | low | - | - | no | - |
| Counter-Strike Source | 240 | `cstrike.sh` | unknown | low | - | - | no | - |
| Cyberpunk 2077 | 1091500 | `redprelauncher.exe` | unknown | low | `REDprelauncher.exe` | - | no | - |
| DEATH STRANDING 2 - ON THE BEACH | 3280350 | `DS2.exe` | unknown | low | - | - | no | - |
| DEATH STRANDING DIRECTORS CUT | 1850570 | `ds.exe` | unknown | low | `ds.exe` | - | no | - |
| EXTERMINATION SHIP Demo | 3619560 | `Extermination ship.exe` | unity-mono | high | `Extermination ship.exe` | `Extermination ship_Data` | yes | - |
| Half-Life | 70 | `hl.sh` | unknown | low | - | - | no | - |
| Half-Life 2 Deathmatch | 320 | `hl2mp.sh` | unknown | low | - | - | no | - |
| Half-Life 2 RTX | - | - | unknown | low | - | - | no | - |
| Lethal Company | 1966720 | `Lethal Company.exe` | unity-mono | high | `Lethal Company.exe` | `Lethal Company_Data` | yes | - |
| MiSide | 2527500 | `MiSideFull.exe` | unity-il2cpp | high | `MiSideFull.exe` | `MiSideFull_Data` | yes | - |
| PEAK | 3527290 | `PEAK.exe` | unity-mono | high | `PEAK.exe` | `PEAK_Data` | yes | - |
| Portal Prelude RTX | - | - | unknown | low | - | - | no | - |
| REPO | 3241660 | `REPO.exe` | unity-mono | high | `REPO.exe` | `REPO_Data` | yes | - |
| S.T.A.L.K.E.R. 2 Heart of Chornobyl | 1643320 | `Stalker2.exe` | unreal | high | `Stalker2/Binaries/Win64/Stalker2-Win64-Shipping.exe` | `Stalker2` | no | - |
| Scarlet Skips | 4513480 | `ScarletSkips.exe` | unreal | high | `ScarletSkips/Binaries/Win64/ScarletSkips-Win64-Shipping.exe` | `ScarletSkips` | no | - |
| SteamLinuxRuntime_soldier | 1391110 | - | unknown | low | - | - | no | - |
| Zenless Zone Zero | 4162040 | `HYP.exe` | unity-il2cpp | high | `games/ZenlessZoneZero Game/ZenlessZoneZero.exe` | `games/ZenlessZoneZero Game/ZenlessZoneZero_Data` | no | hoyo-protect |

### Reading it

- **MiSide** is `unity-il2cpp` with `MiSideFull.exe` and evidence `MiSideFull_Data`, by the
  general rule: `GameAssembly.dll` sits beside `UnityPlayer.dll` and the program. Its
  `Voice Editor/` is a Mono program one level down and loses on depth. No rule names MiSide.
- **The Unity Mono games** (Lethal Company, PEAK, Content Warning, R.E.P.O., EXTERMINATION SHIP
  Demo) are all `high`, and the program Steam launches passes the rule, so `hint_used` is yes.
- **S.T.A.L.K.E.R. 2 and Scarlet Skips** are Unreal. Steam launches the bootstrap at the top
  (`Stalker2.exe`), which does not pass rule 5, so it is ignored. The shipping program that
  pairs `Binaries/Win64` with `Content/Paks` in one project folder is the answer.
- **Zenless Zone Zero.** Steam launches the HoYoPlay launcher (`HYP.exe`), which is ignored. The
  game is found three levels down by rule 2, and its HoYoverse protection is reported.
- **`unknown`** is the honest answer for engines the rules do not cover: Source and GoldSrc
  (Counter-Strike, Half-Life), REDengine (Cyberpunk 2077), Decima (both DEATH STRANDING games),
  RAGE (Red Dead Redemption 2), Cocos2d-x (Geometry Dash), and Steam's own runtimes and tools.
  A hint never makes a game of an unknown engine: it must pass a rule to count.
