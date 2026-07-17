//! Pure path-mapping for extraction: turn a part's `Content-Location` (or
//! `Content-ID`, or its index) into a filesystem-relative output path that
//! mirrors the URL hierarchy so relative references resolve natively on disk.
//!
//! Everything here is deliberately side-effect free and heavily unit-tested;
//! the security invariant (a hostile `Content-Location` can never escape the
//! output root) lives at the bottom of the test module.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::Url;

/// Segments longer than this (in bytes) are truncated and disambiguated with a
/// hash, keeping paths comfortably under filesystem name limits.
const MAX_SEGMENT_BYTES: usize = 200;

/// FNV-1a 64-bit hash, returned as its low 32 bits in 8 lowercase hex digits.
///
/// Used only to disambiguate truncated segments and query strings, so a short,
/// deterministic, dependency-free digest is all that is needed.
fn fnv1a_hex8(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

/// True for Windows reserved device basenames (`CON`, `PRN`, `AUX`, `NUL`,
/// `COM1`–`COM9`, `LPT1`–`LPT9`), matched case-insensitively.
fn is_reserved(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    if matches!(u.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let b = u.as_bytes();
    (u.starts_with("COM") || u.starts_with("LPT"))
        && b.len() == 4
        && b[3].is_ascii_digit()
        && b[3] != b'0'
}

/// Sanitize a single decoded path segment so it is a safe, self-contained
/// filename component: forbidden/control characters become `_`, `.`/`..` are
/// neutralized, reserved device names are prefixed, and overlong segments are
/// truncated with a hash suffix. The result can never equal `.` or `..`, which
/// is the backbone of the traversal defense.
fn sanitize_segment(seg: &str) -> String {
    let mut s: String = seg
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();

    if s == "." {
        s = "_.".to_string();
    } else if s == ".." {
        s = "_..".to_string();
    } else {
        let stem = s.split('.').next().unwrap_or("");
        if is_reserved(stem) {
            s = format!("_{s}");
        }
    }

    if s.len() > MAX_SEGMENT_BYTES {
        let hash = fnv1a_hex8(seg.as_bytes());
        let mut cut = MAX_SEGMENT_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s = format!("{}-{}", &s[..cut], hash);
    }

    s
}

/// Map a `Content-Type` (parameters ignored) to a file extension, used only
/// when the URL path supplies none. Delegates to `mhtml_serve::naming::ext_for_mime`,
/// the single source of truth for this map.
fn infer_ext(content_type: &str) -> &'static str {
    mhtml_serve::naming::ext_for_mime(content_type)
}

/// For render-MIME-strict types, the canonical extension a browser needs to
/// apply the resource offline — browsers sniff CSS/JS by extension (not
/// content) when loaded from `file://`, so a mislabeled URL path (e.g.
/// `load.php` serving `text/css`) must be overridden. Images/fonts are sniffed
/// by magic bytes, so their URL extension is left alone. Delegates to
/// `mhtml_serve::naming::forced_ext`, the single source of truth.
fn forced_ext(content_type: &str) -> Option<&'static str> {
    mhtml_serve::naming::forced_ext(content_type)
}

/// Build the final filename from a raw (decoded, non-empty) last path segment:
/// sanitize it, keep or infer an extension, and splice in a `.q<hash>` marker
/// when the URL carried a query string.
fn build_filename(raw_name: &str, query: Option<&str>, content_type: &str) -> String {
    let sanitized = sanitize_segment(raw_name);

    let dot = sanitized
        .rfind('.')
        .filter(|&i| i > 0 && i < sanitized.len() - 1);
    let (stem, ext) = match dot {
        Some(i) => (sanitized[..i].to_string(), sanitized[i + 1..].to_string()),
        None => (sanitized.clone(), infer_ext(content_type).to_string()),
    };
    // A CSS/JS resource must carry its canonical extension regardless of the
    // URL path's (its stem is preserved for readability/dedup).
    let ext = match forced_ext(content_type) {
        Some(forced) => forced.to_string(),
        None => ext,
    };

    let mut name = stem;
    if let Some(q) = query {
        name.push_str(".q");
        name.push_str(&fnv1a_hex8(q.as_bytes()));
    }
    name.push('.');
    name.push_str(&ext);
    name
}

