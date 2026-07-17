//! Resource-URL naming strategy: how an archive's parts are referenced and
//! stored when served or uploaded. Two strategies coexist:
//!
//! - [`NamingStrategy::MirrorPath`] preserves the original URL hierarchy
//!   (`host/dir/file.ext`), so relative references resolve as they did online.
//! - [`NamingStrategy::ContentHash`] is content-addressed: each resource is a
//!   flat `<sha256>.<ext>` key derived from its bytes and MIME type, ignoring
//!   the URL entirely. Ideal for a content-addressed store (e.g. S3).
//!
//! Everything here is pure string/byte mapping. This module is the single
//! source of truth for the content-type→extension map (`ext_for_mime`, which
//! the CLI's disk `naming` delegates to) and for the `/`-relative path diff
//! (`relative_path`, which the CLI's `resolve` delegates to).

use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use url::Url;

use crate::ctype::content_type_essence;

/// How a resource is referenced and stored when served or uploaded.
pub enum NamingStrategy {
    /// Preserve the URL path hierarchy (`host/dir/file.ext`).
    MirrorPath,
    /// Content-addressed flat key (`<sha256>.<ext>`), ignoring the URL.
    ContentHash,
}

/// The relative serving/storage key for one resource under `strategy`.
///
/// [`NamingStrategy::ContentHash`] always yields `Some` (`<hash>.<ext>` from the
/// bytes and MIME, URL ignored). [`NamingStrategy::MirrorPath`] mirrors the
/// URL, yielding `None` when `url` is absent or unparseable.
pub fn resource_key(
    strategy: &NamingStrategy,
    url: Option<&str>,
    mime: &str,
    body: &[u8],
) -> Option<String> {
    match strategy {
        NamingStrategy::ContentHash => {
            Some(format!("{}.{}", content_hash(body), ext_for_mime(mime)))
        }
        NamingStrategy::MirrorPath => mirror_key(url?, mime),
    }
}

/// The lowercase-hex SHA-256 of `bytes`. Used as the content-addressed key
/// under [`NamingStrategy::ContentHash`].
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Map a `Content-Type` (parameters ignored) to a file extension, falling back
/// to `bin` for anything unrecognized. Single source of truth for the CLI's
/// disk `naming`, which delegates here.
pub fn ext_for_mime(content_type: &str) -> &'static str {
    match content_type_essence(content_type).as_str() {
        "text/html" => "html",
        "text/css" => "css",
        "text/javascript" | "application/javascript" => "js",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "font/woff2" => "woff2",
        "font/woff" => "woff",
        "application/json" => "json",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}

/// The reference to write into a document stored at `from_key` when it points
/// at the resource stored at `to_key`. Always relative, never a leading `/`.
///
/// [`NamingStrategy::ContentHash`] emits the bare `to_key`: flat co-location
/// makes it resolve for CSS→image, and the entry document's `<base href>`
/// resolves it against the CDN prefix. [`NamingStrategy::MirrorPath`] emits the
/// path from `from_key`'s directory to `to_key`.
pub(crate) fn reference(from_key: &str, to_key: &str, strategy: &NamingStrategy) -> String {
    match strategy {
        NamingStrategy::ContentHash => to_key.to_string(),
        NamingStrategy::MirrorPath => {
            let from_dir = match from_key.rfind('/') {
                Some(i) => &from_key[..i],
                None => "",
            };
            relative_path(from_dir, to_key)
        }
    }
}

/// The `/`-separated relative path from directory `from_dir` to file `to`,
/// climbing with `..` out of the referrer's directory. Both inputs are
/// output-root-relative `/`-joined keys; empty components are ignored. This is
/// the core the CLI's disk `resolve` delegates to after flattening `Path`s.
pub fn relative_path(from_dir: &str, to: &str) -> String {
    let from: Vec<&str> = from_dir.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();

    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();

    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_string());
    }
    for seg in &to[common..] {
        parts.push((*seg).to_string());
    }
    parts.join("/")
}

