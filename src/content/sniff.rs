// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a file IS, by its bytes and never by its name (`spec/games/manifest.md` §2.7.1, "the other
//! direction is read by bytes"): a text glTF under any name, the byte-order marks, and the
//! content catalogue's own tolerant sniff (`is_gltf_json`, `top_level_has_asset`,
//! `decoded_has_asset`, `bom_refusal`; `ingest-v1.md` §3 step 10), each read as AstraPlugins'
//! `tools/check-content-goldens.py` and `tools/check-games-goldens.py` read them.
//!
//! A game integration differs from a content bundle in two ways, both kept here: a UTF-16/32 text
//! is not refused for its mark but sniffed as the text it decodes to, a piece at a time
//! ([`decoded_pieces`]: a 32 MiB `.pak` that begins `FF FE` is never decoded whole for a test of
//! its first character), and the key-by-key sniff runs only on a file whose bytes name an `asset`
//! key with an object value at all ([`names_asset_object`], the C-speed prefilter), at most 16 MiB
//! a file and 64 MiB a release ([`SniffBudget`]); past either, the key alone refuses.

use super::zipwalk::MAX_GLTF_JSON;

pub(crate) const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

/// The per-character sniff reads at most this much of one release.
pub(crate) const SNIFF_BUDGET: u64 = 64 * 1024 * 1024;

/// A UTF-16 or UTF-32 encoding, by its byte-order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Utf32Be,
    Utf32Le,
    Utf16Le,
    Utf16Be,
}

/// The content contract's marks, UTF-32 first (`FF FE 00 00` also begins `FF FE`).
const BOMS: [(&[u8], Codec); 4] = [
    (b"\x00\x00\xfe\xff", Codec::Utf32Be),
    (b"\xff\xfe\x00\x00", Codec::Utf32Le),
    (b"\xff\xfe", Codec::Utf16Le),
    (b"\xfe\xff", Codec::Utf16Be),
];

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Utf32Be => "utf-32-be",
            Codec::Utf32Le => "utf-32-le",
            Codec::Utf16Le => "utf-16-le",
            Codec::Utf16Be => "utf-16-be",
        }
    }
}

/// The UTF-16/32 mark `raw` begins with, and its length.
pub(crate) fn bom_of(raw: &[u8]) -> Option<(usize, Codec)> {
    BOMS.iter()
        .find(|(bom, _)| raw.starts_with(bom))
        .map(|(bom, c)| (bom.len(), *c))
}

/// An incremental decoder of one codec: units cut between two pieces are carried over, and what
/// is no character is U+FFFD (Python's `"replace"`).
struct Decoder {
    codec: Codec,
    carry: Vec<u8>,
    high: Option<u16>,
}

impl Decoder {
    fn new(codec: Codec) -> Decoder {
        Decoder {
            codec,
            carry: Vec::new(),
            high: None,
        }
    }

    fn unit16(&mut self, u: u16, out: &mut String) {
        if let Some(h) = self.high.take() {
            if (0xDC00..0xE000).contains(&u) {
                let c = 0x10000 + ((u32::from(h) - 0xD800) << 10) + (u32::from(u) - 0xDC00);
                out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                return;
            }
            out.push('\u{FFFD}');
        }
        match u {
            0xD800..0xDC00 => self.high = Some(u),
            0xDC00..0xE000 => out.push('\u{FFFD}'),
            u => out.push(char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}')),
        }
    }

    fn feed(&mut self, bytes: &[u8], out: &mut String) {
        let width = match self.codec {
            Codec::Utf16Le | Codec::Utf16Be => 2,
            Codec::Utf32Le | Codec::Utf32Be => 4,
        };
        let mut all = std::mem::take(&mut self.carry);
        all.extend_from_slice(bytes);
        let whole = all.len() - all.len() % width;
        for u in all[..whole].chunks(width) {
            match self.codec {
                Codec::Utf16Le => self.unit16(u16::from_le_bytes([u[0], u[1]]), out),
                Codec::Utf16Be => self.unit16(u16::from_be_bytes([u[0], u[1]]), out),
                Codec::Utf32Le | Codec::Utf32Be => {
                    let v = if self.codec == Codec::Utf32Le {
                        u32::from_le_bytes([u[0], u[1], u[2], u[3]])
                    } else {
                        u32::from_be_bytes([u[0], u[1], u[2], u[3]])
                    };
                    out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                }
            }
        }
        self.carry = all[whole..].to_vec();
    }

