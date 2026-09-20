//! Text encoding detection, round-trip, and mojibake repair.
//!
//! Windows PowerShell 5.1 (`powershell.exe`) is why this module exists. Its
//! `Set-Content`/`Out-File` default to ANSI (Windows-1252), and its
//! `-Encoding UTF8` writes a BOM *and* re-encodes already-UTF-8 text through
//! cp1252, producing mojibake. A file written that way is not valid UTF-8, so
//! decoding it with `from_utf8_lossy` turns every accented character into
//! `U+FFFD` — the model then sees `�` where the file says `é`, and `edit` can
//! never match what it was shown.
//!
//! Two defences live here, shared by `read` and `edit`:
//!
//! * [`decode`] reads UTF-8 (with or without BOM), UTF-16 LE/BE, and
//!   Windows-1252 faithfully, reporting which it was so a write can round-trip
//!   it. [`encode`] is that round-trip.
//! * [`repair_mojibake`] reverses one round of cp1252→UTF-8 double-encoding,
//!   so `edit` can accept clean text against a file PowerShell corrupted and
//!   write it back as proper UTF-8. The round-trip is attempted per run and
//!   only applied where it succeeds, so correct text is provably untouched —
//!   the same safety property as `scripts/fix_mojibake.py`.

/// How a file's bytes are encoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextEncoding {
    /// UTF-8 without a BOM — the normal case for source files.
    Utf8,
    /// UTF-8 with a BOM. Preserved on write; the BOM is not part of the text.
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Windows-1252, a.k.a. "ANSI": every byte maps to a character, so decoding
    /// never fails. This is what PowerShell 5.1 writes without `-Encoding`.
    Windows1252,
}

/// Decode `bytes` and report the encoding so the caller can round-trip it.
///
/// The returned text never includes a BOM: a BOM is metadata, not content, and
/// leaving `U+FEFF` in the string is what breaks pattern matching at file start.
pub fn decode(bytes: &[u8]) -> (TextEncoding, String) {
    // UTF-16 LE BOM
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return (TextEncoding::Utf16Le, utf16_le(&bytes[2..]));
    }
    // UTF-16 BE BOM
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return (TextEncoding::Utf16Be, utf16_be(&bytes[2..]));
    }
    // UTF-8 BOM
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (
            TextEncoding::Utf8Bom,
            String::from_utf8_lossy(&bytes[3..]).into_owned(),
        );
    }
    // Heuristic: a NUL in every second byte is UTF-16 LE without a BOM. This
    // must be checked before UTF-8, because NUL is itself valid UTF-8 and a
    // BOM-less UTF-16 file would otherwise decode as UTF-8 full of NULs.
    if bytes.len() >= 4 && bytes[1] == 0 && bytes[3] == 0 {
        return (TextEncoding::Utf16Le, utf16_le(bytes));
    }
    // Everything else: valid UTF-8 if it can be, Windows-1252 if it cannot.
    // Trusting the system codepage here is what makes a PowerShell-written
    // `café` readable instead of `caf�`.
    match std::str::from_utf8(bytes) {
        Ok(s) => (TextEncoding::Utf8, s.to_string()),
        Err(_) => (
            TextEncoding::Windows1252,
            bytes.iter().map(|&b| cp1252_decode_byte(b)).collect(),
        ),
    }
}

/// Encode `text` back to `enc`, reporting whether a fallback happened.
///
/// Windows-1252 can only represent 256 characters. When the edited text gained
/// one it cannot hold (a `→`, say), the file is written as UTF-8 instead —
/// upgrading it rather than dropping the character — and the bool is `true` so
/// the caller can say so in its result.
pub fn encode_checked(enc: TextEncoding, text: &str) -> (Vec<u8>, bool) {
    match enc {
        TextEncoding::Utf8 => (text.as_bytes().to_vec(), false),
        TextEncoding::Utf8Bom => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            (out, false)
        }
        TextEncoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            (out, false)
        }
        TextEncoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            (out, false)
        }
        TextEncoding::Windows1252 => {
            let mut out = Vec::with_capacity(text.len());
            for c in text.chars() {
                match cp1252_encode_char(c) {
                    Some(b) => out.push(b),
                    // One unrepresentable character upgrades the whole file.
                    None => return (text.as_bytes().to_vec(), true),
                }
            }
            (out, false)
        }
    }
}

/// Undo one round of cp1252→UTF-8 double-encoding and drop a leading BOM.
///
/// Port of `scripts/fix_mojibake.py::repair`: walk maximal runs of characters
/// that are each encodable as a single cp1252 byte, encode the run back to
/// bytes, and replace it only when those bytes are valid UTF-8. Correct text is
/// therefore left untouched (`—` encodes to `0x97`, not valid UTF-8), and the
/// function is idempotent.
pub fn repair_mojibake(text: &str) -> String {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_mojibake_candidate(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // Maximal run of candidate characters.
        let mut j = i;
        while j < chars.len() && is_mojibake_candidate(chars[j]) {
            j += 1;
        }
        let run = &chars[i..j];
        match cp1252_bytes(run).and_then(|bytes| String::from_utf8(bytes).ok()) {
            Some(fixed) => out.push_str(&fixed),
            None => out.extend(run), // not mojibake — leave exactly as-is
        }
        i = j;
    }
    out
}

