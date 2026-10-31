// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `gi-probe`: what a game integration's release, or an engine descriptor, is checked against
//! before anybody installs it (merged plan R28, R30). One JSON report on stdout:
//!
//! ```json
//! {"schema":"astra.gi-probe/1","tool":{"version":"…","source_tree_sha256":"…"},
//!  "subcommand":"compat","verdict":"pass", …, "findings":[{"code":"…","step":2,"field":"…","detail":"…"}]}
//! ```
//!
//! Exit 0: no findings. Exit 1: findings (the report lists them). Exit 2: the command line was
//! wrong or a file could not be read (the report's `error` says which); nothing was concluded.
//!
//! - `compat`: a release against Astra 0.2.7 and launcher 0.3.1, plan B §2's six C2 steps plus
//!   0.2.7's own tag, asset and manifest rules ([`compat`]). Canary K4.
//! - `validate-descriptor`: an `astra.engine/1` descriptor, the spec's §5 lint ([`descriptor`]).
//! - `pick`: Astra 0.2.7's choice of release out of a GitHub releases listing, for
//!   `tools/fetch-first-party-manifests.sh` (the replica, [`v027::releases::pick_with`]).

mod compat;
mod descriptor;
mod trees;
mod v027;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::{Value, json};

/// The report's `schema`.
pub const REPORT_SCHEMA: &str = "astra.gi-probe/1";

const USAGE: &str = "gi-probe: checks a game integration's release, or an engine descriptor.

USAGE
  gi-probe compat --zip <astra-gi.zip> --source <integration id> --tag <tag>
                  [--asset <name>] [--prerelease] [--sha256 <hex>]
                  [--previous <previous release's zip>] [--notes <release notes file>]
  gi-probe validate-descriptor <file> [--first-party]
  gi-probe pick --source <integration id> --listing <releases.json> [--prerelease]
  gi-probe --version

  compat        the release against Astra 0.2.7 and launcher 0.3.1 (what 0.2.7 ships), in six
                steps: 0.3.1 opens and parses it; 0.2.7's manifest, tag and asset rules for the
                source; its [integration.*] tables change nothing 0.3.1 plans; 0.3.1 installs it
                and prepares a launch with exactly {bridge_port, bridge_token}; the files under
                every `**` are the previous release's or named in the notes; the consent sheet's
                inputs are the previous release's (a change is a notice, not a finding).
                --asset defaults to the zip's file name. --prerelease: the release is a
                GitHub prerelease (its tag may then be a semver pre-release).
  validate-descriptor
                an astra.engine/1 descriptor (TOML, JSON, or a manifest's [integration.engine]):
                it parses, Pass B admits it, and it claims none of the spec's negative folders.
  pick          the release a 0.2.7 release build installs for the source out of a listing
                (GitHub's GET /repositories/<id>/releases, every page as one array), and the
                source's compiled fallback pin.

Exit 0: no findings. 1: findings. 2: a wrong command line or an unreadable file.
";

/// One thing wrong, as the report lists it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub detail: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

impl Finding {
    pub fn new(code: &str, detail: impl Into<String>) -> Finding {
        Finding {
            code: code.to_owned(),
            step: None,
            field: None,
            detail: detail.into(),
            params: BTreeMap::new(),
        }
    }

    pub fn step(mut self, step: u8) -> Finding {
        self.step = Some(step);
        self
    }

    pub fn field(mut self, field: impl Into<String>) -> Finding {
        self.field = Some(field.into());
        self
    }

    pub fn with(mut self, key: &str, value: impl Into<String>) -> Finding {
        self.params.insert(key.to_owned(), value.into());
        self
    }

    /// A launcher refusal (either launcher's: both are `{code, params}`), its `field` or `key`
    /// param as the finding's field.
    pub fn from_refusal(code: &str, params: &BTreeMap<String, String>, detail: &str) -> Finding {
        Finding {
            code: code.to_owned(),
            step: None,
            field: params.get("field").or_else(|| params.get("key")).cloned(),
            detail: detail.to_owned(),
            params: params.clone(),
        }
    }
}

/// Nothing could be concluded: exit 2.
#[derive(Debug)]
pub struct Fatal {
    pub code: &'static str,
    pub detail: String,
}

impl Fatal {
    pub fn usage(detail: impl Into<String>) -> Fatal {
        Fatal {
            code: "usage",
            detail: detail.into(),
        }
    }

    pub fn io(path: &Path, e: impl std::fmt::Display) -> Fatal {
        Fatal {
            code: "io",
            detail: format!("{}: {e}", path.display()),
        }
    }
}

/// `{version, source_tree_sha256}` of this build.
pub fn tool() -> Value {
    json!({
        "version": astra_game_launcher::VERSION,
        "source_tree_sha256": astra_game_launcher::SOURCE_TREE_SHA256,
    })
}

/// The report's head, with `rest` merged after it and `verdict` set from `findings`.
pub fn report(subcommand: &str, rest: Value, findings: &[Finding]) -> Value {
    let mut out = json!({
        "schema": REPORT_SCHEMA,
        "tool": tool(),
        "subcommand": subcommand,
        "verdict": if findings.is_empty() { "pass" } else { "fail" },
    });
    if let (Value::Object(o), Value::Object(r)) = (&mut out, rest) {
        o.extend(r);
    }
    out["findings"] = serde_json::to_value(findings).expect("findings serialise");
    out
}

/// The report's keys in reading order: the head first, `findings` last (a `serde_json` map is
/// sorted, and this crate does not switch `preserve_order` on for the library's sake).
const KEY_ORDER: &[&str] = &[
    "schema",
    "tool",
    "subcommand",
    "verdict",
    "error",
    "input",
    "source",
    "against",
    "spec",
    "integration",
    "descriptor",
    "negative_folders",
    "release",
    "fallback",
    "steps",
    "notices",
];

/// A report serialised with its top-level keys in [`KEY_ORDER`].
struct Ordered<'a>(&'a Value);

impl Serialize for Ordered<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let Value::Object(o) = self.0 else {
            return self.0.serialize(s);
        };
        let rank = |k: &str| match k {
            "findings" => usize::MAX,
            k => KEY_ORDER
                .iter()
                .position(|o| *o == k)
                .unwrap_or(KEY_ORDER.len()),
        };
        let mut keys: Vec<&String> = o.keys().collect();
        keys.sort_by_key(|k| (rank(k), *k));
        let mut m = s.serialize_map(Some(keys.len()))?;
        for k in keys {
            m.serialize_entry(k, &o[k])?;
        }
        m.end()
    }
}

