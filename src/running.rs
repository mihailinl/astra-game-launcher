// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Best effort: is a process with the game's program name running? Under Proton the game's
//! process carries its Windows path (`Z:\…\Game.exe`) as its first argument.

use crate::error::{LauncherError, Result, codes};

fn base_name(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

#[cfg(target_os = "linux")]
pub(crate) fn is_running(exe_file: &str) -> bool {
    let me = std::process::id().to_string();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str() else { continue };
        if !pid.bytes().all(|b| b.is_ascii_digit()) || pid == me {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or(&[]);
        let argv0 = String::from_utf8_lossy(argv0);
        if base_name(&argv0).eq_ignore_ascii_case(exe_file) {
            return true;
        }
    }
    false
}

#[cfg(windows)]
pub(crate) fn is_running(exe_file: &str) -> bool {
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
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            if entry.th32ProcessID != me {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if base_name(&name).eq_ignore_ascii_case(exe_file) {
                    found = true;
                    break;
                }
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) fn is_running(_exe_file: &str) -> bool {
    false
}

/// Refuses with `game-running` when the game's program is running.
pub(crate) fn refuse_if_running(exe_file: &str) -> Result<()> {
    if is_running(exe_file) {
        return Err(LauncherError::new(codes::GAME_RUNNING).with("exe", exe_file));
    }
    Ok(())
}