/// True if `c` is non-ASCII and encodes to exactly one cp1252 byte — the only
/// characters that can participate in a double-encoding run.
fn is_mojibake_candidate(c: char) -> bool {
    (c as u32) >= 0x80 && cp1252_encode_char(c).is_some()
}

fn cp1252_bytes(chars: &[char]) -> Option<Vec<u8>> {
    chars.iter().map(|&c| cp1252_encode_char(c)).collect()
}

/// Map one Windows-1252 byte to its character.
///
/// `0x00..=0x7F` is ASCII and `0xA0..=0xFF` is Latin-1. Only `0x80..=0x9F`
/// differs; the five slots Microsoft leaves undefined (`0x81`, `0x8D`, `0x8F`,
/// `0x90`, `0x9D`) map to their matching C1 control character, as the Windows
/// codec does, which keeps decode→encode lossless.
fn cp1252_decode_byte(b: u8) -> char {
    match b {
        0x80 => '\u{20AC}', // €
        0x82 => '\u{201A}', // ‚
        0x83 => '\u{0192}', // ƒ
        0x84 => '\u{201E}', // „
        0x85 => '\u{2026}', // …
        0x86 => '\u{2020}', // †
        0x87 => '\u{2021}', // ‡
        0x88 => '\u{02C6}', // ˆ
        0x89 => '\u{2030}', // ‰
        0x8A => '\u{0160}', // Š
        0x8B => '\u{2039}', // ‹
        0x8C => '\u{0152}', // Œ
        0x8E => '\u{017D}', // Ž
        0x91 => '\u{2018}', // '
        0x92 => '\u{2019}', // '
        0x93 => '\u{201C}', // "
        0x94 => '\u{201D}', // "
        0x95 => '\u{2022}', // •
        0x96 => '\u{2013}', // –
        0x97 => '\u{2014}', // —
        0x98 => '\u{02DC}', // ˜
        0x99 => '\u{2122}', // ™
        0x9A => '\u{0161}', // š
        0x9B => '\u{203A}', // ›
        0x9C => '\u{0153}', // œ
        0x9E => '\u{017E}', // ž
        0x9F => '\u{0178}', // Ÿ
        other => other as char,
    }
}

/// Inverse of [`cp1252_decode_byte`]. `None` when `c` has no cp1252 byte.
fn cp1252_encode_char(c: char) -> Option<u8> {
    match c {
        '\u{20AC}' => Some(0x80),
        '\u{201A}' => Some(0x82),
        '\u{0192}' => Some(0x83),
        '\u{201E}' => Some(0x84),
        '\u{2026}' => Some(0x85),
        '\u{2020}' => Some(0x86),
        '\u{2021}' => Some(0x87),
        '\u{02C6}' => Some(0x88),
        '\u{2030}' => Some(0x89),
        '\u{0160}' => Some(0x8A),
        '\u{2039}' => Some(0x8B),
        '\u{0152}' => Some(0x8C),
        '\u{017D}' => Some(0x8E),
        '\u{2018}' => Some(0x91),
        '\u{2019}' => Some(0x92),
        '\u{201C}' => Some(0x93),
        '\u{201D}' => Some(0x94),
        '\u{2022}' => Some(0x95),
        '\u{2013}' => Some(0x96),
        '\u{2014}' => Some(0x97),
        '\u{02DC}' => Some(0x98),
        '\u{2122}' => Some(0x99),
        '\u{0161}' => Some(0x9A),
        '\u{203A}' => Some(0x9B),
        '\u{0153}' => Some(0x9C),
        '\u{017E}' => Some(0x9E),
        '\u{0178}' => Some(0x9F),
        // Undefined-in-practice slots: decode produced these C1 controls, so
        // accept them back to keep the round-trip lossless.
        '\u{0081}' | '\u{008D}' | '\u{008F}' | '\u{0090}' | '\u{009D}' => Some(c as u8),
        // ASCII and the Latin-1 supplement are their own byte.
        c if (c as u32) < 0x80 => Some(c as u8),
        c if (0xA0..=0xFF).contains(&(c as u32)) => Some(c as u8),
        _ => None,
    }
}

// ── Line endings ─────────────────────────────────────────────────────────────

