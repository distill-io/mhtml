//! MIME header parsing for MHTML parts: header-line parsing, key/value
//! extraction and content-transfer-encoding parsing, plus a local relaxed
//! `Content-Type` parameter parser.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;

use crate::TransferEncoding;
use crate::scanner::{Scanner, decode_utf8_latin1};

/// Trim leading/trailing ASCII whitespace only (`0x09` TAB, `0x0A` LF, `0x0C`
/// FF, `0x0D` CR, `0x20` SPACE). Rust's `str::trim` additionally strips Unicode
/// `White_Space` (e.g. NBSP `U+00A0`), which the Latin-1 header fallback can
/// introduce; using this preserves such bytes exactly.
fn strip_ascii_whitespace(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c as u32, 0x09 | 0x0A | 0x0C | 0x0D | 0x20))
}

/// Parse an RFC 2822 `Date` header value into a [`SystemTime`].
///
/// Returns `None` for anything the RFC 2822 grammar rejects (matching the
/// golden `overflowed`/`invalid` date cases). Accepts a one-digit day and the
/// `-0000` "unknown local zone" form.
fn parse_date(value: &str) -> Option<SystemTime> {
    let odt = OffsetDateTime::parse(value.trim(), &Rfc2822).ok()?;
    let ts = odt.unix_timestamp();
    if ts >= 0 {
        Some(UNIX_EPOCH + Duration::from_secs(ts as u64))
    } else {
        Some(UNIX_EPOCH - Duration::from_secs(ts.unsigned_abs()))
    }
}

/// Map a `Content-Transfer-Encoding` value to its [`TransferEncoding`].
///
/// The value is whitespace-trimmed and ASCII-lowercased; any token outside the
/// known set (including an empty/absent value) becomes
/// [`TransferEncoding::Unknown`].
fn parse_content_transfer_encoding(value: &str) -> TransferEncoding {
    match strip_ascii_whitespace(value).to_ascii_lowercase().as_str() {
        "base64" => TransferEncoding::Base64,
        "quoted-printable" => TransferEncoding::QuotedPrintable,
        "8bit" => TransferEncoding::EightBit,
        "7bit" => TransferEncoding::SevenBit,
        "binary" => TransferEncoding::Binary,
        _ => TransferEncoding::Unknown,
    }
}

/// Read the RFC 822 key/value section from `scanner` (which must be set to the
/// CRLF separator).
///
/// Stops at EOF or the first empty line (the header/body divider, consumed).
/// Lines starting with a space or tab continue the previous value, appending
/// everything after that one leading whitespace byte. Otherwise the pending
/// key is flushed (values trimmed on both ends) and the line is split at its
/// first `:` into a trimmed+lowercased key and a verbatim value; a line with no
/// `:` is ignored (but still flushes the pending key). Duplicate keys: an
/// intermediate occurrence is inserted only when absent (first-wins), but the
/// trailing key overwrites (last-wins).
fn retrieve_key_value_pairs(scanner: &mut Scanner<'_>) -> HashMap<String, String> {
    let mut pairs = HashMap::new();
    let mut key: Option<String> = None;
    let mut value = String::new();
    while let Some(chunk) = scanner.next_chunk() {
        let line = decode_utf8_latin1(chunk);
        if line.is_empty() {
            break;
        }
        let first = line.as_bytes()[0];
        if first == b'\t' || first == b' ' {
            value.push_str(&line[1..]);
            continue;
        }
        if let Some(k) = key.take() {
            // The non-overwriting insert (used for every non-final key) leaves
            // an existing entry untouched — only the final insert below
            // overwrites. So a key duplicated before the final header is
            // first-wins.
            pairs
                .entry(k)
                .or_insert_with(|| strip_ascii_whitespace(&value).to_string());
            value.clear();
        }
        match line.find(':') {
            None => continue,
            Some(idx) => {
                key = Some(strip_ascii_whitespace(&line[..idx]).to_ascii_lowercase());
                value.push_str(&line[idx + 1..]);
            }
        }
    }
    if let Some(k) = key.take() {
        pairs.insert(k, strip_ascii_whitespace(&value).to_string());
    }
    pairs
}

