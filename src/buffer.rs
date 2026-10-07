//! Large-file text storage: a chunked rope of UTF-8 bytes with a line index.
//!
//! Offsets are UTF-8 byte offsets and must fall on char boundaries. A line break is
//! `\n`; bytes are stored exactly as read, so `\r\n` files stay `\r\n`.

use crate::text;
use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::Path;

/// Target chunk size; chunks split when they exceed twice this.
pub const CHUNK: usize = 64 * 1024;

struct Chunk {
    text: Vec<u8>,
    newlines: u32,
}

impl Chunk {
    fn new(text: Vec<u8>) -> Self {
        let newlines = count_nl(&text) as u32;
        Chunk { text, newlines }
    }
}

fn count_nl(b: &[u8]) -> usize {
    b.iter().filter(|&&x| x == b'\n').count()
}

fn nth_nl(b: &[u8], n: usize) -> usize {
    memchr::memchr_iter(b'\n', b)
        .nth(n)
        .expect("newline index out of range")
}

fn is_char_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

/// Length of the longest prefix of `b` that doesn't end inside a UTF-8 sequence.
fn complete_prefix_len(b: &[u8]) -> usize {
    let n = b.len();
    for back in 1..=n.min(4) {
        let lead = b[n - back];
        if is_char_start(lead) {
            let need = match lead {
                0xF0.. => 4,
                0xE0.. => 3,
                0xC0.. => 2,
                _ => 1,
            };
            return if back >= need { n } else { n - back };
        }
    }
    n
}

/// Splits UTF-8 bytes into chunks of about `size`, never inside a char.
fn split(mut b: &[u8], size: usize) -> Vec<Chunk> {
    let mut out = Vec::with_capacity(b.len() / size + 1);
    while b.len() > size {
        let mut cut = size;
        while !is_char_start(b[cut]) {
            cut -= 1;
        }
        out.push(Chunk::new(b[..cut].to_vec()));
        b = &b[cut..];
    }
    out.push(Chunk::new(b.to_vec()));
    out
}

/// Fenwick (binary indexed) tree of per-chunk sums.
struct Fenwick {
    tree: Vec<u64>,
}

impl Fenwick {
    fn new(values: impl Iterator<Item = u64>) -> Self {
        let mut tree = vec![0];
        tree.extend(values);
        let n = tree.len() - 1;
        for i in 1..=n {
            let j = i + i.isolate_lowest_one();
            if j <= n {
                tree[j] += tree[i];
            }
        }
        Fenwick { tree }
    }

    fn add(&mut self, index: usize, delta: i64) {
        let mut i = index + 1;
        while i < self.tree.len() {
            self.tree[i] = self.tree[i].wrapping_add_signed(delta);
            i += i.isolate_lowest_one();
        }
    }

    /// Sum of the first `count` values.
    fn prefix(&self, count: usize) -> u64 {
        let mut i = count;
        let mut sum = 0;
        while i > 0 {
            sum += self.tree[i];
            i -= i.isolate_lowest_one();
        }
        sum
    }

    fn total(&self) -> u64 {
        self.prefix(self.tree.len() - 1)
    }

    /// Index of the value containing cumulative position `k`, and the sum before it.
    fn find(&self, k: u64) -> (usize, u64) {
        let n = self.tree.len() - 1;
        let mut pos = 0;
        let mut rem = k;
        let mut step = if n == 0 { 0 } else { 1 << n.ilog2() };
        while step > 0 {
            if pos + step <= n && self.tree[pos + step] <= rem {
                pos += step;
                rem -= self.tree[pos];
            }
            step >>= 1;
        }
        (pos, k - rem)
    }
}

pub struct Buffer {
    chunks: Vec<Chunk>,
    bytes: Fenwick,
    lines: Fenwick,
    chunk_size: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(s: &str) -> Self {
        Self::with_chunk_size(s, CHUNK)
    }

    fn with_chunk_size(s: &str, chunk_size: usize) -> Self {
        Self::from_chunks(split(s.as_bytes(), chunk_size), chunk_size)
    }

    fn from_chunks(mut chunks: Vec<Chunk>, chunk_size: usize) -> Self {
        if chunks.is_empty() {
            chunks.push(Chunk::new(Vec::new()));
        }
        let mut b = Buffer {
            chunks,
            bytes: Fenwick::new(std::iter::empty()),
            lines: Fenwick::new(std::iter::empty()),
            chunk_size,
        };
        b.reindex();
        b
    }

