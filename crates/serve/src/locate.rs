//! URL locating primitives shared by every consumer of an archive: normalizing
//! a `Content-Location` into a stable map key, and resolving a raw HTML/CSS
//! reference against a document base into that same key plus its fragment.
//!
//! These are the disk-independent half of reference resolution. Mapping the
//! resolved key onto an actual output path lives in the CLI's `resolve` module.

use url::Url;

/// Normalize a raw URL string into the canonical form used as a rewrite-map
/// key: `url`-crate parsing followed by `as_str()`. Returns `None` for an
/// unparseable URL (which can never match a resolved reference anyway).
pub fn normalize_url(raw: &str) -> Option<String> {
    Url::parse(raw).ok().map(|u| u.as_str().to_string())
}

/// Resolve a raw reference `raw` against document base `base`, returning the
/// normalized target URL (a rewrite-map key) paired with the reference's
/// original fragment suffix (`""` or `#frag`, verbatim).
///
/// Returns `None` for references that must be left untouched: empty/whitespace,
/// fragment-only (`#...`), and the `javascript:` / `data:` / `mailto:` schemes.
pub fn resolve_reference(base: &Url, raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || starts_ci(trimmed, "javascript:")
        || starts_ci(trimmed, "data:")
        || starts_ci(trimmed, "mailto:")
    {
        return None;
    }

    let (url_part, fragment) = match trimmed.find('#') {
        Some(i) => (&trimmed[..i], &trimmed[i..]),
        None => (trimmed, ""),
    };

    let resolved = base.join(url_part).ok()?;
    Some((resolved.as_str().to_string(), fragment.to_string()))
}

/// Case-insensitive ASCII prefix test that never panics on non-boundary splits.
fn starts_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    #[test]
    fn normalize_url_uses_url_crate_form() {
        assert_eq!(
            normalize_url("http://h/a/../b/style.css"),
            Some("http://h/b/style.css".to_string())
        );
        assert_eq!(
            normalize_url("cid:image1@example.com"),
            Some("cid:image1@example.com".to_string())
        );
        assert_eq!(normalize_url("not a url"), None);
    }

    #[test]
    fn relative_ref_is_resolved_and_normalized_against_base() {
        assert_eq!(
            resolve_reference(&base("http://h/dir/page.html"), "style.css"),
            Some(("http://h/dir/style.css".to_string(), String::new()))
        );
    }

    #[test]
    fn fragment_is_split_off_and_preserved() {
        assert_eq!(
            resolve_reference(&base("http://h/page.html"), "style.css#icons"),
            Some(("http://h/style.css".to_string(), "#icons".to_string()))
        );
    }

    #[test]
    fn data_javascript_mailto_empty_and_fragment_only_are_untouched() {
        let b = base("http://h/page.html");
        assert_eq!(resolve_reference(&b, "data:image/png;base64,AAAA"), None);
        assert_eq!(resolve_reference(&b, "JavaScript:void(0)"), None);
        assert_eq!(resolve_reference(&b, "mailto:a@b.com"), None);
        assert_eq!(resolve_reference(&b, ""), None);
        assert_eq!(resolve_reference(&b, "   "), None);
        assert_eq!(resolve_reference(&b, "#section"), None);
    }
}
