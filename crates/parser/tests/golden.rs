//! Golden acceptance suite: a faithful Rust port of the reference parser's
//! test suite, plus the content-ID-to-URI conversion cases.
//!
//! Fidelity rules honoured here:
//! - Each reference test input is reproduced byte-for-byte, one adjacent
//!   string literal per `b"..."` slice (adjacent literals concatenate with NO
//!   separator, so several parts are deliberately joined without a CRLF).
//! - Embedded NULs in `"bin\0ary"` are written `\x00`.
//! - The reference tests parse a NUL-terminated C string, so the trailing NUL
//!   is part of the parsed input — each input therefore ends with an appended
//!   `b"\x00"`.
//!
//! Reference -> Rust mapping:
//!   resources[i]->Url()        -> part.content_location (Option<String>)
//!   resources[i]->ContentID()  -> part.content_id       (None when IsNull)
//!   resources[i]->MimeType()   -> part.content_type
//!   resources[i]->TextEncoding()-> part.charset         (None when IsNull)
//!   resources[i]->Data()       -> part.body
//!   parser.CreationDate()      -> archive.creation_date()
//!   ParseArchive()             -> archive.parse_all()
//!
//! Documented divergences from the reference (all golden cases still agree):
//! - Failure is reported as a typed `Err`, not the reference's empty resource
//!   vector (see `missing_boundary`).
//! - `Date` parsing accepts RFC 2822 only; the reference parser is
//!   looser. The three bad dates are still rejected and the one valid date
//!   still accepted (see the `date_parsing_*` / `overflowed_*` tests).

use std::time::{Duration, UNIX_EPOCH};

use mhtml::{Archive, content_id_to_cid_url};

/// Concatenate byte-string slices into a single owned `Vec<u8>`, preserving
/// every byte exactly (including embedded NULs).
macro_rules! mhtml_bytes {
    ($($piece:expr),+ $(,)?) => {{
        let mut buf: Vec<u8> = Vec::new();
        $(buf.extend_from_slice($piece);)+
        buf
    }};
}

#[test]
fn mhtml_part_headers() {
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Transfer-Encoding: quoted-printable\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"single line\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-ID: <foo-123@mhtml.blink>\r\n",
        b"Content-Transfer-Encoding: binary\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page3\r\n",
        b"Content-Transfer-Encoding: base64\r\n",
        b"Content-Type: text/css; charset=ascii\r\n",
        b"\r\n",
        b"MTIzYWJj\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 3);

    assert_eq!(
        parts[0].content_location.as_deref(),
        Some("http://www.example.com/page1")
    );
    assert_eq!(parts[0].content_id, None);
    assert_eq!(parts[0].content_type, "text/html");
    assert_eq!(parts[0].charset.as_deref(), Some("utf-8"));

    assert_eq!(
        parts[1].content_location.as_deref(),
        Some("http://www.example.com/page2")
    );
    assert_eq!(
        parts[1].content_id.as_deref(),
        Some("<foo-123@mhtml.blink>")
    );
    assert_eq!(parts[1].content_type, "text/plain");
    assert_eq!(parts[1].charset, None);

    assert_eq!(
        parts[2].content_location.as_deref(),
        Some("http://www.example.com/page3")
    );
    assert_eq!(parts[2].content_id, None);
    assert_eq!(parts[2].content_type, "text/css");
    assert_eq!(parts[2].charset.as_deref(), Some("ascii"));
}

#[test]
fn quoted_printable_content_transfer_encoding() {
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Transfer-Encoding: quoted-printable\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"single line\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: quoted-printable\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"long line=3Dbar=3D=\r\n",
        b"more\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page3\r\n",
        b"Content-Transfer-Encoding: quoted-printable\r\n",
        b"Content-Type: text/css; charset=ascii\r\n",
        b"\r\n",
        b"first line\r\n",
        b"second line\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 3);

    assert_eq!(parts[0].body, b"single line\r\n".to_vec());
    assert_eq!(parts[1].body, b"long line=bar=more\r\n".to_vec());
    assert_eq!(parts[2].body, b"first line\r\nsecond line\r\n\r\n".to_vec());
}

#[test]
fn base64_content_transfer_encoding() {
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Transfer-Encoding: base64\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"MTIzYWJj\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: base64\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"MTIzYWJj\r\n",
        b"AQIDDQ4P\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 2);

    assert_eq!(parts[0].body, b"123abc".to_vec());
    assert_eq!(parts[1].body, b"123abc\x01\x02\x03\x0d\x0e\x0f".to_vec());
}

#[test]
fn eight_bit_content_transfer_encoding() {
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: 8bit\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"123\r\n",
        b"bin\x00ary\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 1);

    assert_eq!(parts[0].body, b"123bin\x00ary".to_vec());
}

#[test]
fn seven_bit_content_transfer_encoding() {
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"123\r\n",
        b"abcdefg\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 1);

    assert_eq!(parts[0].body, b"123abcdefg".to_vec());
}

