//! Pure text handling: decoding, line endings, search. No Win32.

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

/// Converts `\n`, `\r`, and `\r\n` to `\r\n`, which the EDIT control requires.
pub fn normalize_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 32);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            _ => out.push(c),
        }
    }
    out
}

fn fold(c: u16) -> u16 {
    let Some(ch) = char::from_u32(c as u32) else {
        return c;
    };
    let mut lower = ch.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) if (l as u32) < 0x10000 => l as u32 as u16,
        _ => c,
    }
}

/// Finds `needle` in UTF-16 `hay`. Down: first match starting at or after `from`.
/// Up: last match ending at or before `from`.
pub fn find(
    hay: &[u16],
    needle: &[u16],
    from: usize,
    match_case: bool,
    down: bool,
) -> Option<usize> {
    let n = needle.len();
    if n == 0 || n > hay.len() {
        return None;
    }
    let f = |c: u16| if match_case { c } else { fold(c) };
    let is_match = |i: usize| {
        hay[i..i + n]
            .iter()
            .zip(needle)
            .all(|(&a, &b)| f(a) == f(b))
    };
    let last = hay.len() - n;
    if down {
        (from..=last).find(|&i| is_match(i))
    } else if from < n {
        None
    } else {
        (0..=(from - n).min(last)).rev().find(|&i| is_match(i))
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
    fn crlf_normalization() {
        assert_eq!(normalize_crlf("a\nb\rc\r\nd"), "a\r\nb\r\nc\r\nd");
        assert_eq!(normalize_crlf("\r\r\n\n"), "\r\n\r\n\r\n");
        assert_eq!(normalize_crlf(""), "");
    }

    #[test]
    fn crlf_normalization_idempotent() {
        let once = normalize_crlf("x\ny\r\rz");
        assert_eq!(normalize_crlf(&once), once);
    }

    #[test]
    fn find_down_and_case() {
        let h = u("Foo foo FOO");
        assert_eq!(find(&h, &u("foo"), 0, true, true), Some(4));
        assert_eq!(find(&h, &u("foo"), 0, false, true), Some(0));
        assert_eq!(find(&h, &u("foo"), 1, false, true), Some(4));
        assert_eq!(find(&h, &u("foo"), 5, false, true), Some(8));
        assert_eq!(find(&h, &u("foo"), 9, false, true), None);
    }

    #[test]
    fn find_up() {
        let h = u("Foo foo FOO");
        assert_eq!(find(&h, &u("foo"), 11, false, false), Some(8));
        assert_eq!(find(&h, &u("foo"), 8, false, false), Some(4));
        assert_eq!(find(&h, &u("foo"), 11, true, false), Some(4));
        assert_eq!(find(&h, &u("foo"), 2, false, false), None);
    }

    #[test]
    fn find_edges_and_misses() {
        let h = u("abc");
        assert_eq!(find(&h, &u("abc"), 0, true, true), Some(0));
        assert_eq!(find(&h, &u("abc"), 3, true, false), Some(0));
        assert_eq!(find(&h, &u("abcd"), 0, true, true), None);
        assert_eq!(find(&h, &u(""), 0, true, true), None);
        assert_eq!(find(&h, &u("z"), 0, true, true), None);
        assert_eq!(find(&h, &u("c"), 99, true, true), None);
    }

    #[test]
    fn find_non_ascii() {
        let h = u("Ünïcode 🎉 ünïcode");
        assert_eq!(find(&h, &u("ÜNÏ"), 1, false, true), Some(11));
        assert_eq!(find(&h, &u("🎉"), 0, true, true), Some(8));
    }
}
