//! The part iterator and per-part body decoding.
//!
//! `multipart/alternative` nesting is conventionally handled by recursion. We
//! reproduce the *observable* behaviour lazily: [`Parts`] is a pull iterator
//! that yields one [`Part`] per `next()` call, flattening IE-style nesting via
//! an explicit stack of boundary contexts. It is lenient — it yields `Ok(part)`
//! until the first corruption, then that one `Err`, then `None` forever.

use std::borrow::Cow;

use base64::Engine;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};

use crate::headers::MimeHeader;
use crate::scanner::{Scanner, decode_utf8_latin1};
use crate::{Error, TransferEncoding};

/// Forgiving-base64 engine: standard alphabet, whitespace pre-stripped by the
/// caller, padding accepted whether present or not (forgiving-base64 per the
/// WHATWG Infra spec, which real archives rely on).
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// A part borrowing the archive bytes; its body is decoded on demand.
///
/// `raw` is the pre-decode content region: for binary parts it borrows directly
/// from the archive; for the text encodings it is the line-joined accumulation.
pub struct Part<'a> {
    pub content_type: String,
    pub charset: Option<String>,
    pub content_location: Option<String>,
    pub content_id: Option<String>,
    /// The *effective* transfer encoding: an `Unknown`/absent header has already
    /// been coerced to [`TransferEncoding::Binary`] (real archives rely on this).
    pub transfer_encoding: TransferEncoding,
    raw: Cow<'a, [u8]>,
}

impl<'a> Part<'a> {
    /// Decode this part's body according to its transfer encoding.
    ///
    /// Binary/7bit/8bit pass through their accumulated bytes (binary borrows,
    /// so the common case is zero-copy). Quoted-printable and base64 always
    /// allocate; only base64 can fail (invalid input ⇒ [`Error::InvalidBase64`]).
    pub fn body(&self) -> Result<Cow<'a, [u8]>, Error> {
        match self.transfer_encoding {
            TransferEncoding::Base64 => {
                // Forgiving-base64 first strips ASCII whitespace.
                let stripped: Vec<u8> = self
                    .raw
                    .iter()
                    .copied()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect();
                BASE64
                    .decode(&stripped)
                    .map(Cow::Owned)
                    .map_err(|_| Error::InvalidBase64)
            }
            TransferEncoding::QuotedPrintable => Ok(Cow::Owned(crate::qp::decode(&self.raw))),
            // Unknown never survives to a `Part` (coerced to Binary at parse
            // time), but the arm keeps the match exhaustive without a wildcard.
            TransferEncoding::SevenBit
            | TransferEncoding::EightBit
            | TransferEncoding::Binary
            | TransferEncoding::Unknown => Ok(self.raw.clone()),
        }
    }
}

/// An owned part with its body already decoded (produced by
/// `Archive::parse_all`).
pub struct OwnedPart {
    pub content_type: String,
    pub charset: Option<String>,
    pub content_location: Option<String>,
    pub content_id: Option<String>,
    pub transfer_encoding: TransferEncoding,
    pub body: Vec<u8>,
}

/// A `multipart/*` boundary context: `--boundary` ends a part, `--boundary--`
/// ends the (sub)document. Rather than nesting by recursion, we keep a stack.
struct Context {
    part_boundary: String,
    doc_boundary: String,
}

/// Lazy iterator over an archive's parts. See the module docs for the leniency
/// contract.
pub struct Parts<'a> {
    scanner: Scanner<'a>,
    mode: Mode,
    /// Latched once a terminal `Err` (or the single non-multipart part) has been
    /// produced, so every subsequent `next()` returns `None`.
    done: bool,
}

enum Mode {
    /// Non-multipart (IE-style) root: the whole body is one part described by
    /// the root header. `Some` until that single part has been yielded.
    Single(Option<MimeHeader>),
    /// Multipart root: a stack of boundary contexts (index 0 is the root),
    /// growing on `multipart/alternative` nesting.
    Multipart(Vec<Context>),
}

