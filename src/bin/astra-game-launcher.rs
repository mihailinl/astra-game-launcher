// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The command-line program. Every error is printed as `{code, params}` JSON on stdout and
//! exits 1; with `--json`, results are JSON too.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use astra_game_launcher::{
    self as gl, Ctx, Detection, GameTarget, GiPackage, InstallPlan, LauncherError, Platform, codes,
};
use serde_json::{Value, json};

const USAGE: &str =
    "astra-game-launcher: put a game-integration mod into a game and launch the game with it.

USAGE
  astra-game-launcher detect <game-dir> [--json]
  astra-game-launcher inspect <zip> --sha256 <hex> [--json]
  astra-game-launcher plan <zip> --sha256 <hex> --game <dir> --exe <rel> [--appid N]
                           [--proton-prefix P] [--platform windows|linux-proton|linux-native]
                           --profile <dir> [--json]
  astra-game-launcher install <same as plan> --yes [--json]
  astra-game-launcher launch --profile <dir> --game <dir> --exe <rel> [--appid N]
                             [--proton-prefix P] [--platform …] [--steam <path>]
                             [--var k=v]… [--vars-stdin] [--dry-run] [--json]
  astra-game-launcher uninstall --profile <dir> --game <dir> --exe <rel> [--proton-prefix P]
                                [--platform …] [--json]
  astra-game-launcher fetch <url> --sha256 <hex> -o <file> [--json]

