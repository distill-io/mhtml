//! Public-API behaviours beyond the golden suite: the
//! IE-style non-multipart root, `multipart/alternative` nesting/flattening,
//! the `Snapshot-Content-Location` accessor, and the lenient iterator's
//! terminal-error contract. These exercise parser paths that the golden
//! suite happens not to cover.

use mhtml::{Archive, TransferEncoding};

macro_rules! mhtml_bytes {
    ($($piece:expr),+ $(,)?) => {{
        let mut buf: Vec<u8> = Vec::new();
        $(buf.extend_from_slice($piece);)+
        buf
    }};
}

#[test]
fn non_multipart_root_is_a_single_part() {
    // IE saves a resourceless page as a plain (non-multipart) MIME message: the
    // whole body is one part described by the root header itself.
    let data = mhtml_bytes![
        b"Content-Type: text/html\r\n",
        b"Content-Location: http://example.com/page\r\n",
        b"Content-Transfer-Encoding: quoted-printable\r\n",
        b"\r\n",
        b"Hello=20World\r\n",
    ];

    let archive = Archive::parse(&data).expect("archive parses");
    let parts = archive.parse_all().expect("single part decodes");
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].content_type, "text/html");
    assert_eq!(
        parts[0].content_location.as_deref(),
        Some("http://example.com/page")
    );
    assert_eq!(parts[0].charset, None);
    assert_eq!(parts[0].body, b"Hello World\r\n".to_vec());
}

#[test]
fn multipart_alternative_nesting_is_flattened() {
    // IE nesting: a `multipart/alternative` sub-document whose parts are
    // flattened into the single top-level stream (ParseArchiveWithHeader
    // recursion), then parsing resumes after the parent boundary.
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\tboundary=\"OUTER\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--OUTER\r\n",
        b"Content-Type: multipart/alternative;\r\n",
        b"\tboundary=\"INNER\"\r\n",
        b"\r\n",
        b"inner-preamble\r\n",
        b"--INNER\r\n",
        b"Content-Location: http://example.com/a\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/html\r\n",
        b"\r\n",
        b"AAA\r\n",
        b"--INNER\r\n",
        b"Content-Location: http://example.com/b\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"BBB\r\n",
        b"--INNER--\r\n",
        b"--OUTER\r\n",
        b"Content-Location: http://example.com/c\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/css\r\n",
        b"\r\n",
        b"CCC\r\n",
        b"--OUTER--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive parses");
    let parts = archive.parse_all().expect("all parts decode");

    let locations: Vec<_> = parts
        .iter()
        .map(|p| p.content_location.as_deref().unwrap())
        .collect();
    assert_eq!(
        locations,
        vec![
            "http://example.com/a",
            "http://example.com/b",
            "http://example.com/c",
        ]
    );
    assert_eq!(parts[0].body, b"AAA".to_vec());
    assert_eq!(parts[1].body, b"BBB".to_vec());
    assert_eq!(parts[2].body, b"CCC".to_vec());
}

#[test]
fn snapshot_content_location_is_exposed_from_root() {
    let data = mhtml_bytes![
        b"From: <Saved by Blink>\r\n",
        b"Snapshot-Content-Location: http://example.com/saved-page\r\n",
        b"Content-Type: multipart/related;\r\n",
        b"\tboundary=\"BoUnDaRy\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--BoUnDaRy\r\n",
        b"Content-Location: http://example.com/page1\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/html\r\n",
        b"\r\n",
        b"body\r\n",
        b"--BoUnDaRy--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive parses");
    assert_eq!(
        archive.snapshot_content_location(),
        Some("http://example.com/saved-page")
    );
}

#[test]
fn snapshot_content_location_is_none_when_absent() {
    let data = mhtml_bytes![
        b"Content-Type: multipart/related; boundary=\"B\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--B\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/html\r\n",
        b"\r\n",
        b"x\r\n",
        b"--B--\r\n",
        b"\x00",
    ];

    let archive = Archive::parse(&data).expect("archive parses");
    assert_eq!(archive.snapshot_content_location(), None);
}

#[test]
fn iterator_yields_ok_until_corruption_then_one_err_then_none() {
    // The second part is never terminated by a boundary (EOF first): the
    // iterator yields the good part, then exactly one `Err`, then `None`
    // forever. Already-yielded parts stand.
    let data = mhtml_bytes![
        b"Content-Type: multipart/related; boundary=\"B\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--B\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"good\r\n",
        b"--B\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"no closing boundary here",
    ];

    let archive = Archive::parse(&data).expect("archive parses");
    let mut it = archive.parts();

    let first = it.next().expect("a first item").expect("first is Ok");
    assert_eq!(first.transfer_encoding, TransferEncoding::SevenBit);
    assert_eq!(first.body().unwrap().as_ref(), b"good");

    assert!(it.next().expect("a second item").is_err());
    assert!(it.next().is_none());
    assert!(it.next().is_none());
}

#[test]
fn parse_all_is_all_or_nothing_on_corruption() {
    // The same corrupt archive: `parse_all` fails the whole thing (the
    // reference's empty-vector all-or-nothing).
    let data = mhtml_bytes![
        b"Content-Type: multipart/related; boundary=\"B\"\r\n",
        b"\r\n",
        b"\r\n",
        b"--B\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"good\r\n",
        b"--B\r\n",
        b"Content-Transfer-Encoding: 7bit\r\n",
        b"Content-Type: text/plain\r\n",
        b"\r\n",
        b"no closing boundary here",
    ];

    let archive = Archive::parse(&data).expect("archive parses");
    assert!(archive.parse_all().is_err());
}
