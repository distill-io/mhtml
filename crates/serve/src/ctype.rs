//! Shared `Content-Type` handling for the consumption modules.

/// The media-type "essence" of a `Content-Type` header value: everything before
/// the first `;` (parameters dropped), trimmed and ASCII-lowercased. So
/// `text/HTML; charset=utf-8` becomes `text/html`.
pub fn content_type_essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}
