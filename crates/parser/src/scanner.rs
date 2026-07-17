//! Cursor over a contiguous `&[u8]` that yields chunks delimited by a
//! runtime-swappable separator. Our input is a single slice, so the logic stays
//! simple while preserving the exact chunking semantics the parser relies on.
//!
use std::borrow::Cow;

use memchr::memmem;

/// Reads `data` as a sequence of separator-delimited chunks.
pub(crate) struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
    separator: Vec<u8>,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(data: &'a [u8], separator: &[u8]) -> Self {
        Self {
            data,
            pos: 0,
            separator: separator.to_vec(),
        }
    }

    /// Swap the separator used by subsequent [`Scanner::next_chunk`] calls. The
    /// parts phase toggles this between CRLF (text framing) and the raw MIME
    /// boundary (binary framing).
    pub(crate) fn set_separator(&mut self, separator: &[u8]) {
        self.separator.clear();
        self.separator.extend_from_slice(separator);
    }

    /// Return the bytes up to (but excluding) the next separator, advancing the
    /// cursor past that separator. When no separator remains, the final
    /// unterminated remainder is returned exactly once; every call thereafter
    /// returns `None`. A separator sitting exactly at the cursor yields an empty
    /// slice — the load-bearing "empty header line" vs. "EOF" distinction,
    /// modeled elsewhere as empty-string vs. null-string.
    pub(crate) fn next_chunk(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        match memmem::find(&self.data[self.pos..], &self.separator) {
            Some(rel) => {
                let chunk = &self.data[self.pos..self.pos + rel];
                self.pos += rel + self.separator.len();
                Some(chunk)
            }
            None => {
                let chunk = &self.data[self.pos..];
                self.pos = self.data.len();
                Some(chunk)
            }
        }
    }

    /// Up to `n` bytes at the cursor without advancing. Returns fewer than `n`
    /// bytes when the buffer is shorter.
    pub(crate) fn peek(&self, n: usize) -> &'a [u8] {
        let end = self.pos.saturating_add(n).min(self.data.len());
        &self.data[self.pos..end]
    }

    /// The bytes from the cursor to the end, without advancing. Used to hand the
    /// archive body (everything after the root header) to the part iterator.
    pub(crate) fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

/// Decode `bytes` as UTF-8, falling back to a 1:1 Latin-1 mapping (each byte to
/// the identical `U+00..` code point) when the bytes are not valid UTF-8. Never
/// fails. Header lines and text bodies flow through this.
pub(crate) fn decode_utf8_latin1(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(bytes.iter().map(|&b| b as char).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_mid_buffer_returns_bytes_before_it() {
        let mut s = Scanner::new(b"abc\r\ndef", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b"abc"[..]));
        assert_eq!(s.next_chunk(), Some(&b"def"[..]));
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn separator_at_start_yields_empty_chunk() {
        let mut s = Scanner::new(b"\r\nabc", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b""[..]));
        assert_eq!(s.next_chunk(), Some(&b"abc"[..]));
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn separator_at_end_has_no_trailing_empty_chunk() {
        let mut s = Scanner::new(b"abc\r\n", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b"abc"[..]));
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn absent_separator_yields_remainder_once_then_none() {
        let mut s = Scanner::new(b"abc", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b"abc"[..]));
        assert_eq!(s.next_chunk(), None);
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn empty_data_is_eof_immediately() {
        let mut s = Scanner::new(b"", b"\r\n");
        assert_eq!(s.next_chunk(), None);
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn adjacent_separators_yield_empty_chunks_between() {
        let mut s = Scanner::new(b"abc\r\n\r\ndef", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b"abc"[..]));
        assert_eq!(s.next_chunk(), Some(&b""[..]));
        assert_eq!(s.next_chunk(), Some(&b"def"[..]));
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn set_separator_mid_stream_switches_delimiter() {
        // Read the first CRLF-delimited line, then switch to a raw boundary.
        let mut s = Scanner::new(b"header\r\nbody--BOUNDARY tail", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b"header"[..]));
        s.set_separator(b"--BOUNDARY");
        assert_eq!(s.next_chunk(), Some(&b"body"[..]));
        assert_eq!(s.next_chunk(), Some(&b" tail"[..]));
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn peek_returns_up_to_n_without_advancing() {
        let mut s = Scanner::new(b"--boundary", b"\r\n");
        assert_eq!(s.peek(2), &b"--"[..]);
        // No advance: a subsequent read still sees the whole buffer.
        assert_eq!(s.peek(2), &b"--"[..]);
        assert_eq!(s.next_chunk(), Some(&b"--boundary"[..]));
    }

    #[test]
    fn peek_shorter_than_n_returns_what_is_available() {
        let s = Scanner::new(b"ab", b"\r\n");
        assert_eq!(s.peek(10), &b"ab"[..]);
    }

    #[test]
    fn nul_bytes_are_preserved_in_chunks() {
        let mut s = Scanner::new(b"a\x00b\r\nc\x00", b"\r\n");
        assert_eq!(s.next_chunk(), Some(&b"a\x00b"[..]));
        assert_eq!(s.next_chunk(), Some(&b"c\x00"[..]));
        assert_eq!(s.next_chunk(), None);
    }

    #[test]
    fn decode_utf8_borrows_valid_utf8() {
        let d = decode_utf8_latin1(b"hello");
        assert_eq!(d, "hello");
        assert!(matches!(d, Cow::Borrowed(_)));
    }

    #[test]
    fn decode_latin1_fallback_maps_bytes_to_code_points() {
        let d = decode_utf8_latin1(b"caf\xe9");
        assert_eq!(d, "caf\u{e9}");
        assert!(matches!(d, Cow::Owned(_)));
    }
}
