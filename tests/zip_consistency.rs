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
use std::io::{Cursor, Read as _, Write as _};
use std::path::{Path, PathBuf};

use astra_game_launcher::{GiPackage, LauncherError, codes};
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

/// The writers' zips, with their descriptors counted: (deflated with bit 3, stored with bit 3).
fn writers() -> (Vec<(String, Vec<u8>)>, usize, usize) {
    let listed = sums("tests/fixtures/zip-writers");
    let mut zips = Vec::new();
    let (mut deflated, mut stored) = (0, 0);
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
                match f.compression() {
                    zip::CompressionMethod::Deflated => deflated += 1,
                    zip::CompressionMethod::Stored => stored += 1,
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
        zips.len() >= 9,
        "{:?}",
        zips.iter().map(|z| &z.0).collect::<Vec<_>>()
    );
    assert!(
        deflated > 0 && stored > 0,
        "the writers must reach both descriptor checks (deflated {deflated}, stored {stored}), or this passes them unread"
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

const DESCRIPTOR_SIGNATURE: [u8; 4] = 0x0807_4b50u32.to_le_bytes();

/// One entry as it is written: `data` is what follows the local header (stored or deflated
/// bytes), `after` what follows `data` (a descriptor, or nothing).
struct Raw {
    name: Vec<u8>,
    method: u16,
    flags: u16,
    crc: u32,
    compressed: u32,
    size: u32,
    data: Vec<u8>,
    after: Vec<u8>,
    local_extra: Vec<u8>,
    central_extra: Vec<u8>,
    /// The flags of the local header, when they differ from the central record's.
    local_flags: Option<u16>,
}

impl Raw {
    fn stored(name: &str, data: &[u8]) -> Raw {
        Raw {
            name: name.as_bytes().to_vec(),
            method: 0,
            flags: 0,
            crc: crc32fast::hash(data),
            compressed: data.len() as u32,
            size: data.len() as u32,
            data: data.to_vec(),
            after: Vec::new(),
            local_extra: Vec::new(),
            central_extra: Vec::new(),
            local_flags: None,
        }
    }

    /// `deflated` (which may carry bytes past its stream's end) recording `data`'s CRC and size.
    fn deflated(name: &str, deflated: &[u8], data: &[u8]) -> Raw {
        Raw {
            method: 8,
            compressed: deflated.len() as u32,
            size: data.len() as u32,
            data: deflated.to_vec(),
            ..Raw::stored(name, data)
        }
    }

    /// Stored with bit 3: zeros in the local header, `descriptor` after the data.
    fn stored_with_descriptor(name: &str, data: &[u8], descriptor: Vec<u8>) -> Raw {
        Raw {
            flags: 1 << 3,
            after: descriptor,
            ..Raw::stored(name, data)
        }
    }
}

/// A signed descriptor with 32-bit sizes.
fn descriptor(crc: u32, compressed: u32, size: u32) -> Vec<u8> {
    let mut d = DESCRIPTOR_SIGNATURE.to_vec();
    for v in [crc, compressed, size] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d
}

fn le(out: &mut Vec<u8>, v16: &[u16], v32: &[u32]) {
    for v in v16 {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in v32 {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// The zip of `entries`, after a stored `astra-gi.toml` holding [`MANIFEST`].
fn package(entries: Vec<Raw>) -> Vec<u8> {
    let mut all = vec![Raw::stored("astra-gi.toml", MANIFEST.as_bytes())];
    all.extend(entries);
    let mut zip = Vec::new();
    let mut offsets = Vec::new();
    for e in &all {
        offsets.push(zip.len() as u32);
        let local_flags = e.local_flags.unwrap_or(e.flags);
        let (crc, compressed, size) = if e.flags & (1 << 3) != 0 {
            (0, 0, 0)
        } else {
            (e.crc, e.compressed, e.size)
        };
        le(&mut zip, &[], &[0x0403_4b50]);
        le(
            &mut zip,
            &[20, local_flags, e.method, 0, 0x5021],
            &[crc, compressed, size],
        );
        le(
            &mut zip,
            &[e.name.len() as u16, e.local_extra.len() as u16],
            &[],
        );
        zip.extend_from_slice(&e.name);
        zip.extend_from_slice(&e.local_extra);
        zip.extend_from_slice(&e.data);
        zip.extend_from_slice(&e.after);
    }
    let cd_start = zip.len() as u32;
    for (e, offset) in all.iter().zip(&offsets) {
        le(&mut zip, &[], &[0x0201_4b50]);
        le(
            &mut zip,
            &[20, 20, e.flags, e.method, 0, 0x5021],
            &[e.crc, e.compressed, e.size],
        );
        le(
            &mut zip,
            &[e.name.len() as u16, e.central_extra.len() as u16, 0, 0, 0],
            &[0, *offset],
        );
        zip.extend_from_slice(&e.name);
        zip.extend_from_slice(&e.central_extra);
    }
    let cd_len = zip.len() as u32 - cd_start;
    let n = all.len() as u16;
    le(&mut zip, &[], &[0x0605_4b50]);
    le(&mut zip, &[0, 0, n, n], &[cd_len, cd_start]);
    le(&mut zip, &[0], &[]);
    zip
}

fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
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

/// Check (5): a stored entry with bit 3 ends, for a scanning reader, at the first descriptor
/// signature followed by the CRC of the bytes before it. One inside the data cuts the entry short
/// for that reader, and what follows is free to look like the next local header. The signature
/// alone, with any other CRC, is data.
#[test]
fn a_stored_entry_hiding_a_descriptor_is_refused_and_a_bare_signature_is_not() {
    let prefix = b"#!/bin/sh\n";
    let content = |crc: u32| {
        let mut c = prefix.to_vec();
        c.extend_from_slice(&descriptor(crc, prefix.len() as u32, prefix.len() as u32));
        c.extend_from_slice(b"PK\x03\x04 whatever comes next");
        c
    };
    let entry = |data: Vec<u8>| {
        let d = descriptor(crc32fast::hash(&data), data.len() as u32, data.len() as u32);
        Raw::stored_with_descriptor("payload/a.txt", &data, d)
    };

    let hiding = package(vec![entry(content(crc32fast::hash(prefix)))]);
    zip_9_reads(&hiding);
    let e = refusal(&hiding);
    assert_eq!(
        said(&e),
        (
            codes::ZIP_INCONSISTENT,
            Some("payload/a.txt"),
            Some("descriptor-inside")
        )
    );
    assert_eq!(e.param("at"), Some("10"));

    open(&package(vec![entry(content(crc32fast::hash(prefix) ^ 1))]))
        .expect("a signature without its CRC is data");
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

/// Check (5) is one pass: 4 MiB of nothing but descriptor signatures, each a candidate, is read
/// once (re-hashing the prefix at every signature grows with its square).
#[test]
fn a_stored_entry_of_signatures_is_scanned_in_one_pass() {
    let data: Vec<u8> = DESCRIPTOR_SIGNATURE
        .iter()
        .copied()
        .cycle()
        .take(4 << 20)
        .collect();
    let d = descriptor(crc32fast::hash(&data), data.len() as u32, data.len() as u32);
    let zip = package(vec![Raw::stored_with_descriptor(
        "payload/sig.bin",
        &data,
        d,
    )]);
    let started = std::time::Instant::now();
    open(&zip).unwrap();
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