/// The parsed MIME header of one MHTML part (or the archive root).
#[derive(Debug, Clone)]
pub(crate) struct MimeHeader {
    /// MIME type in its original case (`""` when no `Content-Type` header was
    /// present); `is_multipart` and callers compare it case-insensitively where
    /// appropriate.
    pub(crate) content_type: String,
    /// `charset` parameter of a non-multipart `Content-Type`.
    pub(crate) charset: Option<String>,
    pub(crate) transfer_encoding: TransferEncoding,
    pub(crate) content_location: Option<String>,
    pub(crate) content_id: Option<String>,
    pub(crate) date: Option<SystemTime>,
    /// Archive-level `Snapshot-Content-Location` header (the saved page's URL).
    /// Only meaningful on the root header; ignored on part headers.
    pub(crate) snapshot_content_location: Option<String>,
    /// `type` parameter of a multipart `Content-Type`.
    pub(crate) multipart_type: Option<String>,
    /// `"--" + boundary`, delimiting parts (multipart only).
    pub(crate) end_of_part_boundary: Option<String>,
    /// `"--" + boundary + "--"`, delimiting the archive (multipart only).
    pub(crate) end_of_document_boundary: Option<String>,
}

impl MimeHeader {
    /// Read and interpret a part's MIME header from `scanner` (set to CRLF).
    /// Errors only when a multipart header lacks a `boundary` parameter.
    pub(crate) fn parse(scanner: &mut Scanner<'_>) -> Result<MimeHeader, crate::Error> {
        let pairs = retrieve_key_value_pairs(scanner);
        let mut header = MimeHeader {
            content_type: String::new(),
            charset: None,
            transfer_encoding: TransferEncoding::Unknown,
            content_location: None,
            content_id: None,
            date: None,
            snapshot_content_location: None,
            multipart_type: None,
            end_of_part_boundary: None,
            end_of_document_boundary: None,
        };

        if let Some(content_type) = pairs.get("content-type") {
            let parsed = ParsedContentType::parse(content_type);
            header.content_type = parsed.mime_type.clone();
            if header.is_multipart() {
                header.multipart_type = parsed.param("type").map(str::to_string);
                let boundary = parsed
                    .param("boundary")
                    .ok_or(crate::Error::MissingBoundary)?;
                header.end_of_part_boundary = Some(format!("--{boundary}"));
                header.end_of_document_boundary = Some(format!("--{boundary}--"));
            } else {
                header.charset = parsed
                    .param("charset")
                    .map(|c| strip_ascii_whitespace(c).to_string());
            }
        }

        if let Some(encoding) = pairs.get("content-transfer-encoding") {
            header.transfer_encoding = parse_content_transfer_encoding(encoding);
        }
        if let Some(location) = pairs.get("content-location") {
            header.content_location = Some(location.clone());
        }
        if let Some(id) = pairs.get("content-id") {
            header.content_id = Some(id.clone());
        }
        if let Some(location) = pairs.get("snapshot-content-location") {
            header.snapshot_content_location = Some(location.clone());
        }
        if let Some(date) = pairs.get("date") {
            header.date = parse_date(date);
        }

        Ok(header)
    }

    /// Whether this is a `multipart/*` container (ASCII case-insensitive).
    pub(crate) fn is_multipart(&self) -> bool {
        // Case-insensitive prefix check; `content_type` preserves original case.
        self.content_type
            .as_bytes()
            .get(..b"multipart/".len())
            .is_some_and(|p| p.eq_ignore_ascii_case(b"multipart/"))
    }
}

/// A `Content-Type` value split into its (original-case) MIME type and
/// parameter list, parsed in relaxed mode.
struct ParsedContentType {
    mime_type: String,
    params: Vec<(String, String)>,
}

impl ParsedContentType {
    /// Parse `value` leniently: the MIME type is everything before the first
    /// `;` (ASCII-whitespace-trimmed; original case preserved); parameters are
    /// `;`-separated `name=value`
    /// pairs with trimmed+lowercased names and trimmed values (one surrounding
    /// pair of double quotes stripped). Segments without `=`, and empty
    /// segments, are skipped.
    fn parse(value: &str) -> ParsedContentType {
        let mut segments = value.split(';');
        let mime_type = strip_ascii_whitespace(segments.next().unwrap_or("")).to_string();
        let mut params = Vec::new();
        for segment in segments {
            if segment.trim().is_empty() {
                continue;
            }
            let Some((name, val)) = segment.split_once('=') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            params.push((name, unquote(val.trim()).to_string()));
        }
        ParsedContentType { mime_type, params }
    }

    /// The value of parameter `name` (already lowercased), if present.
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Strip a single surrounding pair of double quotes, if present.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(input: &[u8]) -> HashMap<String, String> {
        let mut s = Scanner::new(input, b"\r\n");
        retrieve_key_value_pairs(&mut s)
    }