    fn reindex(&mut self) {
        if self.chunks.len() > 1 {
            self.chunks.retain(|c| !c.text.is_empty());
        }
        if self.chunks.is_empty() {
            self.chunks.push(Chunk::new(Vec::new()));
        }
        self.bytes = Fenwick::new(self.chunks.iter().map(|c| c.text.len() as u64));
        self.lines = Fenwick::new(self.chunks.iter().map(|c| c.newlines as u64));
    }

    /// Reads a file: BOM-tagged UTF-16 is transcoded, UTF-8 (with or without BOM) is
    /// streamed into chunks, anything else falls back to Windows-1252.
    pub fn open(path: &Path) -> io::Result<Buffer> {
        let mut f = File::open(path)?;
        let mut head = Vec::with_capacity(3);
        Read::by_ref(&mut f).take(3).read_to_end(&mut head)?;
        if head.starts_with(b"\xFF\xFE") || head.starts_with(b"\xFE\xFF") {
            f.read_to_end(&mut head)?;
            return Ok(Self::from_text(&text::decode(&head)));
        }
        let mut carry = if head == b"\xEF\xBB\xBF" {
            Vec::new()
        } else {
            head
        };
        let mut chunks = Vec::new();
        loop {
            let mut buf = Vec::with_capacity(CHUNK + 4);
            buf.append(&mut carry);
            let want = CHUNK.saturating_sub(buf.len()) as u64;
            let got = Read::by_ref(&mut f).take(want).read_to_end(&mut buf)? as u64;
            let eof = got < want;
            if buf.is_empty() {
                break;
            }
            let cut = if eof {
                buf.len()
            } else {
                complete_prefix_len(&buf)
            };
            carry.extend_from_slice(&buf[cut..]);
            buf.truncate(cut);
            if std::str::from_utf8(&buf).is_err() {
                return Ok(Self::from_text(&text::decode(&std::fs::read(path)?)));
            }
            chunks.push(Chunk {
                text: buf,
                newlines: 0,
            });
            if eof {
                break;
            }
        }
        count_newlines_parallel(&mut chunks);
        Ok(Self::from_chunks(chunks, CHUNK))
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        for c in &self.chunks {
            w.write_all(&c.text)?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.bytes.total() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn line_count(&self) -> usize {
        self.lines.total() as usize + 1
    }

    /// Chunk index and offset for `pos`; `pos == len` maps to the end of the last chunk.
    fn locate(&self, pos: usize) -> (usize, usize) {
        let len = self.len();
        assert!(pos <= len, "offset {pos} past end {len}");
        if pos == len {
            let c = self.chunks.len() - 1;
            return (c, self.chunks[c].text.len());
        }
        let (c, before) = self.bytes.find(pos as u64);
        (c, pos - before as usize)
    }

    pub fn byte_at(&self, pos: usize) -> u8 {
        let (c, off) = self.locate(pos);
        self.chunks[c].text[off]
    }

    pub fn is_char_boundary(&self, pos: usize) -> bool {
        pos == 0 || pos == self.len() || is_char_start(self.byte_at(pos))
    }

    /// Byte offset where `line` starts.
    pub fn line_start(&self, line: usize) -> usize {
        assert!(line < self.line_count(), "line {line} out of range");
        if line == 0 {
            return 0;
        }
        let k = (line - 1) as u64;
        let (c, before) = self.lines.find(k);
        let off = nth_nl(&self.chunks[c].text, (k - before) as usize);
        self.bytes.prefix(c) as usize + off + 1
    }

    /// Line containing byte offset `pos`.
    pub fn line_of(&self, pos: usize) -> usize {
        let (c, off) = self.locate(pos);
        self.lines.prefix(c) as usize + count_nl(&self.chunks[c].text[..off])
    }

    /// Offset of the first `byte` at or after `from`.
    pub fn find_byte(&self, from: usize, byte: u8) -> Option<usize> {
        if from >= self.len() {
            return None;
        }
        let (mut c, mut off) = self.locate(from);
        let mut base = from - off;
        loop {
            let t = &self.chunks[c].text;
            if let Some(p) = memchr::memchr(byte, &t[off..]) {
                return Some(base + off + p);
            }
            base += t.len();
            c += 1;
            off = 0;
            if c == self.chunks.len() {
                return None;
            }
        }
    }

    /// Content of `line`, excluding its `\n` and a `\r` before it.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.line_start(line);
        let mut end = if line + 1 < self.line_count() {
            self.line_start(line + 1) - 1
        } else {
            self.len()
        };
        if end > start && self.byte_at(end - 1) == b'\r' {
            end -= 1;
        }
        start..end
    }

    /// Calls `f` with each stored byte slice overlapping `range`, in order.
    pub fn for_each_slice(&self, range: Range<usize>, mut f: impl FnMut(&[u8])) {
        if range.is_empty() {
            return;
        }
        let (mut c, mut off) = self.locate(range.start);
        let mut left = range.len();
        while left > 0 {
            let t = &self.chunks[c].text[off..];
            let take = t.len().min(left);
            f(&t[..take]);
            left -= take;
            c += 1;
            off = 0;
        }
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        let mut out = Vec::with_capacity(range.len());
        self.for_each_slice(range, |b| out.extend_from_slice(b));
        String::from_utf8(out).expect("slice not on char boundaries")
    }

    /// Chars from `pos` to the end.
    pub fn chars_from(&self, pos: usize) -> impl Iterator<Item = char> + '_ {
        let (c, off) = self.locate(pos);
        self.chunks[c..]
            .iter()
            .enumerate()
            .flat_map(move |(i, ch)| {
                let start = if i == 0 { off } else { 0 };
                // Chunks only ever split between chars and are validated on load.
                std::str::from_utf8(&ch.text[start..])
                    .expect("chunk holds valid UTF-8")
                    .chars()
            })
    }

