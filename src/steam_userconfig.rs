// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The launch options a Steam user set for a game themselves (the game's Properties → "Launch
//! options"), from `<steam>/userdata/<user>/config/localconfig.vdf`:
//! `UserLocalConfigStore/Software/Valve/Steam/apps/<appid>/LaunchOptions`.
//!
//! Steam adds them to every start it makes, Astra's `-applaunch` included, so options that carry
//! Doorstop options of their own (`--doorstop-…`, left there by a guide or another mod manager)
//! may win over Astra's: the launcher says so ([`crate::ProcessPlan::warnings`]) and never edits
//! the file. A text KeyValues file written by someone else: read bounded, any corruption answered
//! with nothing, never a panic.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// More than any real `localconfig.vdf` (a large account's is a few MB).
const MAX_CONFIG_BYTES: u64 = 64 * 1024 * 1024;
/// Nesting of KeyValues objects. Real files nest about 8 deep.
const MAX_DEPTH: usize = 64;
/// Steam users on one machine whose files are read.
const MAX_USERS: usize = 64;
/// Where an app's own options sit: `<this>/<appid>/LaunchOptions`.
const APPS_PATH: &[&str] = &["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"];

/// The launch options each Steam user of the install at `steam_root` set for `appids`, in one
/// pass per user's file: app → every non-empty one (one per user who set it). An app nobody set
/// options for is absent.
pub fn steam_user_launch_options(steam_root: &Path, appids: &[u32]) -> BTreeMap<u32, Vec<String>> {
    let wanted: BTreeSet<String> = appids.iter().map(u32::to_string).collect();
    let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let Ok(users) = std::fs::read_dir(steam_root.join("userdata")) else {
        return out;
    };
    let mut users: Vec<_> = users.flatten().map(|e| e.path()).collect();
    users.sort();
    for user in users.into_iter().take(MAX_USERS) {
        let file = user.join("config").join("localconfig.vdf");
        let Some(text) = read_bounded(&file) else {
            continue;
        };
        for (appid, options) in launch_options_in(&text, &wanted) {
            if let Ok(id) = appid.parse::<u32>()
                && !options.trim().is_empty()
            {
                out.entry(id).or_default().push(options);
            }
        }
    }
    out
}

/// Whether launch options pass Doorstop options of their own (`--doorstop-…`, any letter case).
pub fn launch_options_pass_doorstop(options: &str) -> bool {
    options.to_ascii_lowercase().contains("--doorstop-")
}

fn read_bounded(path: &Path) -> Option<String> {
    // A regular file before it is opened: opening a FIFO would wait for a writer forever.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let f = File::open(path).ok()?;
    let mut bytes = Vec::new();
    f.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_CONFIG_BYTES).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// `(appid, LaunchOptions)` for every wanted app in one `localconfig.vdf`'s text.
fn launch_options_in(text: &str, wanted: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut key: Option<String> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => return out,
                        Some('\\') => match chars.next() {
                            None => return out,
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                        },
                        Some('"') => break,
                        Some(other) => s.push(other),
                    }
                }
                match key.take() {
                    None => key = Some(s),
                    Some(k) => {
                        let at_app = stack.len() == APPS_PATH.len() + 1
                            && stack
                                .iter()
                                .zip(APPS_PATH)
                                .all(|(a, b)| a.eq_ignore_ascii_case(b))
                            && wanted.contains(&stack[APPS_PATH.len()]);
                        if at_app && k.eq_ignore_ascii_case("LaunchOptions") {
                            out.push((stack[APPS_PATH.len()].clone(), s));
                        }
                    }
                }
            }
            '{' => {
                if stack.len() >= MAX_DEPTH {
                    return out;
                }
                stack.push(key.take().unwrap_or_default());
            }
            '}' => {
                key = None;
                if stack.pop().is_none() {
                    return out;
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                // A comment, to the end of the line.
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_wanted_apps_own_options_are_read() {
        let text = r#""UserLocalConfigStore"
{
	"Software"
	{
		"Valve"
		{
			"Steam"
			{
				"Apps"
				{
					"4000001"
					{
						"LastPlayed"		"1761800000"
						"LaunchOptions"		"WINEDLLOVERRIDES=\"winhttp=n,b\" %command% --doorstop-enabled true"
					}
					"4000002"
					{
						"LaunchOptions"		"-novid"
					}
				}
				// a comment "LaunchOptions" "x"
			}
		}
	}
	"apps" { "4000001" { "LaunchOptions" "not under Software/Valve/Steam" } }
}
"#;
        let wanted: BTreeSet<String> = ["4000001".to_owned()].into();
        let got = launch_options_in(text, &wanted);
        assert_eq!(
            got,
            vec![(
                "4000001".to_owned(),
                "WINEDLLOVERRIDES=\"winhttp=n,b\" %command% --doorstop-enabled true".to_owned()
            )]
        );
        assert!(launch_options_pass_doorstop(&got[0].1));
        assert!(!launch_options_pass_doorstop("-novid --doorstopper"));
        // Cut anywhere: whatever was read whole, never a panic.
        for n in 0..text.len() {
            if text.is_char_boundary(n) {
                let _ = launch_options_in(&text[..n], &wanted);
            }
        }
    }
}