    #[test]
    fn folds_continuation_lines_with_space_and_tab() {
        // Both a space- and a tab-prefixed continuation append, stripping only
        // the single leading whitespace byte.
        let m = kv(b"Content-Type: multipart/related;\r\n boundary=abc\r\n\ttail\r\n\r\n");
        assert_eq!(
            m.get("content-type").map(String::as_str),
            Some("multipart/related;boundary=abctail"),
        );
    }

    #[test]
    fn duplicate_key_last_value_wins() {
        let m = kv(b"X: first\r\nX: second\r\n\r\n");
        assert_eq!(m.get("x").map(String::as_str), Some("second"));
    }

    #[test]
    fn duplicate_non_final_key_keeps_first_value() {
        // The non-final headers are flushed via a non-overwriting insert (only
        // the trailing header overwrites), so a key duplicated *before* the
        // final header is first-wins. Here the final header is
        // `content-location`, so both `content-type` occurrences go through the
        // non-overwriting insert and the first (`text/plain`) is kept.
        let m = kv(b"Content-Type: text/plain\r\n\
              Content-Type: text/html\r\n\
              Content-Location: http://x/\r\n\r\n");
        assert_eq!(
            m.get("content-type").map(String::as_str),
            Some("text/plain")
        );
        assert_eq!(
            m.get("content-location").map(String::as_str),
            Some("http://x/")
        );
    }

    #[test]
    fn value_trim_strips_only_ascii_whitespace_not_nbsp() {
        // The header trim strips only ASCII whitespace, so a trailing 0xA0
        // byte (Latin-1-decoded to U+00A0 NBSP) is preserved. Rust's `str::trim`
        // (all Unicode White_Space) would wrongly drop it.
        let m = kv(b"Content-Location: http://x/\xa0\r\n\r\n");
        assert_eq!(
            m.get("content-location").map(String::as_str),
            Some("http://x/\u{a0}")
        );
    }