/// A folder below the temporary directory (`TMPDIR`), removed when dropped.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(what: &str) -> Result<Scratch, Fatal> {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "gi-probe-{}-{}-{what}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).map_err(|e| Fatal::io(&p, e))?;
        let p = std::fs::canonicalize(&p).map_err(|e| Fatal::io(&p, e))?;
        Ok(Scratch(p))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `--name value` options, `--flag`s and positional arguments.
pub struct Args {
    pub positional: Vec<String>,
    opts: BTreeMap<String, String>,
    flags: BTreeSet<String>,
}

impl Args {
    fn parse(argv: &[String], with_value: &[&str], flags: &[&str]) -> Result<Args, Fatal> {
        let mut a = Args {
            positional: Vec::new(),
            opts: BTreeMap::new(),
            flags: BTreeSet::new(),
        };
        let mut it = argv.iter();
        while let Some(arg) = it.next() {
            if let Some(name) = arg.strip_prefix("--") {
                if with_value.contains(&name) {
                    let v = it
                        .next()
                        .ok_or_else(|| Fatal::usage(format!("--{name} needs a value")))?;
                    if a.opts.insert(name.to_owned(), v.clone()).is_some() {
                        return Err(Fatal::usage(format!("--{name} given twice")));
                    }
                } else if flags.contains(&name) {
                    a.flags.insert(name.to_owned());
                } else {
                    return Err(Fatal::usage(format!("unknown option --{name}")));
                }
            } else {
                a.positional.push(arg.clone());
            }
        }
        Ok(a)
    }

    pub fn opt(&self, name: &str) -> Option<&str> {
        self.opts.get(name).map(String::as_str)
    }

    pub fn need(&self, name: &str) -> Result<&str, Fatal> {
        self.opt(name)
            .ok_or_else(|| Fatal::usage(format!("--{name} is required")))
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn no_positional(&self) -> Result<(), Fatal> {
        match self.positional.first() {
            Some(p) => Err(Fatal::usage(format!("unexpected argument {p:?}"))),
            None => Ok(()),
        }
    }
}

fn run(argv: &[String]) -> Result<(Value, bool), Fatal> {
    let Some((sub, rest)) = argv.split_first() else {
        return Err(Fatal::usage("no subcommand"));
    };
    match sub.as_str() {
        "compat" => {
            let a = Args::parse(
                rest,
                &[
                    "zip", "source", "tag", "asset", "sha256", "previous", "notes",
                ],
                &["prerelease"],
            )?;
            a.no_positional()?;
            let args = compat::CompatArgs {
                zip: PathBuf::from(a.need("zip")?),
                source: a.need("source")?.to_owned(),
                tag: a.need("tag")?.to_owned(),
                asset: a.opt("asset").map(str::to_owned),
                sha256: a.opt("sha256").map(str::to_owned),
                prerelease: a.flag("prerelease"),
                previous: a.opt("previous").map(PathBuf::from),
                notes: a.opt("notes").map(PathBuf::from),
            };
            compat::run(&args)
        }
        "validate-descriptor" => {
            let a = Args::parse(rest, &[], &["first-party"])?;
            let [file] = a.positional.as_slice() else {
                return Err(Fatal::usage("validate-descriptor takes one file"));
            };
            descriptor::run(Path::new(file), a.flag("first-party"))
        }
        "pick" => {
            let a = Args::parse(rest, &["source", "listing"], &["prerelease"])?;
            a.no_positional()?;
            compat::pick(
                a.need("source")?,
                Path::new(a.need("listing")?),
                a.flag("prerelease"),
            )
        }
        other => Err(Fatal::usage(format!("unknown subcommand {other:?}"))),
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("--version") => {
            println!(
                "gi-probe {} (source tree sha256 {})",
                astra_game_launcher::VERSION,
                astra_game_launcher::SOURCE_TREE_SHA256
            );
            return ExitCode::SUCCESS;
        }
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let (out, code) = match run(&argv) {
        Ok((report, passed)) => (report, if passed { 0 } else { 1 }),
        Err(f) => {
            eprintln!("gi-probe: {}: {}", f.code, f.detail);
            if f.code == "usage" {
                eprint!("\n{USAGE}");
            }
            let sub = argv.first().cloned().unwrap_or_default();
            (
                json!({
                    "schema": REPORT_SCHEMA,
                    "tool": tool(),
                    "subcommand": sub,
                    "error": { "code": f.code, "detail": f.detail },
                }),
                2,
            )
        }
    };
    let mut stdout = std::io::stdout().lock();
    let text = serde_json::to_string_pretty(&Ordered(&out)).expect("a report serialises");
    if writeln!(stdout, "{text}").is_err() {
        return ExitCode::from(2);
    }
    ExitCode::from(code)
}
