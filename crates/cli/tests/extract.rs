//! End-to-end tests for `mhtml extract`. Fixture A is a hand-authored
//! `multipart/related` archive (exact CRLFs, a realistic archive-header/part
//! shape) exercising absolute/relative/`cid:`
//! references, a `<base>` tag, a `srcset`, CSS `url()`/`@import`, and a
//! Content-ID frame with no Content-Location. Fixture B truncates it mid-part.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mhtml_cli::extract::{self, Outcome};

const BOUNDARY: &str = "BoUnDaRy_A";

/// A 1x1 transparent PNG, base64-encoded (decodes to a valid PNG starting with
/// the `\x89PNG` signature).
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// The root HTML document (the decoded body). Every `=` is followed by `"`, a
/// non-hex byte, so quoted-printable decoding passes it through verbatim.
const ROOT_HTML: &[&str] = &[
    "<!DOCTYPE html>",
    "<html>",
    "<head>",
    "<base href=\"https://example.com/blog/\">",
    "<link rel=\"stylesheet\" href=\"/style/main.css\">",
    "</head>",
    "<body>",
    "<img src=\"https://example.com/img/logo.png\">",
    "<img src=\"../img/logo.png\">",
    "<img srcset=\"../img/logo.png 1x, https://example.com/img/logo.png 2x\">",
    "<iframe src=\"cid:frame-1@mhtml.test\"></iframe>",
    "<a href=\"https://external.example.org/page\">external</a>",
    "</body>",
    "</html>",
];

const MAIN_CSS: &[&str] = &[
    "@import \"https://cdn.example.net/reset.css\";",
    ".logo { background: url(../img/logo.png); }",
];

/// Join lines with CRLF and a trailing CRLF (matching how a text part body sits
/// between its blank header line and the following boundary).
fn body(lines: &[&str]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        s.push_str("\r\n");
    }
    s
}

/// Assemble Fixture A as raw archive bytes (all ASCII).
fn fixture_a() -> Vec<u8> {
    let mut s = String::new();
    // Archive header (realistic archive-header shape).
    s.push_str("From: <Saved by Blink>\r\n");
    s.push_str("Snapshot-Content-Location: https://example.com/blog/post\r\n");
    s.push_str("Subject: Fixture A\r\n");
    s.push_str("Date: Fri, 1 Mar 2017 22:44:17 -0000\r\n");
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str("Content-Type: multipart/related;\r\n");
    s.push_str("\ttype=\"text/html\";\r\n");
    s.push_str(&format!("\tboundary=\"{BOUNDARY}\"\r\n"));
    s.push_str("\r\n");

    // Part 1: root HTML (quoted-printable).
    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: text/html; charset=utf-8\r\n");
    s.push_str("Content-Transfer-Encoding: quoted-printable\r\n");
    s.push_str("Content-Location: https://example.com/blog/post\r\n");
    s.push_str("\r\n");
    s.push_str(&body(ROOT_HTML));

    // Part 2: stylesheet (quoted-printable).
    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: text/css\r\n");
    s.push_str("Content-Transfer-Encoding: quoted-printable\r\n");
    s.push_str("Content-Location: https://example.com/style/main.css\r\n");
    s.push_str("\r\n");
    s.push_str(&body(MAIN_CSS));

    // Part 3: image (base64).
    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: image/png\r\n");
    s.push_str("Content-Transfer-Encoding: base64\r\n");
    s.push_str("Content-Location: https://example.com/img/logo.png\r\n");
    s.push_str("\r\n");
    s.push_str(PNG_B64);
    s.push_str("\r\n");

    // Part 4: a frame identified only by Content-ID (no Content-Location).
    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: text/html\r\n");
    s.push_str("Content-ID: <frame-1@mhtml.test>\r\n");
    s.push_str("Content-Transfer-Encoding: quoted-printable\r\n");
    s.push_str("\r\n");
    s.push_str(&body(&["<p>frame</p>"]));

    // Closing boundary.
    s.push_str(&format!("--{BOUNDARY}--\r\n"));
    s.into_bytes()
}

/// Fixture B: Fixture A chopped inside the final (Content-ID) frame part, so
/// parts 1-3 parse cleanly but the frame reaches EOF with no closing boundary.
fn fixture_b() -> Vec<u8> {
    let full = fixture_a();
    let marker = b"<p>frame</p>";
    let pos = full
        .windows(marker.len())
        .position(|w| w == marker)
        .expect("frame marker present");
    // Keep only a partial frame body ("<p>fr"), dropping the closing boundary.
    full[..pos + 5].to_vec()
}