impl<'a> Parts<'a> {
    /// Build the iterator over an archive body (`body` is everything after the
    /// root header), using `root` to decide the framing.
    ///
    /// For a multipart root this eagerly skips the preamble to the first
    /// boundary; reaching EOF while skipping is not itself an error — the
    /// subsequent header parse over EOF yields an empty header whose binary path
    /// then errors.
    pub(crate) fn new(body: &'a [u8], root: &MimeHeader) -> Parts<'a> {
        let mut scanner = Scanner::new(body, b"\r\n");
        if root.is_multipart() {
            let mut stack = Vec::new();
            if let (Some(part_boundary), Some(doc_boundary)) =
                (&root.end_of_part_boundary, &root.end_of_document_boundary)
            {
                skip_lines_until(&mut scanner, part_boundary);
                stack.push(Context {
                    part_boundary: part_boundary.clone(),
                    doc_boundary: doc_boundary.clone(),
                });
            }
            Parts {
                scanner,
                mode: Mode::Multipart(stack),
                done: false,
            }
        } else {
            Parts {
                scanner,
                mode: Mode::Single(Some(root.clone())),
                done: false,
            }
        }
    }

    /// Drive the multipart state machine until it produces one part, one error,
    /// or exhausts the stack. Loops until the end of the archive, including
    /// `multipart/alternative` flattening.
    fn advance_multipart(
        scanner: &mut Scanner<'a>,
        stack: &mut Vec<Context>,
    ) -> Option<Result<Part<'a>, Error>> {
        loop {
            if stack.is_empty() {
                return None;
            }
            let header = match MimeHeader::parse(scanner) {
                Ok(h) => h,
                Err(e) => return Some(Err(e)),
            };

            // IE nesting: a `multipart/alternative` sub-document is descended
            // into with its own boundaries (skipping its preamble), then its
            // parts are flattened into this same stream. The comparison is
            // case-sensitive against the original-case MIME type. A multipart
            // header always carries both boundaries (`MimeHeader::parse` errors
            // otherwise), so the pattern always matches here; any other header
            // falls through to the part-body parse below.
            if header.content_type == "multipart/alternative"
                && let (Some(nested_part), Some(nested_doc)) = (
                    &header.end_of_part_boundary,
                    &header.end_of_document_boundary,
                )
            {
                let nested_part = nested_part.clone();
                let nested_doc = nested_doc.clone();
                skip_lines_until(scanner, &nested_part);
                stack.push(Context {
                    part_boundary: nested_part,
                    doc_boundary: nested_doc,
                });
                continue;
            }

            let (part_boundary, doc_boundary) = {
                // The stack is non-empty here (checked above, and the only
                // growth path `continue`s), so this never short-circuits.
                let ctx = stack.last()?;
                (ctx.part_boundary.clone(), ctx.doc_boundary.clone())
            };
            match parse_next_part(scanner, header, Some(&part_boundary), Some(&doc_boundary)) {
                Ok((part, end_of_archive)) => {
                    if end_of_archive {
                        // This (sub)document is finished. Pop it; if a parent
                        // remains, skip to the parent's next part boundary and
                        // continue the parent loop on the following `next()`.
                        stack.pop();
                        if let Some(parent) = stack.last() {
                            let parent_boundary = parent.part_boundary.clone();
                            skip_lines_until(scanner, &parent_boundary);
                        }
                    }
                    return Some(Ok(part));
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl<'a> Iterator for Parts<'a> {
    type Item = Result<Part<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = match &mut self.mode {
            Mode::Single(header) => match header.take() {
                None => None,
                Some(header) => {
                    // A non-multipart root is exactly one part: nothing follows,
                    // whether it decodes or errors.
                    self.done = true;
                    Some(
                        parse_next_part(&mut self.scanner, header, None, None)
                            .map(|(part, _)| part),
                    )
                }
            },
            Mode::Multipart(stack) => Self::advance_multipart(&mut self.scanner, stack),
        };
        if matches!(result, Some(Err(_))) {
            self.done = true;
        }
        result
    }
}

/// Consume CRLF lines until one exactly equals `boundary` (then stop, having
/// consumed it) or EOF is reached; callers ignore whether the boundary was
/// found.
///
/// Comparing the raw chunk bytes to the ASCII boundary is equivalent to a
/// decoded-string comparison: a UTF-8/Latin-1 decode equals an ASCII boundary
/// string only when the underlying bytes are exactly that boundary.
fn skip_lines_until(scanner: &mut Scanner<'_>, boundary: &str) {
    while let Some(chunk) = scanner.next_chunk() {
        if chunk == boundary.as_bytes() {
            return;
        }
    }
}

/// Parse the body region of one part, given its already-parsed `header` and the
/// enclosing context's boundaries (both `None` for a non-multipart root part).
/// Returns the part plus whether the end-of-document boundary was reached.
fn parse_next_part<'a>(
    scanner: &mut Scanner<'a>,
    header: MimeHeader,
    part_boundary: Option<&str>,
    doc_boundary: Option<&str>,
) -> Result<(Part<'a>, bool), Error> {
    // If no content transfer encoding is specified, default to binary.
    let encoding = match header.transfer_encoding {
        TransferEncoding::Unknown => TransferEncoding::Binary,
        other => other,
    };
    // Both boundaries are empty together or set together.
    let check_boundary = part_boundary.is_some();
    let mut end_of_archive = false;
    let mut end_of_part_reached = false;
    let raw: Cow<'a, [u8]>;

    if encoding == TransferEncoding::Binary {
        let Some(part_boundary) = part_boundary else {
            // Binary contents require an end-of-part boundary to delimit them.
            return Err(Error::BinaryPartWithoutBoundary);
        };
        // Bug-compat: real writers do not prepend CRLF to the boundary after
        // a binary part, so we split on the *raw* boundary (no leading CRLF).
        // The content may therefore keep a trailing CRLF, stripped below.
        scanner.set_separator(part_boundary.as_bytes());
        let Some(chunk) = scanner.next_chunk() else {
            // EOF without ever reaching the boundary.
            return Err(Error::UnterminatedPart);
        };
        scanner.set_separator(b"\r\n");

        // Strip one trailing CRLF if present (it may really belong to the
        // content, but this is the bug-compat risk accepted here).
        let body = if chunk.len() >= 2
            && chunk[chunk.len() - 2] == b'\r'
            && chunk[chunk.len() - 1] == b'\n'
        {
            &chunk[..chunk.len() - 2]
        } else {
            chunk
        };

        let next2 = scanner.peek(2);
        if next2.len() < 2 {
            return Err(Error::MalformedBoundary);
        }
        end_of_part_reached = true;
        end_of_archive = next2 == b"--";
        if !end_of_archive {
            // The boundary was trailed by CRLF; that CRLF now shows up as an
            // empty line. Anything else is malformed.
            match scanner.next_chunk() {
                Some([]) => {}
                _ => return Err(Error::MalformedBoundary),
            }
        }
        raw = Cow::Borrowed(body);
    } else {
        // Text encodings: accumulate CRLF lines until a boundary (or EOF for a
        // non-multipart root).
        let mut content: Vec<u8> = Vec::new();
        while let Some(chunk) = scanner.next_chunk() {
            if let Some(doc) = doc_boundary {
                end_of_archive = chunk == doc.as_bytes();
            }
            if let Some(part) = part_boundary
                && (chunk == part.as_bytes() || end_of_archive)
            {
                end_of_part_reached = true;
                break;
            }
            // Each line is decoded UTF-8-with-Latin-1-fallback and re-encoded as
            // UTF-8, so invalid-UTF-8 bytes get Latin-1 → UTF-8 expanded.
            // `decode_utf8_latin1` + `as_bytes` reproduces exactly that.
            content.extend_from_slice(decode_utf8_latin1(chunk).as_bytes());
            if encoding == TransferEncoding::QuotedPrintable {
                // The QP decoder needs CRLF-terminated lines to spot soft breaks.
                content.extend_from_slice(b"\r\n");
            }
        }
        raw = Cow::Owned(content);
    }

    if !end_of_part_reached && check_boundary {
        // A multipart part must be terminated by a boundary; hitting EOF first
        // is corruption.
        return Err(Error::UnterminatedPart);
    }

    let part = Part {
        content_type: header.content_type,
        charset: header.charset,
        content_location: header.content_location,
        content_id: header.content_id,
        transfer_encoding: encoding,
        raw,
    };
    Ok((part, end_of_archive))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(input: &[u8]) -> MimeHeader {
        let mut s = Scanner::new(input, b"\r\n");
        MimeHeader::parse(&mut s).expect("header parses")
    }

    /// Drive `parse_next_part` over a body region with the given boundaries.
    fn one_part<'a>(
        body: &'a [u8],
        hdr: MimeHeader,
        part_b: Option<&str>,
        doc_b: Option<&str>,
    ) -> Result<(Part<'a>, bool), Error> {
        let mut s = Scanner::new(body, b"\r\n");
        parse_next_part(&mut s, hdr, part_b, doc_b)
    }

    #[test]
    fn binary_without_boundary_is_error() {
        // Non-multipart root + binary encoding: no boundary to delimit it.
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        let result = one_part(b"anything", hdr, None, None);
        assert!(matches!(result, Err(Error::BinaryPartWithoutBoundary)));
    }

    #[test]
    fn binary_strips_one_trailing_crlf_before_boundary() {
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        let (part, eoa) = one_part(b"data\r\n--B\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert_eq!(part.body().unwrap().as_ref(), b"data");
        assert!(!eoa);
    }

    #[test]
    fn binary_without_trailing_crlf_keeps_all_bytes() {
        // Bug-compat: a binary part joined directly to the boundary (no CRLF).
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        let (part, _eoa) = one_part(b"data--B\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert_eq!(part.body().unwrap().as_ref(), b"data");
    }

    #[test]
    fn binary_document_boundary_sets_end_of_archive() {
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        let (part, eoa) = one_part(b"--B--\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert!(part.body().unwrap().is_empty());
        assert!(eoa);
    }

    #[test]
    fn binary_empty_body_at_eof_is_unterminated() {
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        // The scanner is already at EOF: `next_chunk` yields `None`, an early
        // return.
        let result = one_part(b"", hdr, Some("--B"), Some("--B--"));
        assert!(matches!(result, Err(Error::UnterminatedPart)));
    }

    #[test]
    fn binary_no_boundary_in_remainder_is_malformed() {
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        // No boundary before EOF: the scanner returns the whole remainder as the
        // content, so the failure surfaces at the
        // boundary-tail peek — fewer than two bytes remain.
        let result = one_part(b"data no boundary", hdr, Some("--B"), Some("--B--"));
        assert!(matches!(result, Err(Error::MalformedBoundary)));
    }

    #[test]
    fn binary_non_empty_tail_after_part_boundary_is_malformed() {
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        // "--B" is followed by "junk" rather than the expected CRLF empty line.
        let result = one_part(b"data--Bjunk\r\n", hdr, Some("--B"), Some("--B--"));
        assert!(matches!(result, Err(Error::MalformedBoundary)));
    }

    #[test]
    fn binary_fewer_than_two_bytes_after_boundary_is_malformed() {
        let hdr = header(b"Content-Transfer-Encoding: binary\r\n\r\n");
        // Boundary sits at the very end: peek(2) can only see one byte.
        let result = one_part(b"data--B\n", hdr, Some("--B"), Some("--B--"));
        assert!(matches!(result, Err(Error::MalformedBoundary)));
    }

    #[test]
    fn seven_bit_joins_lines_without_crlf() {
        let hdr = header(b"Content-Transfer-Encoding: 7bit\r\n\r\n");
        let (part, eoa) =
            one_part(b"123\r\nabc\r\n--B--\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert_eq!(part.body().unwrap().as_ref(), b"123abc");
        assert!(eoa);
    }

    #[test]
    fn quoted_printable_reappends_crlf_per_line() {
        let hdr = header(b"Content-Transfer-Encoding: quoted-printable\r\n\r\n");
        let (part, _eoa) =
            one_part(b"a=3Db=\r\nc\r\n--B\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        // Soft break "=\r\n" joins the two lines; "=3D" decodes to "=".
        assert_eq!(part.body().unwrap().as_ref(), b"a=bc\r\n");
    }

    #[test]
    fn text_part_latin1_bytes_are_expanded_to_utf8() {
        // A raw 0xE9 byte is invalid UTF-8; the Latin-1 fallback
        // expands it to U+00E9 → 0xC3 0xA9. 8bit passes the accumulation
        // through unchanged, exposing the quirk.
        let hdr = header(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
        let (part, _eoa) =
            one_part(b"caf\xe9\r\n--B--\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert_eq!(part.body().unwrap().as_ref(), b"caf\xc3\xa9");
    }

    #[test]
    fn base64_decodes_padding_indifferently_and_strips_whitespace() {
        let hdr = header(b"Content-Transfer-Encoding: base64\r\n\r\n");
        let (part, _eoa) =
            one_part(b"MTIz YWJj\r\n--B--\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert_eq!(part.body().unwrap().as_ref(), b"123abc");
    }

    #[test]
    fn base64_invalid_content_is_error() {
        let hdr = header(b"Content-Transfer-Encoding: base64\r\n\r\n");
        let (part, _eoa) = one_part(b"!!!!\r\n--B--\r\n", hdr, Some("--B"), Some("--B--")).unwrap();
        assert!(matches!(part.body().unwrap_err(), Error::InvalidBase64));
    }

    #[test]
    fn non_multipart_text_reads_to_eof_successfully() {
        // IE-style single part: no boundary, EOF ends the part.
        let hdr = header(b"Content-Transfer-Encoding: 7bit\r\n\r\n");
        let (part, _eoa) = one_part(b"line1\r\nline2\r\n", hdr, None, None).unwrap();
        assert_eq!(part.body().unwrap().as_ref(), b"line1line2");
    }

    #[test]
    fn mixed_case_multipart_alternative_is_not_flattened() {
        // The `multipart/alternative` check is case-SENSITIVE against the
        // original-case MIME type, so a mixed-case `multipart/Alternative` part
        // is NOT recursed/flattened.
        // It is read as one opaque (binary) part whose stored content type keeps
        // its original case.
        let root = header(b"Content-Type: multipart/related; boundary=OUTER\r\n\r\n");
        let body = b"--OUTER\r\n\
                     Content-Type: multipart/Alternative; boundary=INNER\r\n\
                     \r\n\
                     --INNER\r\n\
                     Content-Type: text/plain\r\n\
                     Content-Location: http://x/inner\r\n\
                     \r\n\
                     inner\r\n\
                     --INNER--\r\n\
                     --OUTER--\r\n";
        let mut parts = Parts::new(body, &root);
        let first = parts.next().expect("one part").expect("the part parses ok");
        assert_eq!(first.content_type, "multipart/Alternative");
        assert_eq!(first.content_location, None);
        assert!(parts.next().is_none());
    }

    #[test]
    fn skip_lines_until_stops_after_consuming_boundary() {
        let mut s = Scanner::new(b"junk\r\npreamble\r\n--B\r\nafter\r\n", b"\r\n");
        skip_lines_until(&mut s, "--B");
        assert_eq!(s.next_chunk(), Some(&b"after"[..]));
    }
}