    #[test]
    fn line_without_colon_is_ignored_but_flushes_pending_key() {
        let m = kv(b"Content-ID: <id@host>\r\nnonsense line\r\nContent-Type: text/html\r\n\r\n");
        assert_eq!(m.get("content-id").map(String::as_str), Some("<id@host>"));
        assert_eq!(m.get("content-type").map(String::as_str), Some("text/html"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn empty_line_ends_section_and_key_is_lowercased_trimmed() {
        // The value after the empty divider line is not read into the map.
        let m = kv(b"KEY  : value\r\n\r\nnot: read\r\n");
        assert_eq!(m.get("key").map(String::as_str), Some("value"));
        assert_eq!(m.len(), 1);
    }

    fn parse_header(input: &[u8]) -> Result<MimeHeader, crate::Error> {
        let mut s = Scanner::new(input, b"\r\n");
        MimeHeader::parse(&mut s)
    }

    #[test]
    fn multipart_quoted_boundary_builds_sentinels() {
        let h = parse_header(
            b"Content-Type: multipart/related; type=\"text/html\"; boundary=\"BND\"\r\n\r\n",
        )
        .unwrap();
        assert_eq!(h.content_type, "multipart/related");
        assert_eq!(h.multipart_type.as_deref(), Some("text/html"));
        assert_eq!(h.end_of_part_boundary.as_deref(), Some("--BND"));
        assert_eq!(h.end_of_document_boundary.as_deref(), Some("--BND--"));
        // Multipart headers carry no charset.
        assert_eq!(h.charset, None);
    }

    #[test]
    fn multipart_unquoted_boundary_builds_sentinels() {
        let h = parse_header(b"Content-Type: multipart/related; boundary=--xyz\r\n\r\n").unwrap();
        assert_eq!(h.end_of_part_boundary.as_deref(), Some("----xyz"));
        assert_eq!(h.end_of_document_boundary.as_deref(), Some("----xyz--"));
    }

    #[test]
    fn multipart_missing_boundary_is_error() {
        let err = parse_header(b"Content-Type: multipart/related\r\n\r\n").unwrap_err();
        assert!(matches!(err, crate::Error::MissingBoundary));
    }

    #[test]
    fn non_multipart_extracts_charset_and_fields() {
        let h = parse_header(
            b"Content-Type: text/html; charset=\"UTF-8\"\r\n\
              Content-Transfer-Encoding: quoted-printable\r\n\
              Content-Location: http://example.com/\r\n\
              Content-ID: <id@host>\r\n\r\n",
        )
        .unwrap();
        assert_eq!(h.content_type, "text/html");
        assert_eq!(h.charset.as_deref(), Some("UTF-8"));
        assert_eq!(h.transfer_encoding, TransferEncoding::QuotedPrintable);
        assert_eq!(h.content_location.as_deref(), Some("http://example.com/"));
        assert_eq!(h.content_id.as_deref(), Some("<id@host>"));
        assert_eq!(h.end_of_part_boundary, None);
    }

    #[test]
    fn absent_content_type_yields_empty_non_multipart_header() {
        let h = parse_header(b"Content-Location: http://x/\r\n\r\n").unwrap();
        assert_eq!(h.content_type, "");
        assert!(!h.is_multipart());
        assert_eq!(h.transfer_encoding, TransferEncoding::Unknown);
    }

    #[test]
    fn snapshot_content_location_is_extracted_from_root_header() {
        let h = parse_header(
            b"Snapshot-Content-Location: http://example.com/page\r\n\
              Content-Type: multipart/related; boundary=\"B\"\r\n\r\n",
        )
        .unwrap();
        assert_eq!(
            h.snapshot_content_location.as_deref(),
            Some("http://example.com/page")
        );
    }

    #[test]
    fn date_header_parsed_into_creation_time() {
        let h = parse_header(b"Date: Fri, 1 Mar 2017 22:44:17 -0000\r\n\r\n").unwrap();
        assert_eq!(
            h.date,
            Some(UNIX_EPOCH + Duration::from_secs(1_488_408_257))
        );
    }

    #[test]
    fn content_type_mime_is_trimmed_and_case_preserved() {
        // The parsed MIME type preserves the original case (only whitespace-
        // trimmed); case-insensitivity is applied separately at comparison sites
        // (`is_multipart`, the `multipart/alternative` check).
        let p = ParsedContentType::parse("  TEXT/HTML ; charset=utf-8");
        assert_eq!(p.mime_type, "TEXT/HTML");
    }

    #[test]
    fn content_type_quoted_and_unquoted_param_values() {
        let quoted = ParsedContentType::parse("multipart/related; boundary=\"----=_Part_0\"");
        assert_eq!(quoted.param("boundary"), Some("----=_Part_0"));
        let unquoted = ParsedContentType::parse("multipart/related; boundary=----=_Part_0");
        assert_eq!(unquoted.param("boundary"), Some("----=_Part_0"));
    }

    #[test]
    fn content_type_skips_valueless_and_empty_segments() {
        let p = ParsedContentType::parse("text/html; ; junk; charset=utf-8;");
        assert_eq!(p.param("junk"), None);
        assert_eq!(p.param("charset"), Some("utf-8"));
    }

    #[test]
    fn encoding_tokens_map_to_variants() {
        assert_eq!(
            parse_content_transfer_encoding("base64"),
            TransferEncoding::Base64
        );
        assert_eq!(
            parse_content_transfer_encoding("  Quoted-Printable \t"),
            TransferEncoding::QuotedPrintable
        );
        assert_eq!(
            parse_content_transfer_encoding("8BIT"),
            TransferEncoding::EightBit
        );
        assert_eq!(
            parse_content_transfer_encoding("7bit"),
            TransferEncoding::SevenBit
        );
        assert_eq!(
            parse_content_transfer_encoding("binary"),
            TransferEncoding::Binary
        );
    }

    #[test]
    fn unknown_and_empty_encoding_are_unknown() {
        assert_eq!(
            parse_content_transfer_encoding("uuencode"),
            TransferEncoding::Unknown
        );
        assert_eq!(
            parse_content_transfer_encoding(""),
            TransferEncoding::Unknown
        );
    }

    #[test]
    fn valid_date_one_digit_day_and_minus_zero_zone() {
        // "Fri, 1 Mar 2017 22:44:17 -0000" == unix 1_488_408_257.
        assert_eq!(
            parse_date("Fri, 1 Mar 2017 22:44:17 -0000"),
            Some(UNIX_EPOCH + Duration::from_secs(1_488_408_257)),
        );
    }

    #[test]
    fn invalid_date_garbage_is_none() {
        assert_eq!(parse_date("123xyz"), None);
    }

    #[test]
    fn overflowed_date_is_none() {
        assert_eq!(parse_date("May1 922372"), None);
    }

    #[test]
    fn overflowed_day_is_none() {
        assert_eq!(parse_date("94/3/933720368547"), None);
    }
}