    pub fn insert(&mut self, pos: usize, s: &str) {
        if s.is_empty() {
            return;
        }
        debug_assert!(self.is_char_boundary(pos));
        let (c, off) = self.locate(pos);
        let chunk = &mut self.chunks[c];
        if chunk.text.len() + s.len() <= 2 * self.chunk_size {
            chunk.text.splice(off..off, s.bytes());
            let added = count_nl(s.as_bytes());
            chunk.newlines += added as u32;
            self.bytes.add(c, s.len() as i64);
            self.lines.add(c, added as i64);
        } else {
            let mut joined = Vec::with_capacity(chunk.text.len() + s.len());
            joined.extend_from_slice(&chunk.text[..off]);
            joined.extend_from_slice(s.as_bytes());
            joined.extend_from_slice(&chunk.text[off..]);
            self.chunks.splice(c..=c, split(&joined, self.chunk_size));
            self.reindex();
        }
    }

    pub fn delete(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        debug_assert!(self.is_char_boundary(range.start) && self.is_char_boundary(range.end));
        let (c0, o0) = self.locate(range.start);
        let (c1, o1) = {
            let (c, o) = self.locate(range.end - 1);
            (c, o + 1)
        };
        if c0 == c1 {
            let chunk = &mut self.chunks[c0];
            let removed = count_nl(&chunk.text[o0..o1]);
            chunk.text.drain(o0..o1);
            chunk.newlines -= removed as u32;
            if chunk.text.is_empty() && self.chunks.len() > 1 {
                self.reindex();
            } else {
                self.bytes.add(c0, -(range.len() as i64));
                self.lines.add(c0, -(removed as i64));
            }
        } else {
            self.chunks[c0].text.truncate(o0);
            self.chunks[c1].text.drain(..o1);
            for c in [c0, c1] {
                let ch = &mut self.chunks[c];
                ch.newlines = count_nl(&ch.text) as u32;
            }
            self.chunks.drain(c0 + 1..c1);
            self.reindex();
        }
    }