/// Detect the dominant line ending in `content`, defaulting to LF.
pub fn detect_line_ending(content: &str) -> &'static str {
    match (content.find('\n'), content.find("\r\n")) {
        (None, _) => "\n",
        (_, None) => "\n",
        (Some(lf), Some(crlf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// Normalize CRLF and lone-CR endings to LF.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore `\r\n` endings when the original file used them.
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

fn utf16_le(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn utf16_be(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode without caring about the fallback flag.
    fn to_bytes(e: TextEncoding, t: &str) -> Vec<u8> {
        encode_checked(e, t).0
    }

    #[test]
    fn utf8_without_bom_round_trips() {
        let (enc, text) = decode("café — §\n".as_bytes());
        assert_eq!(enc, TextEncoding::Utf8);
        assert_eq!(text, "café — §\n");
        assert_eq!(to_bytes(enc, &text), "café — §\n".as_bytes());
    }

    #[test]
    fn utf8_bom_is_stripped_and_restored() {
        let bytes = b"\xEF\xBB\xBFcaf\xC3\xA9";
        let (enc, text) = decode(bytes);
        assert_eq!(enc, TextEncoding::Utf8Bom);
        assert_eq!(text, "café");
        assert_eq!(to_bytes(enc, &text), bytes);
    }

    #[test]
    fn utf16_le_round_trips() {
        let (enc, text) = decode(&encode_checked(TextEncoding::Utf16Le, "héllo").0);
        assert_eq!(enc, TextEncoding::Utf16Le);
        assert_eq!(text, "héllo");
        assert_eq!(
            to_bytes(enc, &text),
            to_bytes(TextEncoding::Utf16Le, "héllo")
        );
    }

    #[test]
    fn utf16_be_round_trips() {
        let (enc, text) = decode(&to_bytes(TextEncoding::Utf16Be, "héllo"));
        assert_eq!(enc, TextEncoding::Utf16Be);
        assert_eq!(text, "héllo");
    }

    #[test]
    fn bomless_utf16_le_is_not_read_as_utf8() {
        // 'A' = 41 00, 'B' = 42 00: valid UTF-8 with NULs, but really UTF-16.
        let (enc, text) = decode(&[0x41, 0x00, 0x42, 0x00]);
        assert_eq!(enc, TextEncoding::Utf16Le);
        assert_eq!(text, "AB");
    }

    #[test]
    fn cp1252_is_decoded_not_replaced() {
        // "caf\xE9" is PowerShell 5.1's ANSI output; é must survive.
        let (enc, text) = decode(b"caf\xE9");
        assert_eq!(enc, TextEncoding::Windows1252);
        assert_eq!(text, "café");
    }

    #[test]
    fn cp1252_decodes_the_high_table() {
        // 0x80..=0x9F are the bytes that differ from Latin-1.
        let (enc, text) = decode(&[0x80, 0x97, 0x93]);
        assert_eq!(enc, TextEncoding::Windows1252);
        assert_eq!(text, "€—\u{201C}");
    }

    #[test]
    fn cp1252_round_trips_every_byte() {
        // Every byte must survive decode→encode, including the undefined slots.
        let bytes: Vec<u8> = (0u8..=255).collect();
        let (enc, text) = decode(&bytes);
        assert_eq!(enc, TextEncoding::Windows1252);
        assert_eq!(to_bytes(enc, &text), bytes);
    }

    #[test]
    fn cp1252_write_falls_back_to_utf8_for_unrepresentable_text() {
        let (bytes, fell_back) = encode_checked(TextEncoding::Windows1252, "a→b");
        assert!(fell_back);
        assert_eq!(bytes, "a→b".as_bytes());
        assert_eq!(decode(&bytes).1, "a→b");
    }

    #[test]
    fn repair_undoes_the_em_dash_double_encoding() {
        // U+2014 is E2 80 94; read as cp1252 that is "â€"" and re-encoded as
        // UTF-8 it is â€" — the exact damage Set-Content -Encoding UTF8 does.
        let damaged = "\u{00E2}\u{20AC}\u{201D}";
        assert_eq!(repair_mojibake(damaged), "—");
    }

    #[test]
    fn repair_undoes_the_section_sign_double_encoding() {
        let damaged = "\u{00C2}\u{00A7}"; // Â§
        assert_eq!(repair_mojibake(damaged), "§");
    }

    #[test]
    fn repair_leaves_correct_text_alone() {
        // These are all single cp1252-encodable characters, but their bytes are
        // not valid UTF-8, so the round-trip fails and they are kept.
        assert_eq!(repair_mojibake("café — § ─"), "café — § ─");
    }

    #[test]
    fn repair_keeps_mixed_correct_and_damaged_text() {
        assert_eq!(repair_mojibake("ok Â§ fin"), "ok § fin");
    }

    #[test]
    fn repair_is_idempotent() {
        let once = repair_mojibake("caf\u{00C3}\u{00A9} Â§");
        assert_eq!(repair_mojibake(&once), once);
        assert_eq!(once, "café §");
    }

    #[test]
    fn repair_drops_a_leading_bom() {
        assert_eq!(repair_mojibake("\u{FEFF}hello"), "hello");
    }
}
