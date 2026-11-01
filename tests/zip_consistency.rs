// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Zip cloaking (`src/zip_consistency.rs`), through [`GiPackage::open`]:
//!
//! - AstraPlugins' three vectors (`tests/fixtures/zip-cloaking/`, held to its digests) are read by
//!   zip 9 alone without a complaint, and refused here, each for its own reason;
//! - one integration packed by every common writer (`tests/fixtures/zip-writers/`), data
//!   descriptors included, still opens;
//! - each check's shape, built by hand into a package that would otherwise open (zip 9 reads its
//!   manifest and its files), is refused as `zip-inconsistent` — and the near misses are not.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};

use astra_game_launcher::{GiPackage, LauncherError, codes};
use common::rawzip::*;
use common::sha;

fn dir(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn open(zip: &[u8]) -> Result<GiPackage, LauncherError> {
    GiPackage::open(zip, &sha(zip))
}

fn refusal(zip: &[u8]) -> LauncherError {
    match open(zip) {
        Ok(_) => panic!("the package opened"),
        Err(e) => e,
    }
}

/// `(code, entry, reason)` of a refusal.
fn said(e: &LauncherError) -> (&str, Option<&str>, Option<&str>) {
    (e.code, e.param("entry"), e.param("reason"))
}

/// `SHA256SUMS` in `rel`: file name → digest.
fn sums(rel: &str) -> BTreeMap<String, String> {
    fs::read_to_string(dir(rel).join("SHA256SUMS"))
        .unwrap()
        .lines()
        .map(|l| {
            let (digest, name) = l.split_once("  ").expect("sha256sum format");
            (name.to_owned(), digest.to_owned())
        })
        .collect()
}

/// AstraPlugins' vectors, the digests its `SHA256SUMS` states (the ones Astra's daemon pins too),
/// and what each is refused for.
const VECTORS: [(&str, &str, &str, &str); 3] = [
    (
        "slack.zip",
        "71511e42a17bb35af2db71c72f2a45fe95754ce73ddfcda417968a77a92fe6f3",
        "a.txt",
        "deflate-end",
    ),
    (
        "upath.zip",
        "4649643cdf69d4e2f650209d73b49c190ea8d9a7d80888d5c1c174af093fc59f",
        "notes.txt",
        "unicode-path",
    ),
    (
        "stored-dd.zip",
        "3841c473f7ade57d1efca72aa33847d9508773df2147797a8bdb873828f691fb",
        "notes.txt",
        "descriptor-missing",
    ),
];

fn vector(name: &str) -> Vec<u8> {
    let bytes = fs::read(dir("tests/fixtures/zip-cloaking").join(name)).unwrap();
    let want = VECTORS.iter().find(|v| v.0 == name).unwrap().1;
    assert_eq!(
        sha(&bytes),
        want,
        "tests/fixtures/zip-cloaking/{name} is not AstraPlugins' golden: copy it from there, never regenerate it"
    );
    bytes
}

#[test]
fn the_vectors_are_astraplugins_bytes() {
    let listed = sums("tests/fixtures/zip-cloaking");
    let want: BTreeMap<String, String> = VECTORS
        .iter()
        .map(|(n, d, _, _)| ((*n).to_owned(), (*d).to_owned()))
        .collect();
    assert_eq!(listed, want, "SHA256SUMS is AstraPlugins' own");
    for (name, ..) in VECTORS {
        vector(name);
    }
}

/// What the vectors show is this crate's check, not zip's: zip 9 opens each, its own consistency
/// check passes, and every entry reads, CRC and all.
#[test]
fn zip_9_alone_reads_every_vector_without_a_complaint() {
    for (name, ..) in VECTORS {
        let bytes = vector(name);
        let mut ar = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        ar.validate_header_consistency().unwrap_or_else(|e| {
            panic!("{name}: zip 9's own check refuses it ({e}), so it shows nothing of ours")
        });
        for i in 0..ar.len() {
            let mut out = Vec::new();
            ar.by_index(i).unwrap().read_to_end(&mut out).unwrap();
        }
    }
}

#[test]
fn every_vector_is_refused_as_two_archives_for_its_own_reason() {
    for (name, _, entry, reason) in VECTORS {
        let e = refusal(&vector(name));
        assert_eq!(
            said(&e),
            (codes::ZIP_INCONSISTENT, Some(entry), Some(reason)),
            "{name}: {e}"
        );
    }
}

/// Whether the local header at `at` carries a ZIP64 extra field (0x0001).
fn local_zip64(zip: &[u8], at: usize) -> bool {
    let u16_at = |i: usize| u16::from_le_bytes([zip[i], zip[i + 1]]);
    let (name, extra) = (usize::from(u16_at(at + 26)), usize::from(u16_at(at + 28)));
    let block = &zip[at + 30 + name..at + 30 + name + extra];
    let mut i = 0;
    while i + 4 <= block.len() {
        if u16::from_le_bytes([block[i], block[i + 1]]) == 0x0001 {
            return true;
        }
        i += 4 + usize::from(u16::from_le_bytes([block[i + 2], block[i + 3]]));
    }
    false
}

/// Each writer's zip: (its file name, its bytes).
type Zips = Vec<(String, Vec<u8>)>;

/// The writers' zips, with their descriptors counted: (deflated with bit 3, stored with bit 3),
/// each as [32-bit, 64-bit] (an entry whose local header carries a ZIP64 field).
fn writers() -> (Zips, [usize; 2], [usize; 2]) {
    let listed = sums("tests/fixtures/zip-writers");
    let mut zips = Vec::new();
    let (mut deflated, mut stored) = ([0, 0], [0, 0]);
    for entry in fs::read_dir(dir("tests/fixtures/zip-writers")).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_none_or(|x| x != "zip") {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(&p).unwrap();
        assert_eq!(listed.get(&name), Some(&sha(&bytes)), "{name}: SHA256SUMS");
        let mut ar = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        for i in 0..ar.len() {
            let f = ar.by_index_raw(i).unwrap();
            if f.flags().is_using_data_descriptor() {
                let wide = usize::from(local_zip64(&bytes, f.header_start() as usize));
                match f.compression() {
                    zip::CompressionMethod::Deflated => deflated[wide] += 1,
                    zip::CompressionMethod::Stored => stored[wide] += 1,
                    _ => {}
                }
            }
        }
        zips.push((name, bytes));
    }
    assert_eq!(zips.len(), listed.len(), "a zip SHA256SUMS lists is gone");
    zips.sort();
    (zips, deflated, stored)
}

/// The check refuses a descriptor or a stream that lies, never one that is there: every writer's
/// package opens and places what its manifest names, the flagged-UTF-8 name included.
#[test]
fn every_common_writers_package_opens() {
    let (zips, deflated, stored) = writers();
    assert!(
        zips.len() >= 11,
        "{:?}",
        zips.iter().map(|z| &z.0).collect::<Vec<_>>()
    );
    assert!(
        deflated.iter().chain(&stored).all(|n| *n > 0),
        "the writers must reach both descriptor checks, 32- and 64-bit (deflated {deflated:?}, stored {stored:?}), or this passes them unread"
    );
    for (name, bytes) in &zips {
        let pkg = open(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let placed: BTreeMap<String, String> = pkg
            .placed_files()
            .into_iter()
            .map(|f| (f.dest, f.sha256))
            .collect();
        assert_eq!(
            placed.get("${profile}/payload/café.txt"),
            Some(&sha("une café\n".as_bytes())),
            "{name}: {placed:?}"
        );
        assert_eq!(placed.len(), 4, "{name}: {placed:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Hand-built packages: a stored manifest and one payload entry, exactly as specified.
// ---------------------------------------------------------------------------------------------

const MANIFEST: &str = r#"schema = 1

[integration]
id = "test.cloak"
name = "Cloak"
version = "1.0.0"
authors = ["tester"]
license = "MIT"

[target]
engine = "unity-mono"
exe = "*"
platforms = ["windows-x64"]
anti_cheat = "none"

[[files]]
from = "payload/**"
to = "${profile}/payload/"
"#;

/// The zip of `entries`, after a stored `astra-gi.toml` holding [`MANIFEST`].
fn package(entries: Vec<Raw>) -> Vec<u8> {
    let mut all = vec![Raw::stored("astra-gi.toml", MANIFEST.as_bytes())];
    all.extend(entries);
    archive(&all)
}

/// zip 9 alone reads the hand-built package as an ordinary one: its consistency check passes and
/// every entry reads, so a refusal is this crate's.
fn zip_9_reads(zip: &[u8]) {
    let mut ar = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
    ar.validate_header_consistency().unwrap();
    for i in 0..ar.len() {
        let mut out = Vec::new();
        ar.by_index(i).unwrap().read_to_end(&mut out).unwrap();
    }
}

/// Check (4) without a descriptor: the recorded size runs four bytes past the deflate stream.
/// zip 9 inflates the stream and stops; a reader that takes its sizes from elsewhere (a ZIP64
/// field in the local block, where Java looks) reads the slack.
#[test]
fn a_deflate_stream_ending_before_its_record_is_refused() {
    let data = b"#!/bin/sh\necho hello\n";
    let mut slack = deflate(data, 6);
    let stream = slack.len();
    slack.extend_from_slice(b"SLAK");
    let zip = package(vec![Raw::deflated("payload/run.sh", &slack, data)]);
    zip_9_reads(&zip);
    let e = refusal(&zip);
    assert_eq!(
        said(&e),
        (
            codes::ZIP_INCONSISTENT,
            Some("payload/run.sh"),
            Some("deflate-end")
        )
    );
    assert_eq!(e.param("at"), Some(stream.to_string().as_str()));
    assert_eq!(e.param("recorded"), Some((stream + 4).to_string().as_str()));

    // Without the slack, the same package opens.
    let whole = package(vec![Raw::deflated(
        "payload/run.sh",
        &deflate(data, 6),
        data,
    )]);
    open(&whole).unwrap();
}

/// A deflate stream whose last input byte still owes more than one output buffer (65,537 bytes, a
/// one and zeros, deflate to a handful): the inflater is drained after its last input byte, so the
/// stream is whole (AstraPlugins' review round 2).
#[test]
fn a_deflate_stream_that_owes_output_after_its_last_byte_opens() {
    let mut data = vec![0u8; 64 * 1024 + 1];
    data[0] = 1;
    let deflated = deflate(&data, 9);
    {
        let mut inflater = flate2::Decompress::new(false);
        let mut out = vec![0u8; 64 * 1024];
        let mut status = flate2::Status::Ok;
        while (inflater.total_in() as usize) < deflated.len() && status != flate2::Status::StreamEnd
        {
            let at = inflater.total_in() as usize;
            status = inflater
                .decompress(&deflated[at..], &mut out, flate2::FlushDecompress::None)
                .unwrap();
        }
        assert_ne!(
            status,
            flate2::Status::StreamEnd,
            "the fixture must owe output after its last input byte"
        );
    }
    let pkg = open(&package(vec![Raw::deflated(
        "payload/zeros.bin",
        &deflated,
        &data,
    )]))
    .unwrap();
    assert!(pkg.placed_files().iter().any(|f| f.sha256 == sha(&data)));
}

/// Check (4)'s other halves: a stream that inflates past what it declares, or to another CRC.
#[test]
fn a_deflate_stream_with_another_size_or_crc_is_refused() {
    let data = b"payload payload payload\n";
    let deflated = deflate(data, 6);
    let mut short = Raw::deflated("payload/a.txt", &deflated, data);
    short.size -= 1;
    let e = refusal(&package(vec![short]));
    assert_eq!(e.code, codes::ZIP_INCONSISTENT, "{e}");
    assert_eq!(e.param("reason"), Some("deflate-size"), "{e}");

    let mut other_crc = Raw::deflated("payload/a.txt", &deflated, data);
    other_crc.crc ^= 1;
    let e = refusal(&package(vec![other_crc]));
    assert_eq!(
        said(&e),
        (codes::ZIP_INCONSISTENT, Some("payload/a.txt"), Some("crc"))
    );

    let garbage = Raw::deflated("payload/a.txt", b"\xff\xff\xff\xff", data);
    let e = refusal(&package(vec![garbage]));
    assert_eq!(e.code, codes::ZIP_INCONSISTENT, "{e}");
}

/// Check (5): a stored entry with bit 3 ends, for a scanning reader, at a descriptor signature
/// inside its data. libarchive's read path ends it at the first signature followed by the CRC of
/// the bytes before it (`stored-dd.zip`); its skip path — `tar -t`, and every entry a selective
/// extract passes over — at the first signature whatever follows it. So a signature anywhere in
/// the data is refused, with the CRC of what precedes it or any other.
#[test]
fn a_stored_entry_holding_a_descriptor_signature_is_refused_whatever_follows_it() {
    let prefix = b"#!/bin/sh\n";
    let content = |crc: u32, compressed: u32, size: u32| {
        let mut c = prefix.to_vec();
        c.extend_from_slice(&descriptor(crc, compressed, size));
        c.extend_from_slice(b"PK\x03\x04 whatever comes next");
        c
    };
    let n = prefix.len() as u32;
    let crc = crc32fast::hash(prefix);
    for (why, data) in [
        ("the CRC of the bytes before it", content(crc, n, n)),
        ("another CRC, other sizes", content(crc ^ 1, n + 1, n + 1)),
        ("another CRC, the sizes", content(crc ^ 1, n, n)),
        ("the CRC, other sizes", content(crc, n + 1, n + 1)),
    ] {
        let hiding = package(vec![Raw::streamed("payload/a.txt", &data)]);
        zip_9_reads(&hiding);
        let e = refusal(&hiding);
        assert_eq!(
            said(&e),
            (
                codes::ZIP_INCONSISTENT,
                Some("payload/a.txt"),
                Some("descriptor-inside")
            ),
            "{why}: {e}"
        );
        assert_eq!(e.param("at"), Some("10"), "{why}");
    }
    // A signature at the data's very start, and one straddling its last bytes and the real
    // descriptor (which cannot match: the descriptor begins with the signature itself).
    let e = refusal(&package(vec![Raw::streamed(
        "payload/a.txt",
        b"PK\x07\x08rest",
    )]));
    assert_eq!(e.param("reason"), Some("descriptor-inside"), "{e}");
    assert_eq!(e.param("at"), Some("0"));
    for tail in [&b"x PK"[..], b"x PK\x07", b"x P"] {
        open(&package(vec![Raw::streamed("payload/a.txt", tail)]))
            .unwrap_or_else(|e| panic!("{tail:?}: {e}"));
    }
    // Three of the signature's four bytes are data.
    open(&package(vec![Raw::streamed(
        "payload/a.txt",
        b"PK\x07\x09 PK\x06\x08 PL\x07\x08 QK\x07\x08",
    )]))
    .expect("no signature, no descriptor");
}

/// The reviewer's vector, as libarchive 3.8.10's streaming reader reads it: a stored entry with
/// bit 3 whose data holds a bare signature (another CRC behind it), then a local header for
/// `payload/hidden.txt` whose size swallows the entry's real descriptor and the next entry,
/// `payload/evil.dll`. That reader lists `astra-gi.toml`, `payload/x.bin`, `payload/hidden.txt`,
/// `payload/readme.txt` — and the DLL that every reader of the directory installs is not there.
#[test]
fn a_bare_signature_that_hides_an_installed_dll_from_a_streaming_reader_is_refused() {
    let hidden_name = b"payload/hidden.txt";
    let evil = Raw::stored("payload/evil.dll", b"MZ evil");
    // What the hidden entry swallows: the real descriptor (16 bytes), then evil.dll whole.
    let swallowed = (16 + local_header(&evil).len() + evil.data.len()) as u32;
    let fake = Raw {
        compressed: swallowed,
        size: swallowed,
        crc: 0,
        ..Raw::stored("payload/hidden.txt", b"")
    };
    let mut data = b"AAAA".to_vec();
    data.extend_from_slice(&DESCRIPTOR_SIGNATURE);
    data.extend_from_slice(&0xdead_beefu32.to_le_bytes());
    data.extend_from_slice(b"junk");
    data.extend_from_slice(&local_header(&fake));
    assert!(local_header(&fake).ends_with(hidden_name));
    let zip = package(vec![
        Raw::streamed("payload/x.bin", &data),
        evil,
        Raw::stored("payload/readme.txt", b"read me\n"),
    ]);
    zip_9_reads(&zip);
    let e = refusal(&zip);
    assert_eq!(
        said(&e),
        (
            codes::ZIP_INCONSISTENT,
            Some("payload/x.bin"),
            Some("descriptor-inside")
        ),
        "{e}"
    );
    assert_eq!(e.param("at"), Some("4"));
}

/// Check (5)'s descriptor: exactly at the data's end, signed, with the record's CRC and sizes. An
/// unsigned one is refused although AstraPlugins' README allows it: a scanning reader cannot find
/// it (debt row 34).
#[test]
fn a_stored_entry_needs_its_signed_descriptor_at_its_end() {
    let data = b"harmless notes\n";
    let (crc, n) = (crc32fast::hash(data), data.len() as u32);
    let with = |d: Vec<u8>| {
        package(vec![Raw::stored_with_descriptor(
            "payload/notes.txt",
            data,
            d,
        )])
    };

    open(&with(descriptor(crc, n, n))).unwrap();
    for (why, d) in [
        ("unsigned", descriptor(crc, n, n)[4..].to_vec()),
        ("another CRC", descriptor(crc ^ 1, n, n)),
        ("other sizes", descriptor(crc, n + 1, n + 1)),
        ("another size", descriptor(crc, n, n + 1)),
        ("another compressed size", descriptor(crc, n + 1, n)),
        ("none", Vec::new()),
    ] {
        let zip = with(d);
        zip_9_reads(&zip);
        let e = refusal(&zip);
        assert_eq!(
            said(&e),
            (
                codes::ZIP_INCONSISTENT,
                Some("payload/notes.txt"),
                Some("descriptor-missing")
            ),
            "{why}: {e}"
        );
    }
}

/// Check (5) for a Deflated entry with bit 3: a reader of local headers ends it where its stream
/// ends — check (4) holds that to the record — and reads a descriptor there. Without one, it reads
/// the next header out of step.
#[test]
fn a_deflated_entry_with_bit_3_needs_its_signed_descriptor_at_its_end() {
    let data = b"#!/bin/sh\necho hello\n";
    let deflated = deflate(data, 6);
    let entry = |after: Vec<u8>| Raw {
        flags: 1 << 3,
        after,
        ..Raw::deflated("payload/run.sh", &deflated, data)
    };
    let (crc, c, n) = (
        crc32fast::hash(data),
        deflated.len() as u32,
        data.len() as u32,
    );
    open(&package(vec![entry(descriptor(crc, c, n))])).unwrap();
    for (why, after) in [
        ("none", Vec::new()),
        ("unsigned", descriptor(crc, c, n)[4..].to_vec()),
        ("another CRC", descriptor(crc ^ 1, c, n)),
        ("another size", descriptor(crc, c, n + 1)),
        ("another compressed size", descriptor(crc, c + 1, n)),
        ("64-bit sizes, no ZIP64 field", descriptor64(crc, c, n)),
    ] {
        let zip = package(vec![entry(after)]);
        zip_9_reads(&zip);
        let e = refusal(&zip);
        assert_eq!(
            said(&e),
            (
                codes::ZIP_INCONSISTENT,
                Some("payload/run.sh"),
                Some("descriptor-missing")
            ),
            "{why}: {e}"
        );
    }
}

/// Check (5)'s width: an entry whose local header carries a ZIP64 field (what a writer that
/// cannot know a size writes: `tests/fixtures/zip-writers/python-stream-zip64*.zip`) ends on a
/// descriptor with 64-bit sizes, and one with 32-bit sizes is refused — Stored and Deflated.
#[test]
fn a_zip64_local_header_takes_a_64_bit_descriptor() {
    let data = b"#!/bin/sh\necho hello\n";
    let deflated = deflate(data, 6);
    let mut zip64 = vec![0x01, 0x00, 16, 0];
    zip64.extend_from_slice(&[0u8; 16]);
    let (crc, n) = (crc32fast::hash(data), data.len() as u32);
    let c = deflated.len() as u32;
    let stored = |after: Vec<u8>| Raw {
        local_extra: zip64.clone(),
        ..Raw::stored_with_descriptor("payload/run.sh", data, after)
    };
    let deflated_entry = |after: Vec<u8>| Raw {
        flags: 1 << 3,
        after,
        local_extra: zip64.clone(),
        ..Raw::deflated("payload/run.sh", &deflated, data)
    };
    for (why, wide, narrow) in [
        (
            "stored",
            stored(descriptor64(crc, n, n)),
            stored(descriptor(crc, n, n)),
        ),
        (
            "deflated",
            deflated_entry(descriptor64(crc, c, n)),
            deflated_entry(descriptor(crc, c, n)),
        ),
    ] {
        let zip = package(vec![wide]);
        zip_9_reads(&zip);
        open(&zip).unwrap_or_else(|e| panic!("{why}, 64-bit: {e}"));
        let zip = package(vec![narrow]);
        zip_9_reads(&zip);
        let e = refusal(&zip);
        assert_eq!(
            said(&e),
            (
                codes::ZIP_INCONSISTENT,
                Some("payload/run.sh"),
                Some("descriptor-missing")
            ),
            "{why}, 32-bit: {e}"
        );
    }
}

/// Check (4)'s guard against a stream that never ends: a Deflated entry recording only the first
/// half of a valid stream (both headers agree, so zip 9 opens the archive) inflates until its
/// bytes run out and is refused there, within a deadline — never a loop that waits for input
/// that is not coming. Unplaced, so nothing but the check reads it.
#[test]
fn a_deflate_stream_that_runs_out_is_refused_and_does_not_hang() {
    let data: Vec<u8> = (0..64 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let whole = deflate(&data, 6);
    let half = whole[..whole.len() / 2].to_vec();
    let opened = |stream: Vec<u8>| {
        let zip = package(vec![
            Raw::stored("payload/readme.txt", b"read me\n"),
            Raw::deflated("other/half.bin", &stream, &data),
        ]);
        let mut ar = zip::ZipArchive::new(Cursor::new(&zip)).unwrap();
        ar.validate_header_consistency().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(open(&zip).map(|_| ()));
        });
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .expect("open() did not return within 30 s")
    };
    let e = opened(half).unwrap_err();
    assert_eq!(
        said(&e),
        (
            codes::ZIP_INCONSISTENT,
            Some("other/half.bin"),
            Some("deflate-unended")
        ),
        "{e}"
    );
    opened(whole).expect("the whole stream");
}

/// Check (5) for a Stored entry with bit 3, past its descriptor: the data's own CRC is the
/// record's (zip 9 never reads an entry nothing places, so this check alone sees it), and a
/// Stored entry's two sizes are one.
#[test]
fn a_stored_entry_with_bit_3_has_its_records_crc_and_one_size() {
    let data = b"harmless notes\n";
    let (crc, n) = (crc32fast::hash(data), data.len() as u32);
    let entry = |crc: u32, compressed: u32, size: u32| Raw {
        crc,
        compressed,
        size,
        ..Raw::stored_with_descriptor("other/notes.txt", data, descriptor(crc, compressed, size))
    };
    let consistent = |zip: &[u8]| {
        let mut ar = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        ar.validate_header_consistency().unwrap();
    };
    let package = |e: Raw| package(vec![Raw::stored("payload/readme.txt", b"read me\n"), e]);
    open(&package(entry(crc, n, n))).expect("the record's CRC, one size");
    let zip = package(entry(crc ^ 1, n, n));
    consistent(&zip);
    assert_eq!(
        said(&refusal(&zip)),
        (
            codes::ZIP_INCONSISTENT,
            Some("other/notes.txt"),
            Some("crc")
        )
    );
    let zip = package(entry(crc, n, n + 1));
    consistent(&zip);
    assert_eq!(
        said(&refusal(&zip)),
        (
            codes::ZIP_INCONSISTENT,
            Some("other/notes.txt"),
            Some("stored-size")
        )
    );
}

/// Check (5) is one pass: 4 MiB of near-signatures (three of the four bytes, over and over) is
/// read once and opens, and the same with one real signature at its very end is refused there.
#[test]
fn a_stored_entry_of_near_signatures_is_scanned_in_one_pass() {
    let mut data: Vec<u8> = b"PK\x07\x09"
        .iter()
        .copied()
        .cycle()
        .take(4 << 20)
        .collect();
    let started = std::time::Instant::now();
    open(&package(vec![Raw::streamed("payload/sig.bin", &data)])).unwrap();
    let at = data.len();
    data.extend_from_slice(&DESCRIPTOR_SIGNATURE);
    let e = refusal(&package(vec![Raw::streamed("payload/sig.bin", &data)]));
    assert_eq!(e.param("reason"), Some("descriptor-inside"), "{e}");
    assert_eq!(e.param("at"), Some(at.to_string().as_str()));
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(20), "took {took:?}");
}

/// The Unicode Path field (0x7075) in any header, whatever its CRC: zip keeps the field only when
/// its CRC matches the stored name, and another reader may not ask.
#[test]
fn a_unicode_path_field_is_refused_in_either_header_whatever_its_crc() {
    let field = |crc: u32| {
        let mut body = vec![1u8];
        body.extend_from_slice(&crc.to_le_bytes());
        body.extend_from_slice(b"payload/launch.sh");
        let mut f = 0x7075u16.to_le_bytes().to_vec();
        f.extend_from_slice(&(body.len() as u16).to_le_bytes());
        f.extend_from_slice(&body);
        f
    };
    let name_crc = crc32fast::hash(b"payload/notes.txt");
    for (local, central) in [(true, true), (true, false), (false, true)] {
        for crc in [name_crc, name_crc ^ 1] {
            let mut e = Raw::stored("payload/notes.txt", b"payload");
            if local {
                e.local_extra = field(crc);
            }
            if central {
                e.central_extra = field(crc);
            }
            let zip = package(vec![e]);
            let err = refusal(&zip);
            // In one header only, zip 9's own comparison of the two refuses it first; in both,
            // with the wrong CRC, zip 9 ignores the field and reads the stored name.
            let (entry, reason) = if local && central {
                if crc != name_crc {
                    zip_9_reads(&zip);
                }
                ("payload/notes.txt", "unicode-path")
            } else {
                ("", "headers")
            };
            assert_eq!(
                said(&err),
                (codes::ZIP_INCONSISTENT, Some(entry), Some(reason)),
                "{local} {central} {crc}: {err}"
            );
        }
    }
}

/// Check (3): a non-ASCII name every reader decodes alike, or none. Unflagged UTF-8 is a name
/// zip 9 reads as UTF-8 and zip 8 (launcher 0.3.1) as CP437; a flag in one header only, or a
/// flagged name that is not UTF-8, is two names too.
#[test]
fn a_non_ascii_name_must_say_utf8_in_both_headers_and_be_it() {
    let name = "payload/café.txt";
    let flagged = |local: u16, central: u16, raw: &[u8]| {
        let mut e = Raw::stored(name, "une café\n".as_bytes());
        e.name = raw.to_vec();
        e.flags = central;
        e.local_flags = Some(local);
        e
    };
    let utf8 = name.as_bytes();
    let latin1 = b"payload/caf\xe9.txt";
    open(&package(vec![flagged(1 << 11, 1 << 11, utf8)])).expect("flagged UTF-8 in both headers");
    for (why, e) in [
        ("unflagged", flagged(0, 0, utf8)),
        ("central only", flagged(0, 1 << 11, utf8)),
        ("local only", flagged(1 << 11, 0, utf8)),
        ("flagged, not UTF-8", flagged(1 << 11, 1 << 11, latin1)),
        ("unflagged, not UTF-8", flagged(0, 0, latin1)),
    ] {
        let zip = package(vec![e]);
        zip_9_reads(&zip);
        let err = refusal(&zip);
        assert_eq!(err.code, codes::ZIP_INCONSISTENT, "{why}: {err}");
        assert_eq!(err.param("reason"), Some("name-encoding"), "{why}: {err}");
    }
}

/// Check (1): two records of one name. zip 9 keeps one and drops the other without a word, so
/// two readers read two different files under it.
#[test]
fn two_records_of_one_name_are_two_archives() {
    let zip = package(vec![
        Raw::stored("payload/a.txt", b"one"),
        Raw::stored("payload/a.txt", b"two"),
    ]);
    let e = refusal(&zip);
    assert_eq!(
        said(&e),
        (codes::ZIP_INCONSISTENT, Some(""), Some("headers")),
        "{e}"
    );
}

/// Check (7): bytes before the first entry, between two entries or before the central directory.
/// A reader of local headers takes them for the next header: libarchive 3.8.10's streaming reader
/// ends the archive at a `PK\x01\x02` (between `readme.txt` and `evil.dll` it lists the manifest
/// and `readme.txt` only; as a prefix, nothing at all), and Java's `ZipInputStream` at one zero
/// byte — while every reader of the directory installs `evil.dll`. zip 9's own scan of those bytes
/// looks for a valid local header and finds none.
#[test]
fn bytes_outside_every_entry_are_refused() {
    let entries = || {
        vec![
            Raw::stored("astra-gi.toml", MANIFEST.as_bytes()),
            Raw::stored("payload/readme.txt", b"read me\n"),
            Raw::stored("payload/evil.dll", b"MZ evil"),
        ]
    };
    let tight = archive(&entries());
    zip_9_reads(&tight);
    open(&tight).expect("the same entries, nothing between them");

    let central = b"PK\x01\x02".to_vec();
    for (why, at, gap, entry) in [
        (
            "a directory signature between entries",
            2,
            central.clone(),
            "payload/evil.dll",
        ),
        (
            "a directory signature as a prefix",
            0,
            central.clone(),
            "astra-gi.toml",
        ),
        (
            "a zero byte between entries",
            2,
            vec![0u8],
            "payload/evil.dll",
        ),
        ("a zero byte as a prefix", 0, vec![0u8], "astra-gi.toml"),
    ] {
        let mut e = entries();
        e[at].before = gap.clone();
        let zip = archive(&e);
        zip_9_reads(&zip);
        let err = refusal(&zip);
        assert_eq!(
            said(&err),
            (codes::ZIP_INCONSISTENT, Some(entry), Some("gap")),
            "{why}: {err}"
        );
        // Where the next header was wanted, and where it is.
        let want: usize = e[..at]
            .iter()
            .map(|r| local_header(r).len() + r.data.len())
            .sum();
        assert_eq!(err.param("at"), Some(want.to_string().as_str()), "{why}");
        assert_eq!(
            err.param("header"),
            Some((want + gap.len()).to_string().as_str()),
            "{why}"
        );
    }
    for (why, gap) in [
        ("a directory signature", central.clone()),
        ("a zero byte", vec![0u8]),
    ] {
        let zip = archive_with(&entries(), &gap);
        zip_9_reads(&zip);
        let err = refusal(&zip);
        assert_eq!(
            said(&err),
            (codes::ZIP_INCONSISTENT, Some(""), Some("gap")),
            "{why} before the directory: {err}"
        );
    }
    // A descriptor where bit 3 says none: bytes a reader of local headers takes for a header.
    let mut e = entries();
    e[1].after = descriptor(e[1].crc, e[1].compressed, e[1].size);
    let err = refusal(&archive(&e));
    assert_eq!(
        said(&err),
        (
            codes::ZIP_INCONSISTENT,
            Some("payload/evil.dll"),
            Some("gap")
        ),
        "{err}"
    );
}

/// The check reads the whole archive past an entry it does not inflate (encrypted, or of another
/// method; the hygiene that follows refuses those): a cloak behind one is still the refusal.
#[test]
fn an_entry_the_check_cannot_read_does_not_end_it() {
    let field = {
        let mut body = vec![1u8];
        body.extend_from_slice(&crc32fast::hash(b"payload/notes.txt").to_le_bytes());
        body.extend_from_slice(b"payload/launch.sh");
        let mut f = 0x7075u16.to_le_bytes().to_vec();
        f.extend_from_slice(&(body.len() as u16).to_le_bytes());
        f.extend_from_slice(&body);
        f
    };
    for (why, unread) in [
        (
            "encrypted",
            Raw {
                flags: 1,
                ..Raw::stored("payload/secret.bin", b"secret")
            },
        ),
        (
            "compression",
            Raw {
                method: 12,
                ..Raw::stored("payload/packed.bin", b"packed")
            },
        ),
    ] {
        let cloaked = Raw {
            local_extra: field.clone(),
            central_extra: field.clone(),
            ..Raw::stored("payload/notes.txt", b"notes")
        };
        let e = refusal(&package(vec![unread, cloaked]));
        assert_eq!(
            said(&e),
            (
                codes::ZIP_INCONSISTENT,
                Some("payload/notes.txt"),
                Some("unicode-path")
            ),
            "behind an entry refused for {why}: {e}"
        );
        // Behind nothing wrong, the unreadable entry is the hygiene's.
        let e = refusal(&package(vec![
            Raw {
                flags: if why == "encrypted" { 1 } else { 0 },
                method: if why == "compression" { 12 } else { 0 },
                ..Raw::stored("payload/x.bin", b"x")
            },
            Raw::stored("payload/notes.txt", b"notes"),
        ]));
        assert_eq!(
            (e.code, e.param("reason")),
            (codes::ZIP_UNSAFE_ENTRY, Some(why)),
            "{e}"
        );
    }
}

/// The work is bounded: what the checks inflate counts against `max_uncompressed_bytes`, so a
/// bomb is refused for its size before anything else is asked of it.
#[test]
fn the_checks_inflate_no_more_than_the_package_may_unpack_to() {
    let data = vec![0u8; 1 << 20];
    let zip = package(vec![Raw::deflated(
        "payload/zeros.bin",
        &deflate(&data, 9),
        &data,
    )]);
    let limits = astra_game_launcher::Limits {
        max_uncompressed_bytes: 64 * 1024,
        ..Default::default()
    };
    let e = GiPackage::open_with_limits(&zip, &sha(&zip), &limits).unwrap_err();
    assert_eq!(e.code, codes::TOO_LARGE, "{e}");
    assert_eq!(e.param("uncompressed"), Some("true"));
    // The checks' own refusal (the entry they were inflating), not the count of declared sizes
    // that follows them in `open`.
    assert_eq!(e.param("entry"), Some("payload/zeros.bin"), "{e}");
}
