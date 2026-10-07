//! Pure text handling: decoding and line endings. No Win32.

/// Windows-1252 code points for bytes 0x80..=0x9F (undefined slots map to themselves).
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

/// Decodes file bytes: BOM-tagged UTF-8/UTF-16, then plain UTF-8, then Windows-1252.
pub fn decode(b: &[u8]) -> String {
    if let Some(r) = b.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(r).into_owned();
    }
    if let Some(r) = b.strip_prefix(b"\xFF\xFE") {
        return utf16(r, u16::from_le_bytes);
    }
    if let Some(r) = b.strip_prefix(b"\xFE\xFF") {
        return utf16(r, u16::from_be_bytes);
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => b
            .iter()
            .map(|&c| match c {
                0x80..=0x9F => char::from_u32(CP1252_HIGH[(c - 0x80) as usize] as u32).unwrap(),
                _ => c as char,
            })
            .collect(),
    }
}

fn utf16(b: &[u8], from_bytes: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| from_bytes(c))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Converts `\r\n` and lone `\r` to `\n`, the internal line ending.
pub fn to_lf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\r') {
        out.push_str(&rest[..i]);
        out.push('\n');
        rest = &rest[i + 1..];
        rest = rest.strip_prefix('\n').unwrap_or(rest);
    }
    out.push_str(rest);
    out
}

/// Converts `\n`-only text to the given line ending.
pub fn with_eol(s: &str, eol: &str) -> String {
    if eol == "\n" {
        s.to_owned()
    } else {
        s.replace('\n', eol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn decode_plain_utf8() {
        assert_eq!(decode("héllo ✓".as_bytes()), "héllo ✓");
    }

    #[test]
    fn decode_utf8_bom() {
        assert_eq!(decode(b"\xEF\xBB\xBFabc"), "abc");
    }

    #[test]
    fn decode_utf16_le_bom() {
        let mut b = vec![0xFF, 0xFE];
        b.extend(u("hé✓").iter().flat_map(|c| c.to_le_bytes()));
        assert_eq!(decode(&b), "hé✓");
    }

    #[test]
    fn decode_utf16_be_bom() {
        let mut b = vec![0xFE, 0xFF];
        b.extend(u("hé✓").iter().flat_map(|c| c.to_be_bytes()));
        assert_eq!(decode(&b), "hé✓");
    }

    #[test]
    fn decode_invalid_utf8_falls_back_to_cp1252() {
        assert_eq!(decode(b"caf\xE9 \x80 \x93x\x94"), "café € “x”");
    }

    #[test]
    fn decode_empty() {
        assert_eq!(decode(b""), "");
        assert_eq!(decode(b"\xFF\xFE"), "");
    }

    #[test]
    fn utf8_round_trip() {
        let s = "line1\r\nline2 ✓ 🎉\r\n";
        assert_eq!(decode(s.as_bytes()), s);
    }

    #[test]
    fn lf_normalization() {
        assert_eq!(to_lf("a\nb\rc\r\nd"), "a\nb\nc\nd");
        assert_eq!(to_lf("\r\r\n\n"), "\n\n\n");
        assert_eq!(to_lf("trailing\r"), "trailing\n");
        assert_eq!(to_lf("é\r\n✓"), "é\n✓");
        assert_eq!(to_lf(""), "");
    }

    #[test]
    fn lf_normalization_idempotent() {
        let once = to_lf("x\ny\r\rz");
        assert_eq!(to_lf(&once), once);
    }

    #[test]
    fn eol_conversion() {
        assert_eq!(with_eol("a\nb\n", "\r\n"), "a\r\nb\r\n");
        assert_eq!(with_eol("a\nb", "\n"), "a\nb");
        assert_eq!(to_lf(&with_eol("x\ny", "\r\n")), "x\ny");
    }
}
