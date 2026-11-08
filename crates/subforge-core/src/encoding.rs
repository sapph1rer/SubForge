//! Text encoding detection & conversion (UTF-8/16 BOMs, TIS-620/Windows-874, Shift_JIS, ...).

use encoding_rs::Encoding;

#[derive(Debug, Clone)]
pub struct Decoded {
    pub text: String,
    /// Detected encoding label, e.g. "UTF-8", "windows-874", "Shift_JIS".
    pub encoding: String,
    pub had_bom: bool,
}

/// Decode raw bytes. Order: BOM -> valid UTF-8 -> forced label -> chardetng guess.
pub fn decode(bytes: &[u8], force: Option<&str>) -> Decoded {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        let (cow, _) = enc.decode_without_bom_handling(&bytes[bom_len..]);
        return Decoded { text: cow.into_owned(), encoding: enc.name().to_string(), had_bom: true };
    }
    if let Some(label) = force {
        if let Some(enc) = lookup(label) {
            let (cow, _, _) = enc.decode(bytes);
            return Decoded { text: cow.into_owned(), encoding: enc.name().to_string(), had_bom: false };
        }
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Decoded { text: s.to_string(), encoding: "UTF-8".into(), had_bom: false };
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    det.feed(bytes, true);
    let enc = det.guess(None, chardetng::Utf8Detection::Allow);
    let (cow, _, _) = enc.decode(bytes);
    Decoded { text: cow.into_owned(), encoding: enc.name().to_string(), had_bom: false }
}

/// Look up an encoding by label. Extra aliases: "utf8", "utf-8-bom", "thai", "tis-620", "sjis".
pub fn lookup(label: &str) -> Option<&'static Encoding> {
    let l = label.trim().to_ascii_lowercase();
    let l = match l.as_str() {
        "utf8" | "utf-8-bom" | "utf8-bom" | "utf-8-sig" => "utf-8",
        "thai" | "tis620" | "tis-620" | "cp874" => "windows-874",
        "sjis" | "shiftjis" | "cp932" => "shift_jis",
        "utf16" | "utf-16" => "utf-16le",
        other => other,
    };
    Encoding::for_label(l.as_bytes())
}

/// Encode text for writing. `label` like "utf-8", "utf-8-bom", "utf-16le", "windows-874".
/// Returns an error string if the encoding is unknown or characters cannot be represented.
pub fn encode(text: &str, label: &str) -> Result<Vec<u8>, String> {
    let l = label.trim().to_ascii_lowercase();
    let bom = matches!(l.as_str(), "utf-8-bom" | "utf8-bom" | "utf-8-sig");
    match l.as_str() {
        "utf-8" | "utf8" | "utf-8-bom" | "utf8-bom" | "utf-8-sig" => {
            let mut v = Vec::with_capacity(text.len() + 3);
            if bom {
                v.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            v.extend_from_slice(text.as_bytes());
            Ok(v)
        }
        "utf-16le" | "utf-16" | "utf16" => {
            let mut v = vec![0xFF, 0xFE];
            for u in text.encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            Ok(v)
        }
        "utf-16be" => {
            let mut v = vec![0xFE, 0xFF];
            for u in text.encode_utf16() {
                v.extend_from_slice(&u.to_be_bytes());
            }
            Ok(v)
        }
        _ => {
            let enc = lookup(&l).ok_or_else(|| format!("unknown encoding: {label}"))?;
            let (cow, _, had_errors) = enc.encode(text);
            if had_errors {
                return Err(format!("text contains characters not representable in {}", enc.name()));
            }
            Ok(cow.into_owned())
        }
    }
}
