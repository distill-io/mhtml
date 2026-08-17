//! Faithful, lossless MHTML (`multipart/related`) archive parser.
//!
//! [`Archive::parse`] reads only the root header; [`Archive::parts`] yields
//! parts lazily; each [`Part::body`] decodes on demand. [`Archive::parse_all`]
//! is the strict convenience that eagerly decodes every part and fails the
//! whole archive on the first error.

use std::time::SystemTime;

mod headers;
mod parts;
mod qp;
mod scanner;

use headers::MimeHeader;

pub use parts::{OwnedPart, Part, Parts};

/// Errors produced while parsing or decoding an MHTML archive.
///
/// Real parsers report all of these as a single whole-archive failure; we
/// surface them typed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `multipart/*` header carried no `boundary` parameter, so its parts
    /// cannot be delimited (the whole archive is rejected here).
    #[error("multipart MIME header is missing its boundary parameter")]
    MissingBoundary,
    /// A binary-encoded part appeared without an enclosing boundary to delimit
    /// it (only possible for a non-multipart, IE-style root).
    #[error("binary part has no boundary to delimit it")]
    BinaryPartWithoutBoundary,
    /// End of input was reached before a part's delimiting boundary.
    #[error("MHTML part is not terminated by a boundary")]
    UnterminatedPart,
    /// The bytes trailing a binary part's boundary were malformed (too few
    /// bytes to classify, or a non-empty line where the boundary's CRLF was
    /// expected).
    #[error("malformed boundary terminator after binary part")]
    MalformedBoundary,
    /// A base64-encoded part failed to decode.
    #[error("invalid base64 content in MHTML part")]
    InvalidBase64,
}

/// A part's `Content-Transfer-Encoding`.
///
/// `Unknown` (and a missing header) are treated as `Binary` by the parser;
/// a yielded [`Part`] therefore never reports `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferEncoding {
    QuotedPrintable,
    Base64,
    SevenBit,
    EightBit,
    Binary,
    Unknown,
}

/// A parsed MHTML archive borrowing the input bytes.
pub struct Archive<'a> {
    /// The parsed root header — source of the creation date, snapshot location,
    /// and (for multipart) the boundaries that frame the parts.
    root: MimeHeader,
    /// Everything after the root header, where the parts begin.
    body: &'a [u8],
}

impl<'a> Archive<'a> {
    /// Parse the archive's root header, borrowing `data`. Only the top-level
    /// header is read here; bodies are decoded on demand.
    ///
    /// Fails only when the root is a `multipart/*` type without a `boundary`
    /// parameter (the one hard header-level error).
    pub fn parse(data: &'a [u8]) -> Result<Archive<'a>, Error> {
        let mut scanner = scanner::Scanner::new(data, b"\r\n");
        let root = MimeHeader::parse(&mut scanner)?;
        let body = scanner.remaining();
        Ok(Archive { root, body })
    }

    /// Strict parse: eagerly parse and decode every part, failing the whole
    /// archive on the first error (an all-or-nothing contract).
    pub fn parse_all(&self) -> Result<Vec<OwnedPart>, Error> {
        let mut out = Vec::new();
        for part in self.parts() {
            let part = part?;
            let body = part.body()?.into_owned();
            out.push(OwnedPart {
                content_type: part.content_type,
                charset: part.charset,
                content_location: part.content_location,
                content_id: part.content_id,
                transfer_encoding: part.transfer_encoding,
                body,
            });
        }
        Ok(out)
    }

    /// Lazily iterate the archive's parts.
    pub fn parts(&self) -> Parts<'a> {
        Parts::new(self.body, &self.root)
    }

    /// The archive-level `Date` header, if present and valid. Per-part `Date`
    /// headers are ignored.
    ///
    /// Divergence: some parsers expose the creation date only after a *fully
    /// successful* parse; we expose it straight from the root header. The golden
    /// date tests all parse successfully, so both agree.
    pub fn creation_date(&self) -> Option<SystemTime> {
        self.root.date
    }

    /// The `Snapshot-Content-Location` header (the saved page's URL).
    pub fn snapshot_content_location(&self) -> Option<&str> {
        self.root.snapshot_content_location.as_deref()
    }
}

/// Translate a `Content-ID` of the form `<foo@bar.com>` into a `cid:` URL
/// (`cid:foo@bar.com`).
///
/// Returns `None` if the content-id is `<= 2` bytes long or is not wrapped in
/// `<...>` (rfc2557 §8.3).
pub fn content_id_to_cid_url(content_id: &str) -> Option<String> {
    if content_id.len() <= 2 {
        return None;
    }
    // The wrapping angle brackets are single-byte ASCII, so trimming them can
    // never split a multi-byte code point.
    if !content_id.starts_with('<') || !content_id.ends_with('>') {
        return None;
    }
    Some(format!("cid:{}", &content_id[1..content_id.len() - 1]))
}