    fn finish(&mut self, out: &mut String) {
        if self.high.take().is_some() {
            out.push('\u{FFFD}');
        }
        if !std::mem::take(&mut self.carry).is_empty() {
            out.push('\u{FFFD}');
        }
    }
}

/// The text of `raw` as UTF-8, `size` raw bytes at a time: a UTF-16/32 text decoded by its mark
/// (the mark itself dropped), anything else as it is. With `flush`, what is left at the end is
/// said too (a prefix of a file is read without it).
pub(crate) fn decoded_pieces(
    raw: &[u8],
    size: usize,
    flush: bool,
) -> Box<dyn Iterator<Item = Vec<u8>> + '_> {
    let Some((bom, codec)) = bom_of(raw) else {
        return Box::new(raw.chunks(size).map(<[u8]>::to_vec));
    };
    let mut dec = Decoder::new(codec);
    let body = &raw[bom..];
    let mut pieces = body.chunks(size);
    let mut done = false;
    Box::new(std::iter::from_fn(move || {
        if let Some(p) = pieces.next() {
            let mut s = String::new();
            dec.feed(p, &mut s);
            return Some(s.into_bytes());
        }
        if done || !flush {
            return None;
        }
        done = true;
        let mut s = String::new();
        dec.finish(&mut s);
        Some(s.into_bytes())
    }))
}