/// Assemble a relative path from a head directory (host or synthetic root),
/// the decoded path segments, and the query. An empty trailing segment (empty
/// path or trailing slash) yields `index.html`.
fn assemble(
    head: String,
    raw_segments: Vec<String>,
    query: Option<&str>,
    content_type: &str,
) -> PathBuf {
    let mut segs = raw_segments;
    let filename_raw = segs.pop().unwrap_or_default();

    let mut path = PathBuf::from(head);
    for dir in segs.into_iter().filter(|s| !s.is_empty()) {
        path.push(sanitize_segment(&dir));
    }

    let raw_name = if filename_raw.is_empty() {
        "index.html".to_string()
    } else {
        filename_raw
    };
    path.push(build_filename(&raw_name, query, content_type));
    path
}

/// The `<host>[_<port>]` head directory. Missing/empty hosts (e.g. `file://`)
/// fall back to `_nohost`; default ports are dropped by the `url` crate.
fn host_component(url: &Url) -> String {
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .map(sanitize_segment)
        .unwrap_or_else(|| "_nohost".to_string());
    match url.port() {
        Some(p) => format!("{host}_{p}"),
        None => host,
    }
}

/// Map a parsed URL to a relative path mirroring its hierarchy.
fn from_url(url: &Url, content_type: &str) -> PathBuf {
    let head = host_component(url);
    let segments: Vec<String> = match url.path_segments() {
        Some(it) => it
            .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
            .collect(),
        None => vec![
            percent_decode_str(url.path())
                .decode_utf8_lossy()
                .into_owned(),
        ],
    };
    assemble(head, segments, url.query(), content_type)
}

/// Fallback for a `Content-Location` the `url` crate cannot parse: treat the
/// raw string as a slash/backslash-separated path under `_nohost`. Every
/// segment is still sanitized, so the traversal invariant holds.
fn from_opaque(loc: &str, content_type: &str) -> PathBuf {
    let raw: Vec<String> = loc.split(['/', '\\']).map(str::to_string).collect();
    assemble("_nohost".to_string(), raw, None, content_type)
}

/// Map a `Content-Location` string to a relative output path.
fn from_location(loc: &str, content_type: &str) -> PathBuf {
    match Url::parse(loc) {
        Ok(url) => from_url(&url, content_type),
        Err(_) => from_opaque(loc, content_type),
    }
}

/// Tracks already-emitted output paths (compared case-insensitively, since the
/// target filesystem may be case-insensitive) so colliding parts receive `.1`,
/// `.2`, … suffixes in the order they are reserved.
#[derive(Default)]
pub struct Dedup {
    seen: HashSet<String>,
}

/// Insert `.n` before the final extension of `path`'s filename.
fn with_suffix(path: &Path, n: u32) -> PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let new_name = match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => format!("{}.{}.{}", &name[..i], n, &name[i + 1..]),
        None => format!("{name}.{n}"),
    };
    match path.parent() {
        Some(p) => p.join(new_name),
        None => PathBuf::from(new_name),
    }
}