NOTES
  On Linux a game whose exe ends in .exe runs under Proton; its prefix defaults to
  <library>/steamapps/compatdata/<appid>/pfx.
  `launch --dry-run` prepares everything (INI keys, the Proton override, which uninstall
  reverts) and prints the command without starting it.
  `--vars-stdin` reads `k=v` lines or a JSON object from stdin, so a token stays out of `ps`.
  Errors print {\"code\", \"params\"} as JSON on stdout and exit 1.
";

struct Args {
    positional: Vec<String>,
    opts: BTreeMap<String, Vec<String>>,
    flags: BTreeSet<String>,
}

fn usage(detail: impl Into<String>) -> LauncherError {
    LauncherError::new(codes::USAGE).with("detail", detail)
}

impl Args {
    fn parse(argv: &[String], with_value: &[&str], flags: &[&str]) -> Result<Args, LauncherError> {
        let mut a = Args {
            positional: Vec::new(),
            opts: BTreeMap::new(),
            flags: BTreeSet::new(),
        };
        let mut it = argv.iter();
        while let Some(arg) = it.next() {
            if arg.starts_with('-') && arg.len() > 1 {
                let (name, inline) = match arg.split_once('=') {
                    Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                    None => (arg.clone(), None),
                };
                if with_value.contains(&name.as_str()) {
                    let value = match inline {
                        Some(v) => v,
                        None => it
                            .next()
                            .cloned()
                            .ok_or_else(|| usage(format!("{name} needs a value")))?,
                    };
                    a.opts.entry(name).or_default().push(value);
                } else if flags.contains(&name.as_str()) && inline.is_none() {
                    a.flags.insert(name);
                } else {
                    return Err(usage(format!("unknown option {name}")));
                }
            } else {
                a.positional.push(arg.clone());
            }
        }
        Ok(a)
    }

    fn one(&self, name: &str) -> Option<&str> {
        self.opts
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    fn need(&self, name: &str) -> Result<&str, LauncherError> {
        self.one(name)
            .ok_or_else(|| usage(format!("{name} is required")))
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn positional(&self, n: usize, what: &str) -> Result<&str, LauncherError> {
        self.positional
            .get(n)
            .map(String::as_str)
            .ok_or_else(|| usage(format!("{what} is required")))
    }
}

/// What a command returns: JSON for `--json`, text otherwise.
struct Output {
    json: Value,
    text: String,
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

const GAME_OPTS: &[&str] = &[
    "--game",
    "--exe",
    "--appid",
    "--proton-prefix",
    "--platform",
];

fn game_target(a: &Args) -> Result<GameTarget, LauncherError> {
    let dir = PathBuf::from(a.need("--game")?);
    let exe = PathBuf::from(a.need("--exe")?);
    let steam_appid = match a.one("--appid") {
        Some(s) => Some(
            s.parse::<u32>()
                .map_err(|_| usage("--appid must be a number"))?,
        ),
        None => None,
    };
    let proton_prefix = || {
        a.one("--proton-prefix")
            .map(PathBuf::from)
            .or_else(|| steam_appid.and_then(|id| gl::proton_prefix_for(&dir, id)))
    };
    let platform = match a.one("--platform") {
        Some("windows") => Platform::Windows,
        Some("linux-proton") => Platform::LinuxProton {
            prefix: proton_prefix(),
        },
        Some("linux-native") => Platform::LinuxNative,
        Some(other) => return Err(usage(format!("unknown platform {other}"))),
        None if cfg!(windows) => Platform::Windows,
        None if exe
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe")) =>
        {
            Platform::LinuxProton {
                prefix: proton_prefix(),
            }
        }
        None => Platform::LinuxNative,
    };
    Ok(GameTarget {
        dir,
        exe,
        steam_appid,
        platform,
    })
}

fn open_package(a: &Args) -> Result<GiPackage, LauncherError> {
    let path = PathBuf::from(a.positional(1, "<zip>")?);
    let sha = a.need("--sha256")?;
    let io = |e: std::io::Error| LauncherError::io(e).with("path", path.display().to_string());
    let len = std::fs::metadata(&path).map_err(io)?.len();
    let limit = gl::Limits::default().max_zip_bytes;
    if len > limit {
        return Err(LauncherError::new(codes::TOO_LARGE)
            .with("bytes", len.to_string())
            .with("limit", limit.to_string()));
    }
    let bytes = std::fs::read(&path).map_err(io)?;
    GiPackage::open(&bytes, sha)
}

fn code_of<T: serde::Serialize>(v: &T) -> String {
    json!(v).as_str().unwrap_or("").to_owned()
}

fn detection_text(d: &Detection) -> String {
    let exe = d
        .exe
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".into());
    let ac = if d.anti_cheat.is_empty() {
        "none".into()
    } else {
        d.anti_cheat.join(", ")
    };
    let mut s = String::new();
    let _ = writeln!(
        s,
        "engine:     {} ({})",
        d.engine.code(),
        code_of(&d.confidence)
    );
    let _ = writeln!(s, "binary:     {}", code_of(&d.binary));
    let _ = writeln!(s, "exe:        {exe}");
    let _ = writeln!(s, "anti-cheat: {ac}");
    s
}

fn plan_text(p: &InstallPlan) -> String {
    let mut s = String::new();
    let i = &p.integration;
    let license = if i.license.is_empty() {
        "-"
    } else {
        &i.license
    };
    let _ = writeln!(s, "{} {} ({})", i.name, i.version, i.id);
    let _ = writeln!(
        s,
        "  authors: {}   license: {license}",
        i.authors.join(", ")
    );
    if let Some(r) = &i.repo {
        let _ = writeln!(s, "  repo: {r}");
    }
    let _ = writeln!(s, "  zip sha256: {}", p.source_sha256);
    if let Some(r) = &p.replaces {
        let _ = writeln!(s, "  replaces: {} {}", r.name, r.version);
    }
    let _ = writeln!(
        s,
        "profile: {}  ({} files, {} bytes)",
        p.profile_dir.display(),
        p.profile_files,
        p.profile_bytes
    );
    let _ = writeln!(s, "beside the game's exe, in {}:", p.game_dir.display());
    for g in &p.game_files {
        let _ = writeln!(
            s,
            "  {:<18} {}  sha256 {}",
            g.action.code(),
            g.rel_path,
            g.sha256
        );
    }
    let _ = writeln!(
        s,
        "launch args: {}",
        serde_json::to_string(&p.launch_args).unwrap_or_default()
    );
    if let Some((dll, v)) = &p.proton_override {
        let _ = writeln!(s, "proton dll override: {dll}={v}");
    }
    let _ = writeln!(s, "warnings: {}", p.warnings.join(", "));
    s
}

fn read_vars(a: &Args) -> Result<BTreeMap<String, String>, LauncherError> {
    let mut vars = BTreeMap::new();
    if a.flag("--vars-stdin") {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(LauncherError::io)?;
        if input.trim_start().starts_with('{') {
            let obj: BTreeMap<String, String> =
                serde_json::from_str(&input).map_err(|e| usage(format!("--vars-stdin: {e}")))?;
            vars.extend(obj);
        } else {
            for line in input.lines() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if line.trim().is_empty() {
                    continue;
                }
                let (k, v) = line
                    .split_once('=')
                    .ok_or_else(|| usage("--vars-stdin: k=v lines"))?;
                vars.insert(k.to_owned(), v.to_owned());
            }
        }
    }
    for kv in a.opts.get("--var").into_iter().flatten() {
        let (k, v) = kv.split_once('=').ok_or_else(|| usage("--var takes k=v"))?;
        vars.insert(k.to_owned(), v.to_owned());
    }
    Ok(vars)
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), LauncherError> {
    let name = path
        .file_name()
        .ok_or_else(|| usage("-o names no file"))?
        .to_string_lossy()
        .into_owned();
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".{name}.part-{}", std::process::id()));
    let res = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(LauncherError::io(e).with("path", path.display().to_string()));
    }
    Ok(())
}