/// The whole text of `raw`, decoded by its mark, as UTF-8 — only for a file a cap already bounds
/// (`sniff_text`).
pub(crate) fn sniff_text(raw: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if bom_of(raw).is_none() {
        return std::borrow::Cow::Borrowed(raw);
    }
    std::borrow::Cow::Owned(decoded_pieces(raw, 1 << 20, true).flatten().collect())
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// The first byte of the text that is not JSON whitespace (after a UTF-8 mark at its very start),
/// read 64 raw bytes at a time; `None` when none is found — in a whole file, there is none; in
/// a prefix (`complete` false), the rest decides.
pub(crate) fn first_char(raw: &[u8], complete: bool) -> Option<u8> {
    let mut first = true;
    for mut piece in decoded_pieces(raw, 64, complete) {
        if first && piece.starts_with(UTF8_BOM) {
            piece.drain(..UTF8_BOM.len());
        }
        first = first && piece.is_empty();
        if let Some(&c) = piece.iter().find(|&&b| !is_ws(b)) {
            return Some(c);
        }
    }
    None
}

/// Whether the bytes name the key `asset` — each letter plain or `\u00XX`-escaped, as JSON decodes
/// both to the same key — followed by `:` and an object, at C speed (the games checker's
/// `_ASSET_OBJ`).
fn asset_object_in(hay: &[u8]) -> bool {
    let at_letter = |s: &[u8], i: usize, letter: u8| -> Option<usize> {
        if s.get(i) == Some(&letter) {
            return Some(i + 1);
        }
        let esc = format!("\\u00{letter:02x}");
        (s.get(i..i + 6) == Some(esc.as_bytes())).then_some(i + 6)
    };
    for q in memchr::memchr_iter(b'"', hay) {
        let mut i = q + 1;
        let mut ok = true;
        for &l in b"asset" {
            match at_letter(hay, i, l) {
                Some(n) => i = n,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok || hay.get(i) != Some(&b'"') {
            continue;
        }
        i += 1;
        while hay.get(i).is_some_and(|&b| is_ws(b)) {
            i += 1;
        }
        if hay.get(i) != Some(&b':') {
            continue;
        }
        i += 1;
        while hay.get(i).is_some_and(|&b| is_ws(b)) {
            i += 1;
        }
        if hay.get(i) == Some(&b'{') {
            return true;
        }
    }
    false
}

/// [`asset_object_in`] over the text of `raw`: a UTF-8 file searched as it is, a UTF-16/32 one in
/// decoded pieces of 1 MiB that overlap by 4 KiB (the games checker's `_names_key`).
pub(crate) fn names_asset_object(raw: &[u8]) -> bool {
    if bom_of(raw).is_none() {
        return asset_object_in(raw);
    }
    let mut tail: Vec<u8> = Vec::new();
    for piece in decoded_pieces(raw, 1 << 20, true) {
        let mut window = std::mem::take(&mut tail);
        window.extend_from_slice(&piece);
        if asset_object_in(&window) {
            return true;
        }
        tail = window[window.len().saturating_sub(4096)..].to_vec();
    }
    false
}

/// The content catalogue's `top_level_has_asset`: does the top-level object name an `asset`
/// member, read key by key? A tolerant scan over `text`'s characters: it tracks strings and
/// nesting, compares each depth-1 KEY (JSON-decoded, else as written), and never needs the rest
/// to parse.
pub(crate) fn top_level_has_asset(mut text: impl Iterator<Item = char>) -> bool {
    let mut c = text.next();
    while c.is_some_and(|c| matches!(c, ' ' | '\t' | '\r' | '\n')) {
        c = text.next();
    }
    if c != Some('{') {
        return false;
    }
    let (mut depth, mut expect_key) = (1i64, true);
    while let Some(c) = text.next() {
        if c == '"' {
            // The string's raw text, up to its unescaped closing quote (a backslash takes the
            // next character whatever it is); a key longer than 30 characters cannot be `asset`.
            let mut raw = String::new();
            let mut long = false;
            let mut closed = false;
            while let Some(x) = text.next() {
                if x == '"' {
                    closed = true;
                    break;
                }
                let mut push = |ch: char| {
                    if raw.len() <= 30 {
                        raw.push(ch);
                    } else {
                        long = true;
                    }
                };
                push(x);
                if x == '\\'
                    && let Some(y) = text.next()
                {
                    push(y);
                }
            }
            if depth == 1 && expect_key {
                let key = if long {
                    None
                } else if closed {
                    Some(serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or(raw))
                } else {
                    Some(raw)
                };
                if key.as_deref() == Some("asset") {
                    return true;
                }
                expect_key = false;
            }
            if !closed {
                return false;
            }
            continue;
        }
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            ',' if depth == 1 => expect_key = true,
            _ => {}
        }
    }
    false
}

/// The characters of `b`, invalid UTF-8 read as U+FFFD (`decode("utf-8", "replace")`).
fn lossy_chars(b: &[u8]) -> impl Iterator<Item = char> + '_ {
    b.utf8_chunks().flat_map(|chunk| {
        chunk
            .valid()
            .chars()
            .chain((!chunk.invalid().is_empty()).then_some('\u{FFFD}'))
    })
}

/// The content catalogue's `is_gltf_json` (S5): after an optional UTF-8 mark, the first
/// non-whitespace byte is `{` and the TOP-LEVEL object names an `asset` member, whether or not the
/// rest of it parses.
pub(crate) fn is_gltf_json(b: &[u8]) -> bool {
    let b = b.strip_prefix(UTF8_BOM).unwrap_or(b);
    if b.iter().find(|&&c| !is_ws(c)) != Some(&b'{') {
        return false;
    }
    top_level_has_asset(lossy_chars(b))
}

/// The characters of `raw` after its mark, decoded by `codec`.
fn decoded_chars(raw: &[u8]) -> impl Iterator<Item = char> + '_ {
    decoded_pieces(raw, 1 << 20, true).flat_map(|p| {
        String::from_utf8(p)
            .unwrap_or_default()
            .chars()
            .collect::<Vec<_>>()
    })
}

