//! CSS URL rewriting via a scan-and-replace over `url(...)` tokens and
//! `@import` rules. Only the URL spans are touched; every other byte —
//! comments, whitespace, selectors, values, escaped strings — is reproduced
//! verbatim, which is the property that matters most for a faithful extract.

use url::Url;

/// CSS whitespace per the Syntax spec (space, tab, and the newline forms).
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Bytes that can appear inside an identifier; used only to decide whether an
/// occurrence of `url(` starts a fresh token or is the tail of a longer name.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'\\') || b >= 0x80
}

/// Skip a string literal starting at `start` (a quote byte), honoring backslash
/// escapes. Returns `(inner byte range, index just past the closing quote)`, or
/// `None` if the string is unterminated.
fn scan_string(bytes: &[u8], start: usize) -> Option<(std::ops::Range<usize>, usize)> {
    let quote = bytes[start];
    let inner_start = start + 1;
    let mut j = inner_start;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            c if c == quote => return Some((inner_start..j, j + 1)),
            _ => j += 1,
        }
    }
    None
}

/// Scan the body of a `url(...)` token whose `(` is at `open`. Returns the URL's
/// inner byte range and the index just past the closing `)`, or `None` if
/// malformed/unterminated.
fn scan_url_body(bytes: &[u8], open: usize) -> Option<(std::ops::Range<usize>, usize)> {
    let mut j = open + 1;
    while j < bytes.len() && is_ws(bytes[j]) {
        j += 1;
    }
    if j >= bytes.len() {
        return None;
    }
    if bytes[j] == b'"' || bytes[j] == b'\'' {
        let (inner, after) = scan_string(bytes, j)?;
        let mut k = after;
        while k < bytes.len() && is_ws(bytes[k]) {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == b')' {
            Some((inner, k + 1))
        } else {
            None
        }
    } else {
        let inner_start = j;
        while j < bytes.len() && bytes[j] != b')' && !is_ws(bytes[j]) {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        let inner_end = j.min(bytes.len());
        while j < bytes.len() && is_ws(bytes[j]) {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b')' {
            Some((inner_start..inner_end, j + 1))
        } else {
            None
        }
    }
}

/// Case-insensitive match of `needle` (ASCII) at `pos` in `bytes`.
fn matches_ci(bytes: &[u8], pos: usize, needle: &[u8]) -> bool {
    bytes
        .get(pos..pos + needle.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(needle))
}

/// Rewrite the `url(...)` tokens and `@import` targets in `css`, resolving each
/// reference against `base` via `resolve` and replacing hits with the relative
/// on-disk path. Unresolved references are left byte-for-byte unchanged.
pub fn rewrite_css<R>(css: &str, base: &Url, resolve: &R) -> String
where
    R: Fn(&Url, &str) -> Option<String>,
{
    let bytes = css.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut copied = 0;
    let mut i = 0;

    // Resolve the URL span `inner`; on a hit, flush the pending run up to
    // `token_start`, emit `pre + replacement + post`, and mark bytes consumed to
    // `token_end`. On a miss, do nothing so the original bytes flush verbatim.
    let rewrite = |out: &mut String,
                   copied: &mut usize,
                   inner: std::ops::Range<usize>,
                   token_start: usize,
                   token_end: usize,
                   pre: &str,
                   post: &str| {
        if let Some(new) = resolve(base, &css[inner]) {
            out.push_str(&css[*copied..token_start]);
            out.push_str(pre);
            out.push_str(&new);
            out.push_str(post);
            *copied = token_end;
        }
    };

    while i < len {
        // Comments: skip to `*/` so their interior is never scanned.
        if matches_ci(bytes, i, b"/*") {
            i = css[i + 2..]
                .find("*/")
                .map(|p| i + 2 + p + 2)
                .unwrap_or(len);
            continue;
        }
        // String literals outside of any token: skip whole, copied verbatim.
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            i = scan_string(bytes, i).map(|(_, after)| after).unwrap_or(len);
            continue;
        }
        // `url( ... )` function token at an identifier boundary.
        if matches_ci(bytes, i, b"url(") && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            if let Some((inner, end)) = scan_url_body(bytes, i + 3) {
                rewrite(&mut out, &mut copied, inner, i, end, "url(\"", "\")");
                i = end;
            } else {
                i += 1;
            }
            continue;
        }
        // `@import` rule: a bare string target needs rewriting (a `url(...)`
        // target is caught by the branch above on the next iteration).
        if matches_ci(bytes, i, b"@import") && !bytes.get(i + 7).is_some_and(|&b| is_ident_byte(b))
        {
            let mut j = i + 7;
            while j < len && is_ws(bytes[j]) {
                j += 1;
            }
            if (bytes.get(j) == Some(&b'"') || bytes.get(j) == Some(&b'\''))
                && let Some((inner, after)) = scan_string(bytes, j)
            {
                let quote = &css[j..j + 1];
                rewrite(&mut out, &mut copied, inner, j, after, quote, quote);
                i = after;
            } else {
                i = j;
            }
            continue;
        }
        i += 1;
    }

    out.push_str(&css[copied..]);
    out
}

/// Resolve a CSS part's charset label to an `encoding_rs` encoding, defaulting
/// to UTF-8 for absent, unknown, or replacement labels (matching the HTML path).
/// Callers decode the raw body with this, rewrite the text, then re-encode with
/// the same encoding so high bytes round-trip and every `url()`/`@import` still
/// gets rewritten even when a stray byte would break a UTF-8 interpretation.
pub fn css_encoding(charset: Option<&str>) -> &'static encoding_rs::Encoding {
    charset
        .and_then(|c| encoding_rs::Encoding::for_label_no_replacement(c.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolver that maps a fixed set of resolved URL strings to replacements,
    /// mirroring the real resolver's fragment handling (strip, then re-append).
    fn resolver(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&Url, &str) -> Option<String> {
        move |base: &Url, raw: &str| {
            if raw.starts_with("data:") {
                return None;
            }
            let (url_part, frag) = match raw.find('#') {
                Some(i) => (&raw[..i], &raw[i..]),
                None => (raw, ""),
            };
            let resolved = base.join(url_part).ok()?;
            pairs
                .iter()
                .find(|(u, _)| *u == resolved.as_str())
                .map(|(_, p)| format!("{p}{frag}"))
        }
    }

    fn base() -> Url {
        Url::parse("http://h/dir/page.css").unwrap()
    }

    #[test]
    fn rewrites_unquoted_url() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        assert_eq!(
            rewrite_css("a{background:url(bg.png)}", &base(), &r),
            "a{background:url(\"bg.png\")}"
        );
    }

    #[test]
    fn rewrites_double_and_single_quoted_url() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        assert_eq!(
            rewrite_css("a{background:url(\"bg.png\")}", &base(), &r),
            "a{background:url(\"bg.png\")}"
        );
        assert_eq!(
            rewrite_css("a{background:url('bg.png')}", &base(), &r),
            "a{background:url(\"bg.png\")}"
        );
    }

    #[test]
    fn rewrites_import_string_syntax() {
        let r = resolver(&[("http://h/dir/theme.css", "theme.css")]);
        assert_eq!(
            rewrite_css("@import \"theme.css\";\nbody{}", &base(), &r),
            "@import \"theme.css\";\nbody{}"
        );
        assert_eq!(
            rewrite_css(
                "@import 'sub/theme.css';",
                &base(),
                &resolver(&[("http://h/dir/sub/theme.css", "sub/theme.css")])
            ),
            "@import 'sub/theme.css';"
        );
    }

    #[test]
    fn rewrites_import_url_syntax() {
        let r = resolver(&[("http://h/dir/theme.css", "theme.css")]);
        assert_eq!(
            rewrite_css("@import url(theme.css);", &base(), &r),
            "@import url(\"theme.css\");"
        );
    }

    #[test]
    fn data_url_is_left_untouched() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        let css = "a{background:url(data:image/png;base64,AAAA)}";
        assert_eq!(rewrite_css(css, &base(), &r), css);
        let css2 = "a{background:url(\"data:image/png;base64,AA==\")}";
        assert_eq!(rewrite_css(css2, &base(), &r), css2);
    }

    #[test]
    fn unmapped_url_is_left_untouched() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        let css = "a{background:url(other.png)}";
        assert_eq!(rewrite_css(css, &base(), &r), css);
    }

    #[test]
    fn string_literals_and_comments_are_never_rewritten() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        // `url(...)` inside a string value or a comment must be preserved,
        // even the escaped-quote string, byte-for-byte.
        let css = r#"/* url(bg.png) */
a::before{content:"a\"url(bg.png)b"}"#;
        assert_eq!(rewrite_css(css, &base(), &r), css);
    }

    #[test]
    fn rewrites_real_url_alongside_preserved_string() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        assert_eq!(
            rewrite_css(r#".x{content:"a\"b";background:url(bg.png)}"#, &base(), &r),
            r#".x{content:"a\"b";background:url("bg.png")}"#
        );
    }

    #[test]
    fn similar_function_names_are_not_mistaken_for_url() {
        let r = resolver(&[("http://h/dir/bg.png", "bg.png")]);
        // `blur(` and `-webkit-url(` are different tokens; leave them alone.
        let css = "a{filter:blur(2px);x:-webkit-url(bg.png)}";
        assert_eq!(rewrite_css(css, &base(), &r), css);
    }

    #[test]
    fn fragment_only_svg_reference_is_rewritten_verbatim_when_mapped() {
        let r = resolver(&[("http://h/dir/sprite.svg", "sprite.svg")]);
        assert_eq!(
            rewrite_css("a{fill:url(sprite.svg#icon)}", &base(), &r),
            "a{fill:url(\"sprite.svg#icon\")}"
        );
    }
}
