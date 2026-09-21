//! R-OAI-IMG-GUARD - validate (and repair) images before they reach the provider.
//!
//! ## Why this exists
//!
//! Anthropic (via Bedrock Converse behind litellm) answers a malformed image
//! with a bare `Could not process image` and a 500. Two consequences:
//!
//! 1. The turn fails with no indication of *which* image or *what* is wrong.
//! 2. The image stays in the transcript, so it is re-sent on every following
//!    turn - the session is poisoned and never recovers. Server logs caught this
//!    as three consecutive identical failures before the session was abandoned.
//!
//! An unusable image must therefore never be forwarded verbatim. It is either
//! repaired, or replaced by a text note explaining the problem. A turn that
//! reports "this image was unreadable" is strictly better than a turn that dies.
//!
//! ## What is actually broken in practice
//!
//! Measured on a real drawio export that failed reproducibly: the file was a
//! PNG truncated by 8 bytes - the trailing `IEND` chunk was missing. GDI+ and
//! most viewers decode it happily (which is why it looked fine locally), but
//! strict decoders and Bedrock reject it. Re-appending `IEND` makes it decode.
//!
//! ## What this deliberately does *not* do
//!
//! No resizing. It was the first hypothesis and it was wrong: a *valid* re-encode
//! of that same 2349x1574 image is accepted, and so are valid PNGs at 2600, 3000,
//! 4000 and 5000px (up to 1.4 MB). The earlier "limit near 1800px" reading was an
//! artifact - each test resize silently repaired the file, so the comparison was
//! broken-vs-repaired, not small-vs-large. Dimensions were never the cause.

use base64::Engine;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock};

/// A complete, zero-length `IEND` chunk: length, type, and its constant CRC32.
/// PNG's terminator carries no payload, so these 12 bytes are always identical.
const IEND_CHUNK: [u8; 12] = [
    0x00, 0x00, 0x00, 0x00, // length = 0
    b'I', b'E', b'N', b'D', // type
    0xAE, 0x42, 0x60, 0x82, // CRC32("IEND")
];

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Cap on cached verdicts. Images are large; a session rarely holds many. Past
/// this the cache is cleared wholesale - LRU bookkeeping would cost more than
/// the hit rate justifies.
const MAX_CACHE_ENTRIES: usize = 32;

/// What should go on the wire for a given image.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Decodes cleanly - forward the original URI untouched.
    Pass,
    /// Was malformed but repaired; forward this data URI instead.
    Repaired(String),
    /// Unusable and unrepairable. Do NOT forward it: send this explanation as
    /// text so the turn completes and the model can say something useful.
    Unusable(String),
}

/// Verdicts are cached per URI: the provider is called once per turn and images
/// live on in the transcript, so the same image is re-encoded every turn for the
/// rest of the session (one blob was seen re-encoded 724 times). Validation
/// requires a full decode, which is exactly what must not happen 724 times.
fn cache() -> &'static Mutex<HashMap<u64, Verdict>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Verdict>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Decide what to send for one image URI.
///
/// Anything not verifiable here is passed through rather than blocked: remote
/// `https://` URLs (no bytes to inspect), non-base64 data URIs, and SVG (vector
/// XML, which has no raster form and which `image` cannot decode). Being unable
/// to check is not evidence of breakage.
pub fn check(url: &str) -> Verdict {
    let Some((mime, b64)) = parse_base64_data_uri(url) else {
        return Verdict::Pass;
    };
    if mime.contains("svg") {
        return Verdict::Pass;
    }

    let key = hash_of(url);
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }

    let verdict = classify(mime, b64);

    if let Ok(mut c) = cache().lock() {
        if c.len() >= MAX_CACHE_ENTRIES {
            c.clear();
        }
        c.insert(key, verdict.clone());
    }
    verdict
}

fn classify(mime: &str, b64: &str) -> Verdict {
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return Verdict::Unusable("[image dropped: payload is not valid base64]".to_string());
    };

    if decodes(&bytes) {
        return Verdict::Pass;
    }

    // Only PNG has a repair worth attempting, and only one failure mode has been
    // observed in the wild: a missing or truncated IEND terminator.
    if let Some(fixed) = repair_png(&bytes) {
        if decodes(&fixed) {
            return Verdict::Repaired(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&fixed)
            ));
        }
    }

    // Report size and type: enough for the user to identify the file, without
    // pretending to a precise diagnosis the decoder did not give us.
    Verdict::Unusable(format!(
        "[image dropped: {} data is corrupt and could not be decoded ({} bytes). \
         The file itself is likely truncated or incomplete.]",
        if mime.is_empty() { "image" } else { mime },
        bytes.len()
    ))
}

fn decodes(bytes: &[u8]) -> bool {
    image::load_from_memory(bytes).is_ok()
}

