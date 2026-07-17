//! The `mhtml list <file>` subcommand: one aligned row per part — index,
//! content-type, transfer-encoding, decoded size, and content-location (or
//! content-id, or `-`). Iteration is lenient: on a trailing parse error the
//! rows gathered so far are still printed and the command exits non-zero.

use std::path::Path;

use anyhow::Context;
use mhtml::{Archive, Part, TransferEncoding};

/// Human-readable transfer-encoding label.
fn encoding_str(enc: TransferEncoding) -> &'static str {
    match enc {
        TransferEncoding::QuotedPrintable => "quoted-printable",
        TransferEncoding::Base64 => "base64",
        TransferEncoding::SevenBit => "7bit",
        TransferEncoding::EightBit => "8bit",
        TransferEncoding::Binary => "binary",
        TransferEncoding::Unknown => "unknown",
    }
}

/// Format a byte count as a compact human size (`512 B`, `1.5 KB`, `1.0 MB`).
fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// One printable line of the listing.
struct Row {
    index: usize,
    content_type: String,
    encoding: String,
    size: String,
    location: String,
}

impl Row {
    fn new(index: usize, part: &Part, decoded_size: u64) -> Self {
        let location = part
            .content_location
            .clone()
            .or_else(|| part.content_id.clone())
            .unwrap_or_else(|| "-".to_string());
        Row {
            index,
            content_type: part.content_type.clone(),
            encoding: encoding_str(part.transfer_encoding).to_string(),
            size: human_size(decoded_size),
            location,
        }
    }
}

/// Replace control characters with the Unicode replacement character so a
/// hostile `Content-Location` cannot inject terminal escape sequences (ESC,
/// cursor moves, colour/title codes) when the listing is printed to a terminal.
fn sanitize_display(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// Render rows into an aligned, column-padded table (no trailing spaces on the
/// final column). Returns the empty string for no rows.
fn format_rows(rows: &[Row]) -> String {
    let (mut w_idx, mut w_ct, mut w_enc, mut w_size) = (0, 0, 0, 0);
    for r in rows {
        w_idx = w_idx.max(r.index.to_string().len());
        w_ct = w_ct.max(r.content_type.len());
        w_enc = w_enc.max(r.encoding.len());
        w_size = w_size.max(r.size.len());
    }
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!(
            "{:>w_idx$}  {:<w_ct$}  {:<w_enc$}  {:>w_size$}  {}\n",
            r.index,
            r.content_type,
            r.encoding,
            r.size,
            sanitize_display(&r.location),
        ));
    }
    out
}

/// Outcome of a listing run so the binary can choose an exit code.
pub enum Outcome {
    /// Every part was listed.
    Complete,
    /// A trailing iterator error stopped the walk; a warning was printed and
    /// the already-listed rows stand.
    Truncated,
}

/// Read `path`, parse it, and print the listing to stdout. A trailing parse
/// error is reported to stderr and reflected in [`Outcome::Truncated`]; a
/// failure to read the file or its root header is a hard `Err`.
pub fn run(path: &Path) -> anyhow::Result<Outcome> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let archive = Archive::parse(&data).context("parsing MHTML archive")?;

    let mut rows = Vec::new();
    let mut trailing = None;
    for (i, part) in archive.parts().enumerate() {
        match part {
            Ok(p) => {
                let size = p.body().map(|b| b.len() as u64).unwrap_or(0);
                rows.push(Row::new(i, &p, size));
            }
            Err(e) => {
                trailing = Some(e);
                break;
            }
        }
    }

    print!("{}", format_rows(&rows));

    match trailing {
        Some(e) => {
            eprintln!("warning: stopped listing after part {}: {e}", rows.len());
            Ok(Outcome::Truncated)
        }
        None => Ok(Outcome::Complete),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1048576), "1.0 MB");
    }

    #[test]
    fn encoding_labels() {
        assert_eq!(
            encoding_str(TransferEncoding::QuotedPrintable),
            "quoted-printable"
        );
        assert_eq!(encoding_str(TransferEncoding::Base64), "base64");
        assert_eq!(encoding_str(TransferEncoding::Binary), "binary");
    }

    fn row(index: usize, ct: &str, enc: &str, size: &str, loc: &str) -> Row {
        Row {
            index,
            content_type: ct.to_string(),
            encoding: enc.to_string(),
            size: size.to_string(),
            location: loc.to_string(),
        }
    }

    #[test]
    fn empty_rows_render_empty() {
        assert_eq!(format_rows(&[]), "");
    }

    #[test]
    fn control_bytes_in_location_are_sanitized() {
        // A hostile Content-Location carrying terminal-escape bytes (ESC = 0x1b)
        // must not reach the terminal verbatim — it would inject ANSI sequences.
        let rows = [row(
            0,
            "text/html",
            "7bit",
            "1 B",
            "http://h/\u{1b}[31mPWNED\u{1b}[0m/x.html",
        )];
        let out = format_rows(&rows);
        assert!(!out.contains('\u{1b}'), "raw ESC survived: {out:?}");
        assert!(out.contains("http://h/"), "location body lost: {out:?}");
    }

    #[test]
    fn rows_are_column_aligned() {
        let rows = [
            row(
                0,
                "text/html",
                "quoted-printable",
                "1.2 KB",
                "http://h/index.html",
            ),
            row(10, "image/png", "base64", "800 B", "http://h/logo.png"),
        ];
        let out = format_rows(&rows);
        let expected = concat!(
            " 0  text/html  quoted-printable  1.2 KB  http://h/index.html\n",
            "10  image/png  base64             800 B  http://h/logo.png\n",
        );
        assert_eq!(out, expected);
    }
}