    /// The line ending of the first line break, or `None` if there is none.
    pub fn detect_eol(&self) -> Option<&'static str> {
        if self.line_count() < 2 {
            return None;
        }
        let nl = self.line_start(1) - 1;
        Some(if nl > 0 && self.byte_at(nl - 1) == b'\r' {
            "\r\n"
        } else {
            "\n"
        })
    }

    /// Finds `needle` searching forward from `from` (match starts at or after it) or
    /// backward (match ends at or before it). Returns the matched byte range, which can
    /// differ in length from `needle` when case-folding. Large searches use all cores;
    /// the region nearest `from` is searched first so nearby matches return quickly.
    pub fn find(
        &self,
        needle: &str,
        from: usize,
        forward: bool,
        match_case: bool,
    ) -> Option<Range<usize>> {
        const NEAR: usize = 16 << 20;
        if needle.is_empty() {
            return None;
        }
        let (lo, hi) = if forward {
            (from, self.len())
        } else {
            (0, from)
        };
        let near = if forward {
            lo..hi.min(lo + NEAR)
        } else {
            lo.max(hi.saturating_sub(NEAR))..hi
        };
        let end_limit = if forward { self.len() } else { from };
        if let Some(m) = self.find_in(needle, near.clone(), end_limit, forward, match_case) {
            return Some(m);
        }
        let rest = if forward {
            near.end..hi
        } else {
            lo..near.start
        };
        if rest.is_empty() {
            return None;
        }
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let step = rest.len().div_ceil(threads).max(NEAR);
        let mut parts: Vec<Range<usize>> = (rest.start..rest.end)
            .step_by(step)
            .map(|s| s..(s + step).min(rest.end))
            .collect();
        if !forward {
            parts.reverse();
        }
        // Results come back in search order; the first hit wins.
        std::thread::scope(|s| {
            let handles: Vec<_> = parts
                .into_iter()
                .map(|r| s.spawn(move || self.find_in(needle, r, end_limit, forward, match_case)))
                .collect();
            handles
                .into_iter()
                .find_map(|h| h.join().expect("search thread"))
        })
    }

    /// First (forward) or last (backward) match whose start lies in `starts` and whose
    /// end is at most `end_limit`. Splitting `starts` never loses a match, which is what
    /// lets the search run in parallel.
    fn find_in(
        &self,
        needle: &str,
        starts: Range<usize>,
        end_limit: usize,
        forward: bool,
        match_case: bool,
    ) -> Option<Range<usize>> {
        let (lo, hi) = (starts.start, starts.end);
        if lo >= hi {
            return None;
        }
        if match_case {
            return self.find_exact(needle, lo, hi, end_limit, forward);
        }
        let first = needle.chars().next()?;
        // Every case variant of the needle starts with one of these char-start bytes.
        let mut leads = vec![lead_byte(first)];
        leads.extend(first.to_lowercase().next().map(lead_byte));
        leads.extend(first.to_uppercase().next().map(lead_byte));
        leads.sort_unstable();
        leads.dedup();
        if forward {
            let (mut c, mut off) = self.locate(lo);
            let mut base = lo - off;
            loop {
                let t = &self.chunks[c].text;
                let end = t.len().min(hi - base);
                let mut i = off;
                while let Some(p) = next_lead(&t[i..end], &leads) {
                    if let Some(e) = self.match_folded(c, i + p, base, needle) {
                        return Some(base + i + p..e);
                    }
                    i += p + 1;
                }
                base += t.len();
                c += 1;
                off = 0;
                if c == self.chunks.len() || base >= hi {
                    return None;
                }
            }
        } else {
            let (mut c, last) = self.locate(hi - 1);
            let mut limit = last + 1;
            let mut base = hi - limit;
            loop {
                let t = &self.chunks[c].text;
                let floor = lo.saturating_sub(base).min(limit);
                let mut i = limit;
                while let Some(p) = prev_lead(&t[floor..i], &leads).map(|p| floor + p) {
                    match self.match_folded(c, p, base, needle) {
                        Some(e) if e <= end_limit => return Some(base + p..e),
                        _ => i = p,
                    }
                }
                if c == 0 || base <= lo {
                    return None;
                }
                c -= 1;
                limit = self.chunks[c].text.len();
                base -= limit;
            }
        }
    }

    /// Case-sensitive search: SIMD substring search inside each chunk, plus a check of
    /// the few starts where a match can straddle a chunk boundary.
    fn find_exact(
        &self,
        needle: &str,
        lo: usize,
        hi: usize,
        end_limit: usize,
        forward: bool,
    ) -> Option<Range<usize>> {
        let n = needle.len();
        let at = |start: usize| {
            self.match_at(start, needle, true)
                .filter(|&e| e <= end_limit)
                .map(|e| start..e)
        };
        if forward {
            let finder = memchr::memmem::Finder::new(needle);
            let (mut c, mut off) = self.locate(lo);
            let mut base = lo - off;
            loop {
                let t = &self.chunks[c].text;
                if let Some(p) = finder.find(&t[off..]) {
                    let s = base + off + p;
                    return (s < hi && s + n <= end_limit).then_some(s..s + n);
                }
                for s in t.len().saturating_sub(n - 1).max(off)..t.len() {
                    if base + s >= hi {
                        return None;
                    }
                    if let Some(m) = at(base + s) {
                        return Some(m);
                    }
                }
                base += t.len();
                c += 1;
                off = 0;
                if c == self.chunks.len() || base >= hi {
                    return None;
                }
            }
        } else {
            let finder = memchr::memmem::FinderRev::new(needle);
            // Walk back from the chunk holding the last allowed start.
            let (mut c, last) = self.locate(hi - 1);
            let mut limit = last + 1; // starts allowed in t[..limit]
            let mut base = hi - limit;
            loop {
                let t = &self.chunks[c].text;
                let floor = lo.saturating_sub(base).min(limit);
                // Straddling starts sit after every in-chunk match, so try them first.
                for s in (t.len().saturating_sub(n - 1).max(floor)..limit.min(t.len())).rev() {
                    if let Some(m) = at(base + s) {
                        return Some(m);
                    }
                }
                let mut end = t.len().min(limit + n - 1);
                while let Some(p) = finder.rfind(&t[floor..end]).map(|p| floor + p) {
                    if p < limit && base + p + n <= end_limit {
                        return Some(base + p..base + p + n);
                    }
                    end = p + n - 1;
                }
                if c == 0 || base <= lo {
                    return None;
                }
                c -= 1;
                limit = self.chunks[c].text.len();
                base -= limit;
            }
        }
    }

    /// Case-insensitive match at offset `i` of chunk `c` (which starts at `base`). Checks
    /// within the chunk when the needle fits, otherwise walks chars across chunks.
    fn match_folded(&self, c: usize, i: usize, base: usize, needle: &str) -> Option<usize> {
        let t = &self.chunks[c].text[i..];
        if needle.is_ascii() {
            let n = needle.as_bytes();
            if t.len() >= n.len() {
                return t[..n.len()]
                    .eq_ignore_ascii_case(n)
                    .then_some(base + i + n.len());
            }
        } else {
            // A matching span is at most 4 bytes per needle char.
            let window = &t[..t.len().min(needle.chars().count() * 4)];
            let window = &window[..complete_prefix_len(window)];
            let s = std::str::from_utf8(window).ok()?;
            let mut hay = s.chars();
            let mut len = 0;
            let mut complete = true;
            for nc in needle.chars() {
                match hay.next() {
                    Some(h) if h == nc || fold(h) == fold(nc) => len += h.len_utf8(),
                    Some(_) => return None,
                    None => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete {
                return Some(base + i + len);
            }
        }
        self.match_at(base + i, needle, false)
    }

    /// If `needle` matches at `start` (a char boundary), returns the match end.
    fn match_at(&self, start: usize, needle: &str, match_case: bool) -> Option<usize> {
        if !self.is_char_boundary(start) {
            return None;
        }
        let mut pos = start;
        let mut hay = self.chars_from(start);
        for n in needle.chars() {
            let h = hay.next()?;
            let same = if match_case {
                h == n
            } else {
                h == n || fold(h) == fold(n)
            };
            if !same {
                return None;
            }
            pos += h.len_utf8();
        }
        Some(pos)
    }
}

fn lead_byte(c: char) -> u8 {
    let mut buf = [0; 4];
    c.encode_utf8(&mut buf).as_bytes()[0]
}

fn next_lead(h: &[u8], leads: &[u8]) -> Option<usize> {
    match *leads {
        [a] => memchr::memchr(a, h),
        [a, b] => memchr::memchr2(a, b, h),
        [a, b, c] => memchr::memchr3(a, b, c, h),
        _ => unreachable!("at most three lead bytes"),
    }
}

fn prev_lead(h: &[u8], leads: &[u8]) -> Option<usize> {
    match *leads {
        [a] => memchr::memrchr(a, h),
        [a, b] => memchr::memrchr2(a, b, h),
        [a, b, c] => memchr::memrchr3(a, b, c, h),
        _ => unreachable!("at most three lead bytes"),
    }
}

fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn count_newlines_parallel(chunks: &mut [Chunk]) {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per = chunks.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for group in chunks.chunks_mut(per) {
            s.spawn(move || {
                for c in group {
                    c.newlines = count_nl(&c.text) as u32;
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents(b: &Buffer) -> String {
        b.slice(0..b.len())
    }

    /// Reference line starts for a String.
    fn model_line_starts(s: &str) -> Vec<usize> {
        std::iter::once(0)
            .chain(s.match_indices('\n').map(|(i, _)| i + 1))
            .collect()
    }

    fn check_against(b: &Buffer, s: &str) {
        assert_eq!(contents(b), s);
        assert_eq!(b.len(), s.len());
        let starts = model_line_starts(s);
        assert_eq!(b.line_count(), starts.len());
        for (line, &st) in starts.iter().enumerate() {
            assert_eq!(b.line_start(line), st, "line_start({line})");
        }
        for (pos, _) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
            let expect = starts.partition_point(|&st| st <= pos) - 1;
            assert_eq!(b.line_of(pos), expect, "line_of({pos})");
        }
    }

    /// xorshift: deterministic randomness without a dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    fn boundary_near(s: &str, mut i: usize) -> usize {
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    #[test]
    fn empty_buffer() {
        let b = Buffer::new();
        check_against(&b, "");
        assert_eq!(b.line_range(0), 0..0);
        assert_eq!(b.detect_eol(), None);
    }

    #[test]
    fn find_byte_crosses_chunks() {
        let b = Buffer::with_chunk_size("ab\ncd\nef", 2);
        assert_eq!(b.find_byte(0, b'\n'), Some(2));
        assert_eq!(b.find_byte(3, b'\n'), Some(5));
        assert_eq!(b.find_byte(6, b'\n'), None);
        assert_eq!(b.find_byte(99, b'\n'), None);
    }

    #[test]
    fn lines_and_ranges() {
        let b = Buffer::from_text("ab\r\ncd\nlast");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.slice(b.line_range(0)), "ab");
        assert_eq!(b.slice(b.line_range(1)), "cd");
        assert_eq!(b.slice(b.line_range(2)), "last");
        assert_eq!(b.detect_eol(), Some("\r\n"));
    }

    #[test]
    fn small_chunks_match_model() {
        let s = "héllo\nwörld ✓\n\n🎉 end\nx".repeat(5);
        for size in [4, 7, 16, 64] {
            check_against(&Buffer::with_chunk_size(&s, size), &s);
        }
    }

    #[test]
    fn complete_prefix_handles_split_chars() {
        let s = "a✓".as_bytes(); // 'a' + 3-byte char
        assert_eq!(complete_prefix_len(&s[..1]), 1);
        assert_eq!(complete_prefix_len(&s[..2]), 1);
        assert_eq!(complete_prefix_len(&s[..3]), 1);
        assert_eq!(complete_prefix_len(s), 4);
    }

    #[test]
    fn randomized_edits_match_string() {
        let alphabet = ["a", "b", "\n", "\r\n", "é", "✓", "🎉", "xyz", "\n\n"];
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let size = 4 + rng.below(30);
            let mut model = String::new();
            let mut b = Buffer::with_chunk_size("", size);
            for _ in 0..300 {
                if model.is_empty() || rng.below(3) > 0 {
                    let pos = boundary_near(&model, rng.below(model.len() + 1));
                    let max_len = if rng.below(10) == 0 { 200 } else { 6 };
                    let ins: String = (0..1 + rng.below(max_len))
                        .map(|_| alphabet[rng.below(alphabet.len())])
                        .collect();
                    model.insert_str(pos, &ins);
                    b.insert(pos, &ins);
                } else {
                    let a = boundary_near(&model, rng.below(model.len() + 1));
                    let z = boundary_near(&model, (a + rng.below(40)).min(model.len()));
                    model.replace_range(a..z, "");
                    b.delete(a..z);
                }
            }
            check_against(&b, &model);
        }
    }

    #[test]
    fn find_forward_backward_and_case() {
        let s = "Foo foo FOO\nfoo✓ ÉCOLE école";
        for size in [3, 5, 1024] {
            let b = Buffer::with_chunk_size(s, size);
            assert_eq!(b.find("foo", 0, true, true), Some(4..7));
            assert_eq!(b.find("foo", 0, true, false), Some(0..3));
            assert_eq!(b.find("foo", 5, true, false), Some(8..11));
            assert_eq!(b.find("foo", s.len(), false, true), Some(12..15));
            assert_eq!(b.find("foo", 11, false, false), Some(8..11));
            assert_eq!(b.find("foo", 2, false, false), None);
            let e = s.find("école").unwrap();
            assert_eq!(
                b.find("École", 0, true, false),
                Some(s.find("ÉCOLE").unwrap()..s.find("ÉCOLE").unwrap() + "ÉCOLE".len())
            );
            assert_eq!(b.find("école", 0, true, true), Some(e..e + "école".len()));
            assert_eq!(b.find("✓", 0, true, true).map(|r| r.len()), Some(3));
            assert_eq!(b.find("zzz", 0, true, false), None);
            assert_eq!(b.find("", 0, true, false), None);
        }
    }

    #[test]
    fn randomized_find_matches_model() {
        let alphabet = [
            "a", "b", "A", "B", "
", "é", "ab", "ba",
        ];
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
            let s: String = (0..rng.below(120))
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let b = Buffer::with_chunk_size(&s, 3 + rng.below(9));
            let lower = s.to_ascii_lowercase();
            for _ in 0..30 {
                let needle: String = (0..1 + rng.below(3))
                    .map(|_| alphabet[rng.below(4)])
                    .collect();
                let from = boundary_near(&s, rng.below(s.len() + 1));
                let fwd = s[from..].find(&needle).map(|i| from + i);
                let bwd = s[..from].rfind(&needle);
                assert_eq!(b.find(&needle, from, true, true).map(|r| r.start), fwd);
                assert_eq!(b.find(&needle, from, false, true).map(|r| r.start), bwd);
                let ln = needle.to_ascii_lowercase();
                let ifwd = lower[from..].find(&ln).map(|i| from + i);
                let ibwd = lower[..from].rfind(&ln);
                assert_eq!(b.find(&needle, from, true, false).map(|r| r.start), ifwd);
                assert_eq!(b.find(&needle, from, false, false).map(|r| r.start), ibwd);
                // Splitting a range (as the parallel search does) must not lose matches.
                let k = boundary_near(&s, from + rng.below(s.len() - from + 1));
                for case in [true, false] {
                    let len = s.len();
                    let whole = b.find_in(&needle, from..len, len, true, case);
                    let split = b
                        .find_in(&needle, from..k, len, true, case)
                        .or_else(|| b.find_in(&needle, k..len, len, true, case));
                    assert_eq!(whole, split, "forward split at {k}");
                    let whole = b.find_in(&needle, 0..from, from, false, case);
                    let kb = boundary_near(&s, rng.below(from + 1));
                    let split = b
                        .find_in(&needle, kb..from, from, false, case)
                        .or_else(|| b.find_in(&needle, 0..kb, from, false, case));
                    assert_eq!(whole, split, "backward split at {kb}");
                }
            }
        }
    }

    #[test]
    fn open_and_write_round_trip() {
        let dir = std::env::temp_dir().join(format!("foxing-buf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rt.txt");
        // Larger than one chunk, with a multi-byte char straddling the CHUNK boundary.
        let mut s = "x".repeat(CHUNK - 1);
        s.push('✓');
        s.push_str("\r\nline two\nend");
        std::fs::write(&p, &s).unwrap();
        let b = Buffer::open(&p).unwrap();
        check_against(&b, &s);
        let mut out = Vec::new();
        b.write_to(&mut out).unwrap();
        assert_eq!(out, s.as_bytes());

        std::fs::write(&p, b"\xEF\xBB\xBFbom\n").unwrap();
        assert_eq!(contents(&Buffer::open(&p).unwrap()), "bom\n");
        std::fs::write(&p, b"caf\xE9").unwrap();
        assert_eq!(contents(&Buffer::open(&p).unwrap()), "café");
        let mut u16 = vec![0xFF, 0xFE];
        u16.extend("wide".encode_utf16().flat_map(|c| c.to_le_bytes()));
        std::fs::write(&p, u16).unwrap();
        assert_eq!(contents(&Buffer::open(&p).unwrap()), "wide");
    }
}