#[test]
fn space_as_header_continuation() {
    // The `boundary` parameter is folded onto a continuation line that begins
    // with a single space (rather than a tab) — still a valid header fold.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b" boundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/html; charset=utf-8\r\n",
        b"\r\n",
        b"123\r\n",
        b"abcdefg\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 1);

    assert_eq!(parts[0].body, b"123abcdefg".to_vec());
}

#[test]
fn binary_content_transfer_encoding() {
    // Part 2's body ("bin\0ary") is joined to the next boundary with NO CRLF —
    // reproduce the C++ adjacent-literal concatenation exactly.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Transfer-Encoding: binary\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: binary\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page3\r\n",
        b"Content-Transfer-Encoding: binary\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 3);

    assert_eq!(parts[0].body, b"bin\x00ary".to_vec());
    assert_eq!(parts[1].body, b"bin\x00ary".to_vec());
    assert!(parts[2].body.is_empty());
}

#[test]
fn unknown_content_transfer_encoding() {
    // Unknown encoding is treated as binary.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Transfer-Encoding: foo\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Transfer-Encoding: unknown\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page3\r\n",
        b"Content-Transfer-Encoding: \r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 3);

    assert_eq!(parts[0].body, b"bin\x00ary".to_vec());
    assert_eq!(parts[1].body, b"bin\x00ary".to_vec());
    assert!(parts[2].body.is_empty());
}

#[test]
fn no_content_transfer_encoding() {
    // Missing encoding is treated as binary.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page2\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page3\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert_eq!(parts.len(), 3);

    assert_eq!(parts[0].body, b"bin\x00ary".to_vec());
    assert_eq!(parts[1].body, b"bin\x00ary".to_vec());
    assert!(parts[2].body.is_empty());
}

#[test]
fn date_parsing_empty_date() {
    // Missing date is ignored.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert!(!parts.is_empty());

    // No `Date` header should yield no creation date.
    assert_eq!(archive.creation_date(), None);
}

#[test]
fn date_parsing_invalid_date() {
    // Invalid archive date is ignored. Also, a `Date` header *within a part*
    // must not be used as the archive creation date.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"Date: 123xyz\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"Date: Fri, 1 Mar 2017 22:44:17 -0000\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert!(!parts.is_empty());

    // Invalid archive `Date` (and the part `Date`) should yield no creation date.
    assert_eq!(archive.creation_date(), None);
}

#[test]
fn date_parsing_valid_date() {
    // Valid archive-level date is used: Fri, 1 Mar 2017 22:44:17 UTC.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"Date: Fri, 1 Mar 2017 22:44:17 -0000\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert!(!parts.is_empty());

    // 2017-03-01 22:44:17 UTC == unix 1_488_408_257.
    assert_eq!(
        archive.creation_date(),
        Some(UNIX_EPOCH + Duration::from_secs(1_488_408_257))
    );
}

#[test]
fn missing_boundary() {
    // No `boundary` parameter in a multipart content type: the reference
    // returns an empty resource vector. DIVERGENCE: we surface this as a typed
    // `Err`.
    let data = mhtml_bytes![b"Content-Type: multipart/false\r\n", b"\x00"];

    assert!(Archive::parse(&data).is_err());
}

#[test]
fn overflowed_date() {
    // A malformed/overflowing `Date` must be ignored (no creation date).
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"Date:May1 922372\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert!(!parts.is_empty());

    assert_eq!(archive.creation_date(), None);
}

#[test]
fn overflowed_day() {
    // A malformed/overflowing `Date` day must be ignored (no creation date).
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Subject: Test Subject\r\n",
        b"Date:94/3/933720368547\r\n",
        b"MIME-Version: 1.0\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\ttype=\"text/html\";\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://www.example.com/page1\r\n",
        b"Content-Type: binary/octet-stream\r\n",
        b"\r\n",
        b"bin\x00ary\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive should parse");
    let parts = archive.parse_all().expect("all parts should decode");
    assert!(!parts.is_empty());

    assert_eq!(archive.creation_date(), None);
}

#[test]
fn content_id_to_cid_url_conversions() {
    // Port of `MHTMLParser::ConvertContentIDToURI` (mhtml_parser.cc) and its
    // header documentation: "<foo@bar.com>" -> "cid:foo@bar.com".
    assert_eq!(
        content_id_to_cid_url("<foo@bar.com>").as_deref(),
        Some("cid:foo@bar.com")
    );
    // length <= 2 -> None.
    assert_eq!(content_id_to_cid_url(""), None);
    assert_eq!(content_id_to_cid_url("<>"), None);
    // not wrapped in <...> -> None.
    assert_eq!(content_id_to_cid_url("foo@bar"), None);
    assert_eq!(content_id_to_cid_url("<foo"), None);
}