/// Rebuild a PNG whose chunk stream does not end in a well-formed `IEND`.
///
/// Walks the chunk list, remembers the end of the last *complete* chunk, then
/// truncates any trailing debris and appends a proper `IEND`. Returns `None` if
/// this is not a PNG or if nothing can be salvaged.
///
/// A chunk that overruns the buffer is dropped rather than kept: its declared
/// length cannot be trusted, and a partial chunk is what broke decoding in the
/// first place.
fn repair_png(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < PNG_MAGIC.len() || bytes[..8] != PNG_MAGIC {
        return None;
    }

    let mut pos = 8usize;
    let mut last_complete_end = 8usize;
    let mut saw_data = false;

    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        // length + type + payload + CRC
        let Some(end) = pos.checked_add(12).and_then(|p| p.checked_add(len)) else {
            break;
        };
        if end > bytes.len() {
            break; // truncated chunk - discard it
        }
        if kind == b"IDAT" {
            saw_data = true;
        }
        last_complete_end = end;
        if kind == b"IEND" {
            // A valid terminator already exists; the fault lies elsewhere.
            return None;
        }
        pos = end;
    }

    // Without IHDR and at least one IDAT there is no image to rescue.
    if !saw_data {
        return None;
    }

    let mut out = Vec::with_capacity(last_complete_end + IEND_CHUNK.len());
    out.extend_from_slice(&bytes[..last_complete_end]);
    out.extend_from_slice(&IEND_CHUNK);
    Some(out)
}

/// Split `data:<mime>;base64,<payload>`. `None` for non-base64 data URIs.
fn parse_base64_data_uri(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("data:")?;
    let (header, payload) = rest.split_once(',')?;
    let mime = header.strip_suffix(";base64")?;
    Some((mime, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 40, 40]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encode test png");
        bytes
    }

    fn as_uri(bytes: &[u8]) -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn valid_image_passes_untouched() {
        assert_eq!(check(&as_uri(&png_bytes(400, 300))), Verdict::Pass);
    }

    /// Large valid images must NOT be altered: the provider accepts them, as
    /// verified against the live gateway up to 5000px.
    #[test]
    fn large_valid_image_is_not_rejected_or_rewritten() {
        assert_eq!(check(&as_uri(&png_bytes(2349, 1574))), Verdict::Pass);
    }

    /// The real-world failure: trailing IEND chunk missing entirely.
    #[test]
    fn png_missing_iend_is_repaired() {
        let good = png_bytes(300, 200);
        let truncated = &good[..good.len() - IEND_CHUNK.len()];
        assert!(!decodes(truncated), "precondition: must be undecodable");

        match check(&as_uri(truncated)) {
            Verdict::Repaired(uri) => {
                let (_, b64) = parse_base64_data_uri(&uri).expect("data uri");
                let fixed = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .expect("base64");
                let img = image::load_from_memory(&fixed).expect("repaired must decode");
                assert_eq!((img.width(), img.height()), (300, 200));
            }
            other => panic!("expected Repaired, got {other:?}"),
        }
    }

    /// Exactly the observed corruption: IEND's length field present, rest gone.
    #[test]
    fn png_with_dangling_length_field_is_repaired() {
        let good = png_bytes(300, 200);
        let mut broken = good[..good.len() - IEND_CHUNK.len()].to_vec();
        broken.extend_from_slice(&[0, 0, 0, 0]);
        assert!(!decodes(&broken), "precondition: must be undecodable");
        assert!(matches!(check(&as_uri(&broken)), Verdict::Repaired(_)));
    }

    /// Poisoning guard: unrepairable data must yield text, never the image.
    #[test]
    fn irreparable_data_becomes_a_text_note() {
        let uri = as_uri(b"this is definitely not a png");
        match check(&uri) {
            Verdict::Unusable(note) => {
                assert!(note.contains("corrupt"), "note should explain: {note}");
            }
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn invalid_base64_is_reported_not_forwarded() {
        let uri = "data:image/png;base64,!!!not base64!!!";
        assert!(matches!(check(uri), Verdict::Unusable(_)));
    }

    /// A PNG header with no image data cannot be rescued by adding IEND.
    #[test]
    fn header_only_png_is_not_falsely_repaired() {
        assert!(repair_png(&PNG_MAGIC).is_none());
    }

    #[test]
    fn repair_declines_when_iend_already_present() {
        assert!(repair_png(&png_bytes(50, 50)).is_none());
    }

    #[test]
    fn svg_passes_through_undecoded() {
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode("<svg/>")
        );
        assert_eq!(check(&uri), Verdict::Pass);
    }

    #[test]
    fn remote_url_passes_through() {
        assert_eq!(check("https://example.com/x.png"), Verdict::Pass);
    }

    #[test]
    fn verdict_is_cached_across_calls() {
        let good = png_bytes(320, 240);
        let truncated = as_uri(&good[..good.len() - IEND_CHUNK.len()]);
        let first = check(&truncated);
        assert_eq!(first, check(&truncated), "cache must be stable");
    }
}