/// The content catalogue's `bom_refusal` (N1, narrowed by #132): a text kind (`.gltf`, `.json`,
/// `.toml`) led by a UTF-16/32 mark is refused, any other file only when it decodes to a glTF; a
/// `.gltf` led by a UTF-8 mark is refused too.
pub(crate) fn bom_refusal(path: &str, b: &[u8]) -> Option<String> {
    let lower = path.to_lowercase();
    if let Some((_, codec)) = bom_of(b) {
        if [".gltf", ".json", ".toml"]
            .iter()
            .any(|x| lower.ends_with(x))
        {
            let kind = path.rsplit('.').next().unwrap_or(path);
            return Some(format!(
                "a text file ({kind}) must be UTF-8, not {}",
                codec.name()
            ));
        }
        // `decoded_has_asset`: `{` after JSON whitespace (no other mark skipped), then a top-level
        // `asset` key.
        let mut chars = decoded_chars(b).peekable();
        while chars
            .next_if(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
            .is_some()
        {}
        if chars.peek() == Some(&'{') && top_level_has_asset(chars) {
            return Some(format!(
                "a glTF encoded {} under a non-.gltf name",
                codec.name()
            ));
        }
        return None;
    }
    if lower.ends_with(".gltf") && b.starts_with(UTF8_BOM) {
        return Some("a .gltf must be UTF-8 JSON without a byte-order mark (glTF 2.0)".to_owned());
    }
    None
}

/// What one release may make the gate spend: the bytes the per-character sniff reads, and the
/// glTF JSON it may parse (the content catalogue's 32 MiB per archive, here per RELEASE, its
/// embedded bundles and archives included).
#[derive(Debug, Default)]
pub(crate) struct SniffBudget {
    pub sniffed: u64,
    pub gltf_json: u64,
}

/// The games checker's `is_text_gltf`: the content catalogue's sniff over the text decoded by its
/// mark. First the C-speed test — no `asset` key with an object value anywhere is no glTF;
/// above 16 MiB, or once the release's sniff budget is spent, the key alone refuses.
pub(crate) fn is_text_gltf(raw: &[u8], budget: &mut SniffBudget) -> bool {
    if first_char(raw, true) != Some(b'{') || !names_asset_object(raw) {
        return false;
    }
    if raw.len() as u64 > MAX_GLTF_JSON {
        return true;
    }
    let text = sniff_text(raw);
    if budget.sniffed + text.len() as u64 > SNIFF_BUDGET {
        return true;
    }
    budget.sniffed += text.len() as u64;
    is_gltf_json(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        let mut out = b"\xff\xfe".to_vec();
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    #[test]
    fn the_top_level_asset_key_is_read_key_by_key() {
        let has = |s: &str| top_level_has_asset(s.chars());
        assert!(has(r#"{"asset":{}}"#));
        assert!(has(r#" {"a":1,"asset":1,<garbage>"#));
        assert!(has(r#"{"asset":{}}"#));
        assert!(!has(r#"{"meta":{"asset":{}}}"#));
        assert!(!has(r#"{"a":"asset"}"#));
        assert!(!has(r#"["asset"]"#));
        assert!(has(r#"{"x":[1,{"y":2}],"asset":0}"#));
        assert!(!has(r#"{"x":"\"asset"}"#));
        // An unterminated key is read as written.
        assert!(has(r#"{"asset"#));
        assert!(!has(r#"{"assets"}"#));
    }

    #[test]
    fn the_prefilter_needs_an_object_after_the_key() {
        assert!(asset_object_in(br#"{"asset" : {"version":"2.0"}}"#));
        assert!(asset_object_in(br#"{"asset":{}}"#));
        assert!(!asset_object_in(br#"{"asset":"hero.png"}"#));
        assert!(!asset_object_in(br#"{"Asset":{}}"#));
        assert!(names_asset_object(&utf16le(r#"{"asset":{}}"#)));
        assert!(!names_asset_object(&utf16le(r#"{"asset":1}"#)));
    }

    #[test]
    fn a_marked_text_is_sniffed_as_the_text_it_decodes_to_a_piece_at_a_time() {
        assert_eq!(first_char(&utf16le("  \n{"), true), Some(b'{'));
        assert_eq!(first_char(b"\xef\xbb\xbf {", true), Some(b'{'));
        assert_eq!(first_char(b" \xef\xbb\xbf{", true), Some(0xef));
        // Lone surrogates decode to U+FFFD, never `{`: the first piece decides.
        let mut pak = b"\xff\xfe".to_vec();
        pak.extend(std::iter::repeat_n([0x00, 0xd8], 1 << 20).flatten());
        assert_eq!(first_char(&pak, true), Some(0xef));
        let mut budget = SniffBudget::default();
        assert!(is_text_gltf(
            &utf16le(r#"{"asset":{"version":"2.0"}}"#),
            &mut budget
        ));
        assert!(!is_text_gltf(
            &utf16le(r#"{"greeting":"Привет"}"#),
            &mut budget
        ));
    }

    #[test]
    fn a_mark_refuses_a_text_kind_and_a_binary_only_when_it_decodes_to_a_gltf() {
        assert!(bom_refusal("a/strings.json", &utf16le("{}")).is_some());
        assert!(bom_refusal("a/x.bin", &utf16le(r#"{"asset":{}}"#)).is_some());
        assert!(bom_refusal("a/x.bin", &utf16le(r#"{"a":1}"#)).is_none());
        assert!(bom_refusal("a/x.gltf", b"\xef\xbb\xbf{}").is_some());
        assert!(bom_refusal("a/x.json", b"\xef\xbb\xbf{}").is_none());
    }
}