/// A base64 image part with the given Content-Location.
fn png_part(location: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: image/png\r\n");
    s.push_str("Content-Transfer-Encoding: base64\r\n");
    s.push_str(&format!("Content-Location: {location}\r\n"));
    s.push_str("\r\n");
    s.push_str(PNG_B64);
    s.push_str("\r\n");
    s
}

/// A well-formed archive whose parts force a file-vs-directory output-path
/// collision: `http://h/a.png` is written as a regular file, then
/// `http://h/a.png/b.png` needs `a.png` as a directory. A later image and an
/// entry document follow so we can prove extraction continues past the clash.
fn fixture_collision() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("From: <Saved by Blink>\r\n");
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str("Content-Type: multipart/related;\r\n");
    s.push_str(&format!("\tboundary=\"{BOUNDARY}\"\r\n"));
    s.push_str("\r\n");

    s.push_str(&png_part("http://h/a.png"));
    s.push_str(&png_part("http://h/a.png/b.png"));
    s.push_str(&png_part("http://h/later.png"));

    s.push_str(&format!("--{BOUNDARY}\r\n"));
    s.push_str("Content-Type: text/html\r\n");
    s.push_str("Content-Transfer-Encoding: quoted-printable\r\n");
    s.push_str("Content-Location: http://h/index.html\r\n");
    s.push_str("\r\n");
    s.push_str(&body(&["<p>entry</p>"]));

    s.push_str(&format!("--{BOUNDARY}--\r\n"));
    s.into_bytes()
}

/// Every file under `root`, as forward-slash paths relative to it.
fn tree(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let rel = path.strip_prefix(base).unwrap();
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

fn write_input(dir: &Path, bytes: &[u8]) -> PathBuf {
    let input = dir.join("archive.mhtml");
    std::fs::write(&input, bytes).unwrap();
    input
}

#[test]
fn extracts_fixture_a_into_the_expected_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_a());
    let out = tmp.path().join("out");

    let outcome =
        extract::run(&input, &out, false, extract::Naming::Mirror, None).expect("extract");
    assert!(matches!(outcome, Outcome::Success));

    let expected: BTreeSet<String> = [
        "_cid/_frame-1@mhtml.test_.html",
        "example.com/blog/post.html",
        "example.com/img/logo.png",
        "example.com/style/main.css",
        "index.html",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    assert_eq!(tree(&out), expected);
}

fn read(path: &Path) -> String {
    String::from_utf8(std::fs::read(path).unwrap()).unwrap()
}

/// Pull every attribute value for `attr` out of `html` (naive `attr="..."`
/// scan, sufficient for the hand-authored fixture).
fn attr_values<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let needle = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(&needle) {
        let after = &rest[i + needle.len()..];
        let end = after.find('"').unwrap_or(after.len());
        out.push(&after[..end]);
        rest = &after[end..];
    }
    out
}

/// Every URL-ish reference in the rewritten HTML: `src`, `href`, and each
/// candidate URL of every `srcset`.
fn html_refs(html: &str) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for attr in ["src", "href"] {
        refs.extend(attr_values(html, attr).into_iter().map(String::from));
    }
    for set in attr_values(html, "srcset") {
        for candidate in set.split(',') {
            if let Some(url) = candidate.split_whitespace().next() {
                refs.push(url.to_string());
            }
        }
    }
    refs
}

/// Every URL-ish reference in the rewritten CSS: `url(...)` bodies and bare
/// `@import "..."` targets.
fn css_refs(css: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = css;
    while let Some(i) = rest.find("url(") {
        let after = &rest[i + 4..];
        let end = after.find(')').unwrap_or(after.len());
        refs.push(after[..end].trim().trim_matches(['"', '\'']).to_string());
        rest = &after[end..];
    }
    rest = css;
    while let Some(i) = rest.find("@import \"") {
        let after = &rest[i + 9..];
        let end = after.find('"').unwrap_or(after.len());
        refs.push(after[..end].to_string());
        rest = &after[end..];
    }
    refs
}

/// Assert every reference either targets an existing extracted file (resolved
/// relative to the referencing file's directory) or is an absolute external URL.
fn assert_refs_resolve(refs: &[String], from_dir: &Path) {
    for r in refs {
        if r.contains("://") {
            continue; // intentionally-unmapped external
        }
        assert!(
            !r.starts_with("cid:"),
            "cid reference left unrewritten: {r}"
        );
        let target = from_dir.join(r);
        assert!(
            target.exists(),
            "reference {r} (from {}) does not resolve to an extracted file: {}",
            from_dir.display(),
            target.display()
        );
    }
}