/// The URL-mirroring key for one resource: `host[_port]` head, the URL's
/// percent-encoded path segments as directories, and a filename whose extension
/// is forced to `css`/`js` for those render-strict types and otherwise inferred
/// only when the path supplies none. A trailing/empty final segment becomes
/// `index.html`. Returns `None` for a URL that has no path (e.g. `cid:`) or
/// cannot be parsed. URL-safe by construction; no filesystem sanitization.
fn mirror_key(url: &str, mime: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .unwrap_or("_nohost");
    let head = match url.port() {
        Some(p) => format!("{host}_{p}"),
        None => host.to_string(),
    };

    let segments: Vec<&str> = url.path_segments()?.collect();
    let (dirs, last) = segments.split_at(segments.len() - 1);

    let mut parts = vec![head];
    for dir in dirs.iter().filter(|s| !s.is_empty()) {
        parts.push((*dir).to_string());
    }
    parts.push(mirror_filename(last[0], mime));
    Some(parts.join("/"))
}

/// The canonical extension a browser needs for a render-strict type loaded from
/// a flat/relative location (`css`/`js` are sniffed by extension, not content).
/// The single source of truth for this map (`mhtml_cli::naming` delegates here).
pub fn forced_ext(mime: &str) -> Option<&'static str> {
    match content_type_essence(mime).as_str() {
        // An HTML/CSS/JS resource must carry its canonical extension: browsers
        // sniff these by extension (not content) from file://, so a mislabeled
        // URL path (e.g. `index.php` serving text/html, or `load.php` serving
        // text/css) is otherwise downloaded or ignored instead of rendered.
        "text/html" | "application/xhtml+xml" => Some("html"),
        "text/css" => Some("css"),
        "text/javascript" | "application/javascript" => Some("js"),
        _ => None,
    }
}