impl Dedup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve `path`, returning it unchanged if free, otherwise the first
    /// `.n`-suffixed variant that is free (comparison is case-insensitive).
    fn reserve(&mut self, path: PathBuf) -> PathBuf {
        let key = path.to_string_lossy().to_lowercase();
        if self.seen.insert(key) {
            return path;
        }
        let mut n: u32 = 1;
        loop {
            let candidate = with_suffix(&path, n);
            let ckey = candidate.to_string_lossy().to_lowercase();
            if self.seen.insert(ckey) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Compute the relative output path for a part, deduplicating against paths
/// already reserved in `dedup`.
///
/// Precedence follows the plan: a `Content-Location` maps by URL hierarchy; if
/// absent, a `Content-ID` maps under `_cid/`; if neither, the part falls back
/// to `_parts/part-<index>`.
pub fn output_path(
    content_location: Option<&str>,
    content_id: Option<&str>,
    content_type: &str,
    index: usize,
    dedup: &mut Dedup,
) -> PathBuf {
    let rel = if let Some(loc) = content_location {
        from_location(loc, content_type)
    } else if let Some(id) = content_id {
        let ext = infer_ext(content_type);
        [
            "_cid".to_string(),
            format!("{}.{ext}", sanitize_segment(id)),
        ]
        .iter()
        .collect()
    } else {
        let ext = infer_ext(content_type);
        ["_parts".to_string(), format!("part-{index}.{ext}")]
            .iter()
            .collect()
    };
    dedup.reserve(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Component;

    fn s(loc: &str, ct: &str) -> String {
        from_location(loc, ct).to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn fnv1a_known_vectors() {
        assert_eq!(fnv1a_hex8(b""), "84222325");
        assert_eq!(fnv1a_hex8(b"hello"), "80aabd0b");
        assert_eq!(fnv1a_hex8(b"CON"), "aa5d684b");
    }

    #[test]
    fn sanitize_replaces_forbidden_and_control() {
        assert_eq!(
            sanitize_segment("a:b*c?\"d<e>f|g\\h/i"),
            "a_b_c__d_e_f_g_h_i"
        );
        assert_eq!(sanitize_segment("x\u{0001}y"), "x_y");
        assert_eq!(sanitize_segment("plain.txt"), "plain.txt");
    }

    #[test]
    fn sanitize_neutralizes_dot_segments() {
        assert_eq!(sanitize_segment("."), "_.");
        assert_eq!(sanitize_segment(".."), "_..");
        assert_eq!(sanitize_segment("..."), "...");
    }

    #[test]
    fn sanitize_prefixes_reserved_names() {
        assert_eq!(sanitize_segment("con"), "_con");
        assert_eq!(sanitize_segment("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_segment("com1"), "_com1");
        assert_eq!(sanitize_segment("LPT9.log"), "_LPT9.log");
        assert_eq!(sanitize_segment("com0"), "com0");
        assert_eq!(sanitize_segment("comx"), "comx");
    }

    #[test]
    fn sanitize_truncates_overlong_segment() {
        let seg = "a".repeat(250);
        let out = sanitize_segment(&seg);
        assert_eq!(out.len(), MAX_SEGMENT_BYTES + 1 + 8);
        assert!(out.starts_with(&"a".repeat(MAX_SEGMENT_BYTES)));
        assert!(out.ends_with(&format!("-{}", fnv1a_hex8(seg.as_bytes()))));
    }

    #[test]
    fn maps_url_hierarchy() {
        assert_eq!(
            s("http://example.com/path/page.html", "text/html"),
            "example.com/path/page.html"
        );
    }

    #[test]
    fn drops_default_port_keeps_custom() {
        assert_eq!(s("http://h:80/x.html", "text/html"), "h/x.html");
        assert_eq!(s("http://h:8080/x.html", "text/html"), "h_8080/x.html");
    }

    #[test]
    fn trailing_slash_and_root_become_index() {
        assert_eq!(s("http://h/dir/", "text/html"), "h/dir/index.html");
        assert_eq!(s("http://h/", "text/html"), "h/index.html");
    }

    #[test]
    fn infers_extension_from_content_type() {
        assert_eq!(s("http://h/logo", "image/png"), "h/logo.png");
        assert_eq!(s("http://h/data", "application/json"), "h/data.json");
        assert_eq!(
            s("http://h/mystery", "application/x-weird"),
            "h/mystery.bin"
        );
    }

    #[test]
    fn infers_pdf_extension_from_content_type() {
        assert_eq!(s("http://h/report", "application/pdf"), "h/report.pdf");
    }

    #[test]
    fn query_string_inserts_hash_before_extension() {
        assert_eq!(
            s("http://h/app.js?v=2", "application/javascript"),
            format!("h/app.q{}.js", fnv1a_hex8(b"v=2"))
        );
    }

    #[test]
    fn forces_css_js_extension_over_misleading_url_extension() {
        // A stylesheet/script served from a non-.css/.js endpoint (e.g.
        // MediaWiki's `load.php`) must extract with the extension the browser
        // needs to apply it offline — browsers refuse to treat a `.php` file
        // as CSS/JS when loaded from file://.
        assert_eq!(s("http://h/w/load.php", "text/css"), "h/w/load.css");
        assert_eq!(s("http://h/w/load.php", "text/javascript"), "h/w/load.js");
        assert_eq!(
            s("http://h/w/load.php?only=styles", "text/css"),
            format!("h/w/load.q{}.css", fnv1a_hex8(b"only=styles"))
        );
        // Already-correct extensions are unaffected.
        assert_eq!(s("http://h/a.css", "text/css"), "h/a.css");
        // An HTML document served from a non-.html URL (e.g. a MediaWiki
        // index.php) must extract as .html, or a browser opening it from
        // file:// downloads it instead of rendering.
        assert_eq!(
            s("http://h/wiki/index.php", "text/html"),
            "h/wiki/index.html"
        );
    }

    #[test]
    fn percent_decodes_path_segments() {
        assert_eq!(s("http://h/a%20b/c.css", "text/css"), "h/a b/c.css");
    }

    #[test]
    fn percent_encoded_slash_stays_one_segment() {
        assert_eq!(s("http://h/a%2Fb", "text/plain"), "h/a_b.txt");
    }

    #[test]
    fn url_collapses_literal_dot_segments() {
        assert_eq!(s("http://h/a/../b.html", "text/html"), "h/b.html");
    }

    #[test]
    fn content_id_fallback() {
        let mut d = Dedup::new();
        let p = output_path(None, Some("<x@y.com>"), "image/gif", 0, &mut d);
        assert_eq!(p.to_string_lossy().replace('\\', "/"), "_cid/_x@y.com_.gif");
    }

    #[test]
    fn part_index_fallback() {
        let mut d = Dedup::new();
        let p = output_path(None, None, "image/gif", 3, &mut d);
        assert_eq!(p.to_string_lossy().replace('\\', "/"), "_parts/part-3.gif");
    }

    #[test]
    fn dedupes_identical_paths_in_order() {
        let mut d = Dedup::new();
        let a = output_path(Some("http://h/a.html"), None, "text/html", 0, &mut d);
        let b = output_path(Some("http://h/a.html"), None, "text/html", 1, &mut d);
        let c = output_path(Some("http://h/a.html"), None, "text/html", 2, &mut d);
        assert_eq!(a.to_string_lossy(), "h/a.html");
        assert_eq!(b.to_string_lossy(), "h/a.1.html");
        assert_eq!(c.to_string_lossy(), "h/a.2.html");
    }

    #[test]
    fn dedupe_is_case_insensitive() {
        let mut d = Dedup::new();
        let a = output_path(Some("http://h/A.html"), None, "text/html", 0, &mut d);
        let b = output_path(Some("http://h/a.html"), None, "text/html", 1, &mut d);
        assert_eq!(a.to_string_lossy(), "h/A.html");
        assert_eq!(b.to_string_lossy(), "h/a.1.html");
    }

    /// The traversal defense: no produced path may be absolute or contain a
    /// `..` component, for any hostile input.
    #[test]
    fn security_never_escapes_root() {
        let hostile = [
            "http://evil/..%2F..%2Fetc%2Fpasswd",
            "http://evil/../../etc/passwd",
            "http://evil/%2e%2e/%2e%2e/x",
            "http://evil/a%00b/c",
            "file:///etc/passwd",
            "file://server/share/secret",
            "/etc/passwd",
            "../../windows/system32",
            "..\\..\\windows\\system32",
            "//host/share/x",
            "C:\\Windows\\system32\\cmd.exe",
            "cid:foo@bar.com",
            &("http://evil/".to_string() + &"a".repeat(5000)),
            "\\\\?\\C:\\evil",
        ];
        for loc in hostile {
            let mut d = Dedup::new();
            let p = output_path(Some(loc), None, "text/html", 0, &mut d);
            assert!(p.is_relative(), "not relative: {loc:?} -> {p:?}");
            for c in p.components() {
                assert!(
                    !matches!(c, Component::ParentDir),
                    "escaped via ..: {loc:?} -> {p:?}"
                );
                assert!(
                    !matches!(c, Component::RootDir | Component::Prefix(_)),
                    "absolute component: {loc:?} -> {p:?}"
                );
            }
        }
    }
}