fn run(argv: &[String]) -> Result<Output, LauncherError> {
    let cmd = argv.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "detect" => {
            let a = Args::parse(argv, &[], &["--json"])?;
            let d = gl::detect(Path::new(a.positional(1, "<game-dir>")?))?;
            Ok(Output {
                text: detection_text(&d),
                json: to_json(&d),
            })
        }
        "inspect" => {
            let a = Args::parse(argv, &["--sha256"], &["--json"])?;
            let s = open_package(&a)?.summary();
            let text = format!("{}\n", serde_json::to_string_pretty(&s).unwrap_or_default());
            Ok(Output {
                json: to_json(&s),
                text,
            })
        }
        "plan" | "install" => {
            let mut opts = vec!["--sha256", "--profile"];
            opts.extend_from_slice(GAME_OPTS);
            let a = Args::parse(argv, &opts, &["--json", "--yes"])?;
            let pkg = open_package(&a)?;
            let game = game_target(&a)?;
            let plan = gl::plan_install(&pkg, &game, Path::new(a.need("--profile")?))?;
            if cmd == "plan" {
                return Ok(Output {
                    text: plan_text(&plan),
                    json: to_json(&plan),
                });
            }
            if !a.flag("--yes") {
                if !a.flag("--json") {
                    eprint!("{}", plan_text(&plan));
                }
                return Err(LauncherError::new(codes::CONSENT_REQUIRED));
            }
            let quiet = a.flag("--json");
            let progress = move |p: gl::Progress| {
                if !quiet {
                    eprintln!("{} {}/{}", p.stage, p.done, p.total);
                }
            };
            let never = || false;
            let ctx = Ctx {
                cancel: &never,
                progress: &progress,
            };
            let ledger = gl::install(&pkg, &plan, &game, &ctx)?;
            let text = format!(
                "{}installed: {} profile files, {} game files\n",
                plan_text(&plan),
                ledger.files.len(),
                ledger.game_files.len()
            );
            Ok(Output {
                json: json!({ "plan": plan, "ledger": ledger }),
                text,
            })
        }
        "launch" => {
            let mut opts = vec!["--profile", "--steam", "--var"];
            opts.extend_from_slice(GAME_OPTS);
            let a = Args::parse(argv, &opts, &["--json", "--vars-stdin", "--dry-run"])?;
            let game = game_target(&a)?;
            let vars = read_vars(&a)?;
            let steam = a.one("--steam").map(PathBuf::from).or_else(gl::find_steam);
            let plan = gl::prepare_launch(
                Path::new(a.need("--profile")?),
                &game,
                &vars,
                steam.as_deref(),
            )?;
            let dry = a.flag("--dry-run");
            if !dry {
                gl::launch(&plan, &gl::StdSpawn)?;
            }
            let text = format!(
                "{} {}\n{}\n",
                plan.program.display(),
                serde_json::to_string(&plan.args).unwrap_or_default(),
                if dry {
                    "(dry run: not started)"
                } else {
                    "started"
                }
            );
            Ok(Output {
                json: json!({ "process": plan, "started": !dry }),
                text,
            })
        }
        "uninstall" => {
            let mut opts = vec!["--profile"];
            opts.extend_from_slice(GAME_OPTS);
            let a = Args::parse(argv, &opts, &["--json"])?;
            let game = game_target(&a)?;
            let r = gl::uninstall(Path::new(a.need("--profile")?), &game)?;
            let text = format!(
                "removed: {:?}\nrestored: {:?}\nkept (changed since): {:?}\nprofile files removed: {}\nproton override restored: {}\n",
                r.removed, r.restored, r.kept, r.profile_files_removed, r.proton_restored
            );
            Ok(Output {
                json: to_json(&r),
                text,
            })
        }
        "fetch" => {
            let a = Args::parse(argv, &["--sha256", "-o"], &["--json"])?;
            let url = a.positional(1, "<url>")?;
            let out = PathBuf::from(a.need("-o")?);
            let fetcher = gl::UreqFetch::default();
            let bytes =
                gl::fetch_verified(&fetcher, url, a.need("--sha256")?, gl::MAX_DOWNLOAD_BYTES)?;
            write_file_atomic(&out, &bytes)?;
            let text = format!(
                "{} ({} bytes, sha256 verified)\n",
                out.display(),
                bytes.len()
            );
            Ok(Output {
                json: json!({ "path": out, "bytes": bytes.len() }),
                text,
            })
        }
        "help" | "--help" | "-h" => Ok(Output {
            json: json!({ "usage": USAGE }),
            text: USAGE.to_owned(),
        }),
        other => Err(usage(format!("unknown command {other}"))),
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let json_out = argv.iter().any(|a| a == "--json");
    match run(&argv) {
        Ok(out) => {
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out.json).unwrap_or_default()
                );
            } else {
                print!("{}", out.text);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}", serde_json::to_string(&e).unwrap_or_default());
            if !json_out {
                eprintln!("error: {e}");
            }
            ExitCode::from(1)
        }
    }
}