/// Build the mirror filename for a URL's final path segment: `index.html` when
/// empty; the `css`/`js` extension forced for render-strict types (stem kept);
/// otherwise the path's own extension, or the MIME-inferred one when absent.
fn mirror_filename(raw_last: &str, mime: &str) -> String {
    if raw_last.is_empty() {
        return "index.html".to_string();
    }
    let dot = raw_last
        .rfind('.')
        .filter(|&i| i > 0 && i < raw_last.len() - 1);
    match forced_ext(mime) {
        Some(forced) => {
            let stem = match dot {
                Some(i) => &raw_last[..i],
                None => raw_last,
            };
            format!("{stem}.{forced}")
        }
        None => match dot {
            Some(_) => raw_last.to_string(),
            None => format!("{raw_last}.{}", ext_for_mime(mime)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_empty_vector() {
        assert_eq!(
            content_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn ext_for_mime_known_and_unknown() {
        assert_eq!(ext_for_mime("text/css"), "css");
        assert_eq!(ext_for_mime("text/javascript"), "js");
        assert_eq!(ext_for_mime("application/javascript"), "js");
        assert_eq!(ext_for_mime("image/png"), "png");
        assert_eq!(ext_for_mime("image/jpeg"), "jpg");
        assert_eq!(ext_for_mime("image/svg+xml"), "svg");
        assert_eq!(ext_for_mime("font/woff2"), "woff2");
        assert_eq!(ext_for_mime("application/pdf"), "pdf");
        assert_eq!(ext_for_mime("application/json"), "json");
        // Parameters are ignored via the essence.
        assert_eq!(ext_for_mime("text/css; charset=utf-8"), "css");
        // Anything unrecognized falls back to a generic binary extension.
        assert_eq!(ext_for_mime("application/x-weird"), "bin");
    }

    #[test]
    fn content_hash_key_is_hash_dot_ext() {
        let body = b"\x89PNG payload";
        // The key is content-addressed: hash of the bytes, extension from MIME,
        // and the URL is ignored entirely (even when absent).
        let expected = format!("{}.png", content_hash(body));
        assert_eq!(
            resource_key(&NamingStrategy::ContentHash, None, "image/png", body),
            Some(expected.clone())
        );
        assert_eq!(
            resource_key(
                &NamingStrategy::ContentHash,
                Some("http://h/whatever/ignored.gif"),
                "image/png",
                body
            ),
            Some(expected)
        );
    }

    #[test]
    fn content_hash_reference_is_bare_ignoring_referrer() {
        // The referrer is irrelevant under ContentHash: the reference is the
        // bare target key, and never starts with '/'.
        assert_eq!(
            reference("deep/a/b.html", "abc123.png", &NamingStrategy::ContentHash),
            "abc123.png"
        );
        assert_eq!(
            reference("x.html", "abc123.png", &NamingStrategy::ContentHash),
            "abc123.png"
        );
    }

    #[test]
    fn mirror_reference_is_referrer_relative() {
        assert_eq!(
            reference("h/a/b.html", "h/img/x.png", &NamingStrategy::MirrorPath),
            "../img/x.png"
        );
        assert_eq!(
            reference("h/page.html", "h/style.css", &NamingStrategy::MirrorPath),
            "style.css"
        );
    }

    #[test]
    fn reference_never_starts_with_slash() {
        let cases = [
            ("h/a/b.html", "h/img/x.png"),
            ("h/page.html", "h/style.css"),
            ("index.html", "sibling.css"),
            ("h/deep/deeper/p.html", "other/z.js"),
        ];
        for (from, to) in cases {
            for strat in [NamingStrategy::MirrorPath, NamingStrategy::ContentHash] {
                let r = reference(from, to, &strat);
                assert!(!r.starts_with('/'), "{from} -> {to} ({r}) started with '/'");
            }
        }
    }

    fn mkey(url: &str, mime: &str) -> Option<String> {
        resource_key(&NamingStrategy::MirrorPath, Some(url), mime, b"body")
    }

    #[test]
    fn mirror_key_mirrors_host_and_path() {
        assert_eq!(
            mkey("http://h/a/b.html", "text/html"),
            Some("h/a/b.html".to_string())
        );
    }

    #[test]
    fn mirror_key_custom_port_and_index() {
        assert_eq!(
            mkey("http://h:8080/x.html", "text/html").as_deref(),
            Some("h_8080/x.html")
        );
        assert_eq!(
            mkey("http://h/dir/", "text/html").as_deref(),
            Some("h/dir/index.html")
        );
        assert_eq!(
            mkey("http://h/", "text/html").as_deref(),
            Some("h/index.html")
        );
    }

    #[test]
    fn mirror_key_infers_and_forces_extension() {
        // No extension in the path: infer from MIME.
        assert_eq!(
            mkey("http://h/logo", "image/png").as_deref(),
            Some("h/logo.png")
        );
        // Mislabeled extension for a render-strict type: force css/js.
        assert_eq!(
            mkey("http://h/w/load.php", "text/css").as_deref(),
            Some("h/w/load.css")
        );
        // Existing, non-forced extension is preserved verbatim (percent-encoded).
        assert_eq!(
            mkey("http://h/a%20b/c.css", "text/css").as_deref(),
            Some("h/a%20b/c.css")
        );
    }

    #[test]
    fn relative_path_diffs() {
        assert_eq!(relative_path("h", "h/style.css"), "style.css");
        assert_eq!(relative_path("h", "h/img/logo.png"), "img/logo.png");
        assert_eq!(relative_path("h/a", "h/style.css"), "../style.css");
        assert_eq!(
            relative_path("h/a/b", "h/img/logo.png"),
            "../../img/logo.png"
        );
        assert_eq!(relative_path("a/b", "c/d/x.png"), "../../c/d/x.png");
    }

    #[test]
    fn mirror_key_none_for_absent_or_unparseable_or_pathless() {
        assert_eq!(
            resource_key(&NamingStrategy::MirrorPath, None, "text/html", b""),
            None
        );
        assert_eq!(mkey("not a url", "text/html"), None);
        assert_eq!(mkey("cid:x@y.com", "image/png"), None);
    }
}
