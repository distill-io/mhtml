//! End-to-end tests for `mhtml list` against hand-authored fixtures.

use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run_list(fixture_name: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mhtml"))
        .arg("list")
        .arg(fixture(fixture_name))
        .output()
        .expect("running the mhtml binary")
}

#[test]
fn lists_every_part_of_a_valid_archive() {
    let out = run_list("simple.mhtml");
    assert!(
        out.status.success(),
        "expected success, got {:?}",
        out.status
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 rows:\n{stdout}");

    assert!(lines[0].contains("text/html"));
    assert!(lines[0].contains("quoted-printable"));
    assert!(lines[0].contains("http://example.com/"));

    assert!(lines[1].contains("text/css"));
    assert!(lines[1].contains("base64"));
    assert!(lines[1].contains("http://example.com/style.css"));

    assert!(lines[2].contains("image/png"));
    assert!(lines[2].contains("http://example.com/logo.png"));
}

#[test]
fn control_bytes_in_content_location_are_not_emitted_raw() {
    // Security regression: a hostile archive whose Content-Location carries ESC
    // (0x1b) bytes must not have them reach stdout, or it could inject ANSI /
    // terminal-escape sequences into the user's terminal.
    let mut archive = Vec::new();
    archive.extend_from_slice(b"From: <Saved by Blink>\r\n");
    archive.extend_from_slice(b"MIME-Version: 1.0\r\n");
    archive.extend_from_slice(b"Content-Type: multipart/related; boundary=\"B\"\r\n\r\n");
    archive.extend_from_slice(b"--B\r\n");
    archive.extend_from_slice(b"Content-Type: text/html\r\n");
    archive.extend_from_slice(b"Content-Transfer-Encoding: quoted-printable\r\n");
    archive.extend_from_slice(b"Content-Location: http://h/");
    archive.push(0x1b);
    archive.extend_from_slice(b"[31mPWNED");
    archive.push(0x1b);
    archive.extend_from_slice(b"[0m/x.html\r\n\r\n");
    archive.extend_from_slice(b"<p>x</p>\r\n");
    archive.extend_from_slice(b"--B--\r\n");

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("evil.mhtml");
    std::fs::write(&path, &archive).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mhtml"))
        .arg("list")
        .arg(&path)
        .output()
        .expect("running the mhtml binary");
    assert!(out.status.success(), "expected success: {out:?}");
    assert!(
        !out.stdout.contains(&0x1b),
        "raw ESC reached stdout: {:?}",
        out.stdout
    );
    // The (sanitized) location is still listed.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("http://h/"), "stdout: {stdout}");
}

#[test]
fn truncated_archive_lists_salvaged_parts_and_exits_nonzero() {
    let out = run_list("truncated.mhtml");
    assert!(!out.status.success(), "expected non-zero exit");

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "expected the one salvaged row:\n{stdout}");
    assert!(lines[0].contains("http://example.com/"));

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("warning"), "stderr: {stderr}");
    assert!(stderr.contains("boundary"), "stderr: {stderr}");
}