#[test]
fn rewrites_every_reference_to_an_extracted_file_or_external() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_a());
    let out = tmp.path().join("out");
    extract::run(&input, &out, false, extract::Naming::Mirror, None).expect("extract");

    let post = read(&out.join("example.com/blog/post.html"));
    let css = read(&out.join("example.com/style/main.css"));
    let index = read(&out.join("index.html"));

    // Absolute and cid references rewritten to relative disk paths; the <base>
    // is neutralized so no live example.com URL survives; the external stays.
    assert!(post.contains("<base>"), "base not neutralized: {post}");
    assert!(
        !post.contains("example.com"),
        "internal URL survived: {post}"
    );
    assert!(post.contains("href=\"../style/main.css\""));
    assert!(post.contains("src=\"../img/logo.png\""));
    assert!(post.contains("srcset=\"../img/logo.png 1x, ../img/logo.png 2x\""));
    assert!(post.contains("src=\"../../_cid/_frame-1@mhtml.test_.html\""));
    assert!(post.contains("https://external.example.org/page"));

    // CSS url() rewritten; the absolute @import left as an external.
    assert!(css.contains("url(\"../img/logo.png\")"));
    assert!(css.contains("@import \"https://cdn.example.net/reset.css\""));

    // The relative img (../img/logo.png) from example.com/blog reaches the file
    // on disk, and so does every other reference in every rewritten document.
    assert_refs_resolve(&html_refs(&post), &out.join("example.com/blog"));
    assert_refs_resolve(&css_refs(&css), &out.join("example.com/style"));
    assert_refs_resolve(&html_refs(&index), &out);
}

#[test]
fn truncated_archive_salvages_earlier_parts_in_default_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_b());
    let out = tmp.path().join("out");

    let outcome =
        extract::run(&input, &out, false, extract::Naming::Mirror, None).expect("extract");
    assert!(matches!(outcome, Outcome::Failed));

    // Parts 1-3 (root, css, image) plus the entry redirect are salvaged; the
    // truncated frame part is not written.
    for salvaged in [
        "example.com/blog/post.html",
        "example.com/style/main.css",
        "example.com/img/logo.png",
        "index.html",
    ] {
        assert!(out.join(salvaged).exists(), "missing salvaged {salvaged}");
    }
    assert!(
        !out.join("_cid").exists(),
        "the truncated frame part must not be extracted"
    );
}

#[test]
fn file_vs_directory_collision_is_salvaged_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_collision());
    let out = tmp.path().join("out");

    // The collision between h/a.png (a file) and h/a.png/b.png (which would
    // need a.png to be a directory) must not abort the whole extract with a
    // raw OS error; it is treated like an unsalvageable part.
    let outcome = extract::run(&input, &out, false, extract::Naming::Mirror, None)
        .expect("extract must not error out");
    assert!(matches!(outcome, Outcome::Failed));

    // Every writable part plus the entry redirect is still salvaged.
    for salvaged in ["h/a.png", "h/later.png", "h/index.html", "index.html"] {
        assert!(out.join(salvaged).exists(), "missing salvaged {salvaged}");
    }
    // The colliding part is skipped, never written.
    assert!(!out.join("h/a.png/b.png").exists());
}

#[test]
fn truncated_archive_in_strict_mode_removes_the_output_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_b());
    let out = tmp.path().join("out");

    let outcome = extract::run(&input, &out, true, extract::Naming::Mirror, None).expect("extract");
    assert!(matches!(outcome, Outcome::Failed));
    assert!(
        !out.exists(),
        "strict mode must leave no output dir behind: {}",
        out.display()
    );
}

#[test]
fn strict_mode_refuses_to_delete_a_preexisting_output_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_b());
    // A pre-existing (empty) output dir the tool did not create this run.
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();

    // Strict cleanup must refuse (error) rather than delete a dir it did not
    // create; the salvaged files are left in place.
    let result = extract::run(&input, &out, true, extract::Naming::Mirror, None);
    assert!(result.is_err(), "expected a refusal error");
    assert!(out.exists(), "the pre-existing dir must not be deleted");
    assert!(out.join("example.com/img/logo.png").exists());
}

fn run_bin(args: &[&std::ffi::OsStr]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_mhtml"))
        .args(args)
        .output()
        .expect("running the mhtml binary")
}

#[test]
fn refuses_to_extract_into_a_non_empty_directory() {
    use std::ffi::OsStr;
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_a());
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("existing.txt"), b"keep me").unwrap();

    let output = run_bin(&[
        OsStr::new("extract"),
        input.as_os_str(),
        OsStr::new("-o"),
        out.as_os_str(),
    ]);
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not empty"), "stderr: {stderr}");
    assert!(stderr.contains("-o"), "stderr: {stderr}");
    // The pre-existing file is untouched.
    assert_eq!(read(&out.join("existing.txt")), "keep me");
}

