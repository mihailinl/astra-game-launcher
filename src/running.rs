// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Best effort: is a process with the game's program name running? Under Proton the game's
//! process carries its Windows path (`Z:\…\Game.exe`) as its first argument.

use crate::error::{LauncherError, Result, codes};

fn base_name(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// Whether a process's program, as [`running_pids`] reads it (a path or a bare name), is the
/// program `exe_file` (a FILE NAME): its last component, compared without case. The ONE matcher
/// of this module.
fn is_program(program: &str, exe_file: &str) -> bool {
    base_name(program).eq_ignore_ascii_case(exe_file)
}

/// Is a process whose program is `exe_file` (a FILE NAME, compared without case) running? Best
/// effort: on Linux the first argument of each process, which under Proton is the game's Windows
/// path; on Windows each process's image name; elsewhere always `false`. This process never counts.
/// [`running_pids`]'s answer, non-empty.
pub fn is_running(exe_file: &str) -> bool {
    !running_pids(exe_file).is_empty()
}

/// The ids of the processes [`is_running`] counts for `exe_file`, the same matcher, in no
/// particular order: on Linux every process whose first argument names the program (under Proton
/// and Wine the game's process carries its Windows path there, `Z:\…\Game.exe`, so the Wine
/// process that runs the game is found); on Windows every process whose image name is it;
/// elsewhere none. This process is never one. What Astra's daemon holds a Link peer's pid
/// against (`link-v1.md` §3, "Peer process").
#[cfg(target_os = "linux")]
pub fn running_pids(exe_file: &str) -> Vec<u32> {
    let me = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| {
            n.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| n.parse::<u32>().ok())
                .flatten()
        }) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or(&[]);
        if is_program(&String::from_utf8_lossy(argv0), exe_file) {
            out.push(pid);
        }
    }
    out
}

#[cfg(windows)]
pub fn running_pids(exe_file: &str) -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let me = std::process::id();
    // SAFETY: plain Win32 calls on a snapshot handle we own and close; the entry struct is
    // initialised with its size as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = Vec::new();
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            if entry.th32ProcessID != me {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if is_program(&name, exe_file) {
                    found.push(entry.th32ProcessID);
                }
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn running_pids(_exe_file: &str) -> Vec<u32> {
    Vec::new()
}

/// Refuses with `game-running` when the game's program is running.
pub(crate) fn refuse_if_running(exe_file: &str) -> Result<()> {
    if is_running(exe_file) {
        return Err(LauncherError::new(codes::GAME_RUNNING).with("exe", exe_file));
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn a_proton_style_first_argument_counts_as_running_and_stops_counting_when_it_exits() {
        let name = format!("Astra Probe {}.exe", std::process::id());
        assert!(!is_running(&name));
        let mut child = std::process::Command::new("sleep")
            .arg0(format!("Z:\\games\\{name}"))
            .arg("30")
            .spawn()
            .unwrap();
        let seen = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            is_running(&name.to_uppercase())
        });
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(seen, "the child's first argument names the program");
        assert!(!is_running(&name), "a reaped child is not running");
    }

    #[test]
    fn running_pids_names_the_child_whose_program_matches_and_no_other() {
        let name = format!("Astra Pid Probe {}.exe", std::process::id());
        assert!(running_pids(&name).is_empty());
        let mut child = std::process::Command::new("sleep")
            .arg0(format!("Z:\\games\\{name}"))
            .arg("30")
            .spawn()
            .unwrap();
        // An unrelated child with a near name: never counted.
        let mut other = std::process::Command::new("sleep")
            .arg0(format!("Z:\\games\\{name}.bak"))
            .arg("30")
            .spawn()
            .unwrap();
        let mut seen = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            seen = running_pids(&name.to_lowercase());
            if !seen.is_empty() {
                break;
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        other.kill().unwrap();
        other.wait().unwrap();
        assert_eq!(
            seen,
            [child.id()],
            "the child, by the same matcher as is_running"
        );
        assert!(
            running_pids(&name).is_empty(),
            "a reaped child is not running"
        );
    }
}
