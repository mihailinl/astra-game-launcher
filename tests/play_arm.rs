// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `remove_play_arm_if_older`: the daemon's crash guard removes a play-arm only when it is older
//! than the moment it decided to, under the profile lock, never through a link, never waiting on a
//! FIFO. (That a fresh arm `launch` writes meanwhile waits for the lock is a unit test in
//! `src/launch.rs`.)

mod common;

use std::fs;

use astra_game_launcher::{PLAY_ARM_FILE_NAME, remove_play_arm_if_older};
use common::*;

fn arm(started_unix_ms: u64) -> String {
    format!(r#"{{"schema":1,"appid":4000001,"started_unix_ms":{started_unix_ms}}}"#)
}

#[test]
fn an_arm_goes_only_when_it_is_older_than_the_decision() {
    let t = Tmp::new("arm");
    let p = t.path();
    let file = p.join(PLAY_ARM_FILE_NAME);
    assert!(!remove_play_arm_if_older(p, 1_000).unwrap(), "no arm");
    assert!(!remove_play_arm_if_older(&p.join("no-such-profile"), 1_000).unwrap());

    fs::write(&file, arm(1_000)).unwrap();
    assert!(
        !remove_play_arm_if_older(p, 1_000).unwrap(),
        "the same moment: kept"
    );
    assert!(!remove_play_arm_if_older(p, 999).unwrap(), "newer: kept");
    assert!(file.exists());
    assert!(
        remove_play_arm_if_older(p, 1_001).unwrap(),
        "older: removed"
    );
    assert!(!file.exists());

    // An arm that says no time, or is not JSON, is not judged older.
    for text in ["{\"schema\":1}", "not json"] {
        fs::write(&file, text).unwrap();
        assert!(!remove_play_arm_if_older(p, u64::MAX).unwrap(), "{text}");
        assert!(file.exists());
    }
    // A folder in its place is left alone.
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(!remove_play_arm_if_older(p, u64::MAX).unwrap());
    assert!(file.is_dir());
}

#[cfg(unix)]
#[test]
fn a_link_is_removed_as_a_link_and_never_followed() {
    let t = Tmp::new("arm-link");
    let p = t.path();
    // A NEWER arm elsewhere: read through the link, it would be kept.
    let elsewhere = p.join("elsewhere.json");
    fs::write(&elsewhere, arm(u64::MAX - 1)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, p.join(PLAY_ARM_FILE_NAME)).unwrap();
    assert!(remove_play_arm_if_older(p, 1_000).unwrap());
    assert!(fs::symlink_metadata(p.join(PLAY_ARM_FILE_NAME)).is_err());
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), arm(u64::MAX - 1));
}

#[cfg(unix)]
#[test]
fn a_fifo_in_its_place_never_blocks_and_is_left_alone() {
    let t = Tmp::new("arm-fifo");
    let p = t.path().to_path_buf();
    let made = std::process::Command::new("mkfifo")
        .arg(p.join(PLAY_ARM_FILE_NAME))
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("no mkfifo here: skipped");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let q = p.clone();
    std::thread::spawn(move || {
        let _ = tx.send(remove_play_arm_if_older(&q, u64::MAX).map_err(|e| e.code));
    });
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("waited on a FIFO");
    assert_eq!(r, Ok(false));
    assert!(fs::symlink_metadata(p.join(PLAY_ARM_FILE_NAME)).is_ok());
}

#[cfg(unix)]
#[test]
fn a_link_to_a_folder_is_removed_as_a_link() {
    let t = Tmp::new("arm-dir-link");
    let p = t.path();
    fs::create_dir(p.join("a-folder")).unwrap();
    fs::write(p.join("a-folder/keep.txt"), b"x").unwrap();
    std::os::unix::fs::symlink(p.join("a-folder"), p.join(PLAY_ARM_FILE_NAME)).unwrap();
    assert!(remove_play_arm_if_older(p, 1_000).unwrap());
    assert!(fs::symlink_metadata(p.join(PLAY_ARM_FILE_NAME)).is_err());
    assert!(p.join("a-folder/keep.txt").is_file());
}

/// An arm that is there and cannot be read (not ours to read: permissions) is an error, never a
/// silent "nothing to remove".
#[cfg(unix)]
#[test]
fn an_arm_that_cannot_be_read_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tmp::new("arm-unreadable");
    let p = t.path();
    let file = p.join(PLAY_ARM_FILE_NAME);
    fs::write(&file, arm(1)).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&file).is_ok() {
        eprintln!("running with the right to read anything: skipped");
        return;
    }
    let e = remove_play_arm_if_older(p, 1_000).unwrap_err();
    assert_eq!(e.code, astra_game_launcher::codes::IO);
    assert!(fs::symlink_metadata(&file).is_ok());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
}