#[test]
fn binary_extracts_and_prints_the_entry_path() {
    use std::ffi::OsStr;
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), &fixture_a());
    let out = tmp.path().join("out");

    let output = run_bin(&[
        OsStr::new("extract"),
        input.as_os_str(),
        OsStr::new("-o"),
        out.as_os_str(),
    ]);
    assert!(output.status.success(), "expected success: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("index.html"), "stdout: {stdout}");
    assert!(out.join("example.com/blog/post.html").exists());
}

/// A minimal archive whose HTML entry references a stylesheet and an image by
/// relative URL, all under one host. Exercises the content-hash flow.
const HASH_ARCHIVE: &[u8] = b"\
From: <Saved by Blink>\r\n\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: 7bit\r\n\
Content-Location: http://example.com/\r\n\
\r\n\
<html><head><title>t</title></head><body>\
<link rel=\"stylesheet\" href=\"style.css\"><img src=\"logo.png\"></body></html>\r\n\
--B\r\n\
Content-Type: text/css\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Location: http://example.com/style.css\r\n\
\r\n\
MTIzYWJj\r\n\
--B\r\n\
Content-Type: image/png\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Location: http://example.com/logo.png\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--B--\r\n";

/// Pull every `"key": "..."` value out of a manifest.json blob (naive scan,
/// sufficient for the hand-authored manifest we emit).
fn manifest_keys(json: &str) -> Vec<String> {
    let needle = "\"key\": \"";
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(needle) {
        let after = &rest[i + needle.len()..];
        let end = after.find('"').unwrap_or(after.len());
        out.push(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

#[test]
fn hash_extract_writes_flat_files_manifest_and_rewritten_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), HASH_ARCHIVE);
    let out = tmp.path().join("out");

    let outcome = extract::run(&input, &out, false, extract::Naming::Hash, None).expect("extract");
    assert!(matches!(outcome, Outcome::Success));

    // manifest.json lists all three resources by hash key, with original URLs.
    let manifest = read(&out.join("manifest.json"));
    let keys = manifest_keys(&manifest);
    assert_eq!(keys.len(), 3, "manifest: {manifest}");
    assert!(
        manifest.contains("http://example.com/style.css"),
        "{manifest}"
    );
    assert!(
        manifest.contains("http://example.com/logo.png"),
        "{manifest}"
    );

    // Every hash key is a flat "<64-hex>.<ext>" file that exists on disk.
    for key in &keys {
        assert!(!key.contains('/'), "key not flat: {key}");
        assert!(out.join(key).exists(), "missing hash file {key}");
        let (hash, ext) = key.split_once('.').expect("hash.ext");
        assert_eq!(hash.len(), 64, "sha256 hex length: {key}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "hex: {key}");
        assert!(!ext.is_empty(), "extension: {key}");
    }

    // The rewritten entry references resources by hash key, never by raw URL.
    let index = read(&out.join("index.html"));
    assert!(
        !index.contains("http://example.com"),
        "raw URL survived: {index}"
    );
    let png_key = keys.iter().find(|k| k.ends_with(".png")).expect("png key");
    let css_key = keys.iter().find(|k| k.ends_with(".css")).expect("css key");
    assert!(index.contains(&format!("src=\"{png_key}\"")), "{index}");
    assert!(index.contains(&format!("href=\"{css_key}\"")), "{index}");
    // With no --base-href, the entry carries no <base href>.
    assert!(!index.contains("<base href"), "unexpected base: {index}");
}

#[test]
fn hash_extract_with_base_href_sets_base_on_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), HASH_ARCHIVE);
    let out = tmp.path().join("out");
    extract::run(
        &input,
        &out,
        false,
        extract::Naming::Hash,
        Some("https://cdn.example/"),
    )
    .expect("extract");

    let index = read(&out.join("index.html"));
    assert!(
        index.contains("<base href=\"https://cdn.example/\">"),
        "{index}"
    );
    // References stay bare relative hash keys (they resolve against <base>).
    let manifest = read(&out.join("manifest.json"));
    let png_key = manifest_keys(&manifest)
        .into_iter()
        .find(|k| k.ends_with(".png"))
        .expect("png key");
    assert!(index.contains(&format!("src=\"{png_key}\"")), "{index}");
}

#[test]
fn mirror_mode_rejects_base_href() {
    let tmp = tempfile::tempdir().unwrap();
    let input = write_input(tmp.path(), HASH_ARCHIVE);
    let out = tmp.path().join("out");
    let result = extract::run(
        &input,
        &out,
        false,
        extract::Naming::Mirror,
        Some("https://cdn.example/"),
    );
    assert!(result.is_err(), "mirror mode must reject --base-href");
}
