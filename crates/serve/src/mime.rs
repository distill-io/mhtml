//! MIME-type fallback for serving parts. A part with no `Content-Type` header
//! has `content_type == ""` (the parser never guesses); an HTTP server needs a
//! type to send, so we fall back to the resource extension, and finally to
//! `application/octet-stream`.

use crate::ctype::content_type_essence;
use url::Url;

/// The MIME type to serve a part with: its declared `Content-Type` essence when
/// present, else guessed from the `Content-Location` extension, else
/// `application/octet-stream`.
pub fn mime_for(content_type: &str, content_location: Option<&str>) -> String {
    if !content_type.is_empty() {
        return content_type_essence(content_type);
    }
    content_location
        .and_then(|loc| Url::parse(loc).ok())
        .and_then(|url| {
            let path = url.path().to_string();
            let last = path.rsplit('/').next()?;
            let ext = last.rsplit_once('.')?.1.to_ascii_lowercase();
            mime_for_ext(&ext)
        })
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Map a lowercased file extension to a known MIME type.
fn mime_for_ext(ext: &str) -> Option<&'static str> {
    let mime = match ext {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "pdf" => "application/pdf",
        _ => return None,
    };
    Some(mime)
}

/// A `Content-Type` header value from a MIME type and optional charset.
pub fn content_type_header(mime: &str, charset: Option<&str>) -> String {
    match charset {
        Some(cs) => format!("{mime}; charset={cs}"),
        None => mime.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_wins_over_extension() {
        assert_eq!(
            mime_for("text/plain", Some("https://x/page.html")),
            "text/plain"
        );
    }

    #[test]
    fn content_type_parameters_stripped_via_essence() {
        assert_eq!(
            mime_for("text/HTML; charset=utf-8", Some("https://x/a.png")),
            "text/html"
        );
    }

    #[test]
    fn fallback_extension_hit() {
        assert_eq!(mime_for("", Some("https://x/style.CSS")), "text/css");
    }

    #[test]
    fn fallback_extension_pdf() {
        assert_eq!(mime_for("", Some("https://x/doc.pdf")), "application/pdf");
    }

    #[test]
    fn fallback_extension_ignores_query_string() {
        assert_eq!(
            mime_for("", Some("https://x/a.pdf?dl=1")),
            "application/pdf"
        );
    }

    #[test]
    fn fallback_unknown_extension() {
        assert_eq!(
            mime_for("", Some("https://x/data.xyz")),
            "application/octet-stream"
        );
    }

    #[test]
    fn fallback_no_extension() {
        assert_eq!(
            mime_for("", Some("https://x/resource")),
            "application/octet-stream"
        );
    }

    #[test]
    fn fallback_no_location() {
        assert_eq!(mime_for("", None), "application/octet-stream");
    }

    #[test]
    fn fallback_unparseable_location() {
        assert_eq!(mime_for("", Some("not a url")), "application/octet-stream");
    }

    #[test]
    fn header_without_charset() {
        assert_eq!(content_type_header("text/html", None), "text/html");
    }

    #[test]
    fn header_with_charset() {
        assert_eq!(
            content_type_header("text/html", Some("utf-8")),
            "text/html; charset=utf-8"
        );
    }
}
