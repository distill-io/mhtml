//! HTML URL rewriting via lol_html's streaming rewriter. Reference-bearing
//! attributes and `<style>` content are resolved against the document's base
//! URL and pointed at the extracted files; every untouched byte streams through
//! verbatim. The `<base href>` is neutralized so relative references stay local.

use std::borrow::Cow;
use std::cell::RefCell;

use lol_html::html_content::{ContentType, Element};
use lol_html::{AsciiCompatibleEncoding, HtmlRewriter, Settings, element, text};
use url::Url;

use crate::rewrite_css::rewrite_css;

/// Decode the HTML character references lol_html leaves encoded in raw
/// attribute values, so a reference written `?a=1&amp;b=2` matches a map keyed
/// by the real URL `?a=1&b=2`. Only the references that legitimately appear in
/// URL attribute values are handled — the five predefined entities and numeric
/// character references; any other `&…;` run is copied through verbatim.
fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    // Longest reference we decode is a 7-digit numeric ref (`&#1114111;`), so a
    // bounded look-ahead keeps this linear and avoids matching a distant `;`.
    const MAX_REF: usize = 10;
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after
            .get(..after.len().min(MAX_REF))
            .and_then(|w| w.find(';').map(|semi| (&after[..semi], semi)))
            .and_then(|(entity, semi)| entity_char(entity).map(|c| (c, semi)));
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Resolve a single character-reference body (the text between `&` and `;`) to
/// its character, or `None` if it is not one we decode.
fn entity_char(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let num = entity.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Elements whose reference-bearing attribute is a plain URL. Each entry is a
/// `(selector, attribute)` pair fed to a shared rewriting handler.
const URL_ATTRS: &[(&str, &str)] = &[
    ("a[href]", "href"),
    ("link[href]", "href"),
    ("area[href]", "href"),
    ("img[src]", "src"),
    ("script[src]", "src"),
    ("iframe[src]", "src"),
    ("frame[src]", "src"),
    ("embed[src]", "src"),
    ("audio[src]", "src"),
    ("video[src]", "src"),
    ("source[src]", "src"),
    ("input[src]", "src"),
    ("track[src]", "src"),
    ("video[poster]", "poster"),
    ("object[data]", "data"),
    ("body[background]", "background"),
    ("button[formaction]", "formaction"),
    ("input[formaction]", "formaction"),
];

/// Resolve the document's effective encoding from a declared charset label,
/// defaulting to UTF-8 for absent, unknown, or non-ASCII-compatible charsets.
fn encoding_for(charset: Option<&str>) -> AsciiCompatibleEncoding {
    charset
        .and_then(|c| encoding_rs::Encoding::for_label_no_replacement(c.as_bytes()))
        .and_then(AsciiCompatibleEncoding::new)
        .unwrap_or_else(AsciiCompatibleEncoding::utf_8)
}

/// Pre-scan the document for its `<base>` element: resolve the first `href`
/// against `base` to obtain the effective base URL (references may precede
/// `<base>`, so this must run before the rewrite pass), and report whether any
/// `<base>` element is present (so the caller knows whether to inject one when
/// emitting a `base_href`).
fn scan_base(html: &[u8], encoding: AsciiCompatibleEncoding, base: &Url) -> (Url, bool) {
    let found: RefCell<Option<String>> = RefCell::new(None);
    let seen: RefCell<bool> = RefCell::new(false);
    let settings = Settings::new()
        .with_encoding(encoding)
        .append_element_content_handler(element!("base", |el| {
            *seen.borrow_mut() = true;
            let mut slot = found.borrow_mut();
            if slot.is_none()
                && let Some(href) = el.get_attribute("href")
            {
                *slot = Some(href);
            }
            Ok(())
        }));
    let mut rewriter = HtmlRewriter::new(settings, |_: &[u8]| {});
    let _ = rewriter.write(html);
    let _ = rewriter.end();

    let effective = found
        .into_inner()
        .and_then(|href| base.join(&decode_entities(&href)).ok())
        .unwrap_or_else(|| base.clone());
    (effective, seen.into_inner())
}

/// Escape a string for use inside a double-quoted HTML attribute value.
fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Rewrite a single plain-URL attribute in place if it resolves to a mapped
/// target; a miss leaves the attribute untouched.
fn rewrite_attr<R>(el: &mut Element, attr: &str, base: &Url, resolve: &R)
where
    R: Fn(&Url, &str) -> Option<String>,
{
    if let Some(value) = el.get_attribute(attr)
        && let Some(new) = resolve(base, &decode_entities(&value))
    {
        let _ = el.set_attribute(attr, &new);
    }
}

/// Normalize a `base_href` to name a directory (trailing slash) so bare
/// references resolve under it rather than replacing its last path segment.
fn ensure_dir_href(href: &str) -> String {
    if href.is_empty() || href.ends_with('/') {
        href.to_string()
    } else {
        format!("{href}/")
    }
}

/// HTML "ASCII whitespace" (tab, LF, FF, CR, space) — the only separators the
/// srcset grammar treats as whitespace.
fn is_srcset_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

/// Rewrite each URL in a `srcset` candidate list, preserving descriptors and
/// whitespace shape. Returns `None` when no candidate resolved, so the original
/// attribute is left byte-for-byte unchanged.
///
/// Candidate splitting follows the HTML srcset grammar: a candidate URL is a
/// maximal run of non-whitespace bytes, so a comma embedded in the URL stays
/// part of it — only a *trailing* comma (or a comma after the descriptor) ends
/// the candidate. Splitting naively on every comma would mis-parse (and
/// corrupt) a URL that legitimately contains one.
fn rewrite_srcset<R>(value: &str, base: &Url, resolve: &R) -> Option<String>
where
    R: Fn(&Url, &str) -> Option<String>,
{
    let bytes = value.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut changed = false;
    let mut i = 0;

    while i < len {
        // Inter-candidate separators: ASCII whitespace and commas, verbatim.
        let sep_start = i;
        while i < len && (is_srcset_ws(bytes[i]) || bytes[i] == b',') {
            i += 1;
        }
        out.push_str(&value[sep_start..i]);
        if i >= len {
            break;
        }

        // URL token: a maximal run of non-whitespace. A trailing comma is a
        // candidate separator, not part of the URL.
        let url_start = i;
        while i < len && !is_srcset_ws(bytes[i]) {
            i += 1;
        }
        let run_end = i;
        let mut url_end = run_end;
        while url_end > url_start && bytes[url_end - 1] == b',' {
            url_end -= 1;
        }
        let url = &value[url_start..url_end];
        match resolve(base, &decode_entities(url)) {
            Some(new) => {
                out.push_str(&new);
                changed = true;
            }
            None => out.push_str(url),
        }
        // Emit any stripped trailing commas verbatim (they separate candidates).
        out.push_str(&value[url_end..run_end]);
        // A comma-terminated URL has no descriptor; fall back to the split loop.
        if url_end < run_end {
            continue;
        }

        // Descriptor: copy verbatim up to the next top-level comma, which starts
        // the following candidate (commas inside parentheses stay in it).
        let desc_start = i;
        let mut depth: u32 = 0;
        while i < len {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        out.push_str(&value[desc_start..i]);
    }

    changed.then_some(out)
}

/// Run the rewrite pass, returning the rewritten bytes or `None` if lol_html
/// bails out (malformed input, memory limit).
fn rewrite_pass<R>(
    html: &[u8],
    base: &Url,
    encoding: AsciiCompatibleEncoding,
    base_href: Option<&str>,
    has_base: bool,
    resolve: &R,
) -> Option<Vec<u8>>
where
    R: Fn(&Url, &str) -> Option<String>,
{
    let style_buf: RefCell<String> = RefCell::new(String::new());
    let mut settings = Settings::new().with_encoding(encoding);

    for &(selector, attr) in URL_ATTRS {
        settings = settings.append_element_content_handler(element!(selector, move |el| {
            rewrite_attr(el, attr, base, resolve);
            Ok(())
        }));
    }

    for (selector, attr) in [
        ("img[srcset]", "srcset"),
        ("source[srcset]", "srcset"),
        ("link[imagesrcset]", "imagesrcset"),
    ] {
        settings = settings.append_element_content_handler(element!(selector, move |el| {
            if let Some(value) = el.get_attribute(attr)
                && let Some(new) = rewrite_srcset(&value, base, resolve)
            {
                let _ = el.set_attribute(attr, &new);
            }
            Ok(())
        }));
    }

    // SVG <image> (raster) and <use> (sprite) reference subresources through
    // `href` and the legacy `xlink:href`; neither element matches the plain
    // URL_ATTRS selectors, so repoint both attributes here. (An external <use>
    // is still blocked by same-origin policy on a file:// document, but the
    // same-origin bundle route serves it correctly, and rewriting is right
    // either way.)
    for selector in ["image", "use"] {
        settings = settings.append_element_content_handler(element!(selector, move |el| {
            rewrite_attr(el, "href", base, resolve);
            rewrite_attr(el, "xlink:href", base, resolve);
            Ok(())
        }));
    }

    // Browsers serialize declarative shadow DOM in MHTML with non-standard
    // attribute names (`shadowmode`, `shadowdelegatesfocus`) that only the
    // MHTML loader understands. Rename them to the web-standard forms so a
    // browser re-parsing the extracted HTML reconstructs the shadow trees.
    settings = settings.append_element_content_handler(element!("template[shadowmode]", |el| {
        for (old, new) in [
            ("shadowmode", "shadowrootmode"),
            ("shadowdelegatesfocus", "shadowrootdelegatesfocus"),
        ] {
            if let Some(v) = el.get_attribute(old) {
                el.remove_attribute(old);
                let _ = el.set_attribute(new, &v);
            }
        }
        Ok(())
    }));

    // <base> handling. Resolution has already been anchored via `base`, so the
    // emitted `<base>` only affects how a browser resolves the (relative)
    // references we write. With no `base_href` the original href is neutralized
    // (removed) so those references stay local; with a `base_href` it is set to
    // that value (a CDN/S3 prefix) so bare keys resolve against it. When the
    // document had no `<base>` at all, one is injected at the top of `<head>`.
    // A bare content-hash reference like `hash.png` only resolves *under* the
    // base when the base names a directory, so a `base_href` without a trailing
    // slash (e.g. `https://cdn/assets`) would drop its last segment. Normalize.
    let emit_base = base_href.map(ensure_dir_href);
    let base_for_handler = emit_base.clone();
    settings = settings.append_element_content_handler(element!("base", move |el| {
        match &base_for_handler {
            Some(href) => {
                let _ = el.set_attribute("href", href);
            }
            None => el.remove_attribute("href"),
        }
        Ok(())
    }));
    if !has_base && let Some(href) = &emit_base {
        let tag = format!("<base href=\"{}\">", escape_attr(href));
        settings = settings.append_element_content_handler(element!("head", move |el| {
            el.prepend(&tag, ContentType::Html);
            Ok(())
        }));
    }

    // Strip integrity/crossorigin/nonce from <link> and <script>: the extracted
    // copies are local and trusted, but SRI can no longer match (CSS `url()`s
    // are rewritten) and a CORS request from a file:// (opaque) origin to
    // another local file cannot succeed, so the browser would refuse to apply
    // the local stylesheet/script. The `nonce` is likewise meaningless offline.
    for selector in ["link", "script"] {
        settings = settings.append_element_content_handler(element!(selector, |el| {
            el.remove_attribute("integrity");
            el.remove_attribute("crossorigin");
            el.remove_attribute("nonce");
            Ok(())
        }));
    }

    // Neutralize a Content-Security-Policy declared in a <meta http-equiv>: a
    // policy that whitelists the original https hosts (not the file: origin the
    // extract loads from) makes the browser refuse to apply the extracted local
    // CSS/scripts/images/fonts even though they load.
    settings = settings.append_element_content_handler(element!("meta[http-equiv]", |el| {
        if let Some(v) = el.get_attribute("http-equiv") {
            let v = v.trim().to_ascii_lowercase();
            if v == "content-security-policy" || v == "content-security-policy-report-only" {
                el.remove();
            }
        }
        Ok(())
    }));

    // style="" attributes delegate to the CSS rewriter.
    settings = settings.append_element_content_handler(element!("[style]", move |el| {
        if let Some(value) = el.get_attribute("style") {
            let rewritten = rewrite_css(&value, base, resolve);
            if rewritten != value {
                let _ = el.set_attribute("style", &rewritten);
            }
        }
        Ok(())
    }));

    // <style> element text content delegates to the CSS rewriter; buffer chunks
    // and rewrite the whole text node once it is complete.
    settings = settings.append_element_content_handler(text!("style", move |t| {
        style_buf.borrow_mut().push_str(t.as_str());
        if t.last_in_text_node() {
            let css = std::mem::take(&mut *style_buf.borrow_mut());
            t.set_str(rewrite_css(&css, base, resolve));
        } else {
            t.set_str(String::new());
        }
        Ok(())
    }));

    let mut output = Vec::new();
    let mut rewriter = HtmlRewriter::new(settings, |c: &[u8]| output.extend_from_slice(c));
    rewriter.write(html).ok()?;
    rewriter.end().ok()?;
    Some(output)
}

/// Rewrite the references in an HTML document `html` (interpreted using
/// `charset`, defaulting to UTF-8), resolving each against `base` via `resolve`.
/// Returns the rewritten bytes, or the original bytes unchanged if the rewriter
/// bails out on malformed input.
///
/// `base_href` controls the emitted `<base>` element (not resolution, which is
/// always anchored on `base`/the document's own `<base href>`): `None`
/// neutralizes any `<base href>` (references stay local); `Some(url)` sets it —
/// injecting a `<base>` into `<head>` if the document had none — so the
/// (relative) references resolve against `url`.
pub fn rewrite_html<R>(
    html: &[u8],
    base: &Url,
    charset: Option<&str>,
    base_href: Option<&str>,
    resolve: &R,
) -> Vec<u8>
where
    R: Fn(&Url, &str) -> Option<String>,
{
    let encoding = encoding_for(charset);
    let (base, has_base) = scan_base(html, encoding, base);
    rewrite_pass(html, &base, encoding, base_href, has_base, resolve)
        .unwrap_or_else(|| html.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&Url, &str) -> Option<String> {
        move |base: &Url, raw: &str| {
            let trimmed = raw.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with("data:")
                || trimmed.starts_with("javascript:")
                || trimmed.starts_with("mailto:")
            {
                return None;
            }
            let (url_part, frag) = match trimmed.find('#') {
                Some(i) => (&trimmed[..i], &trimmed[i..]),
                None => (trimmed, ""),
            };
            let resolved = base.join(url_part).ok()?;
            pairs
                .iter()
                .find(|(u, _)| *u == resolved.as_str())
                .map(|(_, p)| format!("{p}{frag}"))
        }
    }

    fn base() -> Url {
        Url::parse("http://h/dir/page.html").unwrap()
    }

    fn run(html: &str, pairs: &'static [(&'static str, &'static str)]) -> String {
        let out = rewrite_html(html.as_bytes(), &base(), None, None, &resolver(pairs));
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn rewrites_anchor_href() {
        assert_eq!(
            run(
                r#"<a href="other.html">x</a>"#,
                &[("http://h/dir/other.html", "other.html")]
            ),
            r#"<a href="other.html">x</a>"#
        );
        assert_eq!(
            run(
                r#"<a href="http://h/img/p.html">x</a>"#,
                &[("http://h/img/p.html", "../img/p.html")]
            ),
            r#"<a href="../img/p.html">x</a>"#
        );
    }

    #[test]
    fn rewrites_img_src() {
        assert_eq!(
            run(
                r#"<img src="logo.png">"#,
                &[("http://h/dir/logo.png", "logo.png")]
            ),
            r#"<img src="logo.png">"#
        );
    }

    #[test]
    fn rewrites_declarative_shadow_dom_attributes() {
        // Browsers serialize shadow roots in MHTML with the non-standard
        // `shadowmode` / `shadowdelegatesfocus` attributes, which no browser
        // parses as declarative shadow DOM — so the extracted page loses all
        // shadow content. Rename them to the web-standard `shadowrootmode` /
        // `shadowrootdelegatesfocus` so shadow trees reconstruct offline.
        assert_eq!(
            run(
                r#"<div><template shadowmode="open"><span>s</span></template></div>"#,
                &[]
            ),
            r#"<div><template shadowrootmode="open"><span>s</span></template></div>"#
        );
        assert_eq!(
            run(
                r#"<template shadowmode="closed" shadowdelegatesfocus="">x</template>"#,
                &[]
            ),
            r#"<template shadowrootmode="closed" shadowrootdelegatesfocus="">x</template>"#
        );
    }

    #[test]
    fn strips_integrity_crossorigin_nonce_from_link_and_script() {
        // A local extract cannot satisfy SRI (the CSS was url()-rewritten so its
        // hash no longer matches) or CORS (a file:// document has an opaque
        // origin), so `integrity`/`crossorigin` make the browser refuse to
        // apply the local stylesheet/script. Strip them (and the moot `nonce`)
        // while still rewriting the href/src to the local file.
        assert_eq!(
            run(
                r#"<link crossorigin="anonymous" integrity="sha512-x" nonce="n" rel="stylesheet" href="app.css">"#,
                &[("http://h/dir/app.css", "app.css")]
            ),
            r#"<link rel="stylesheet" href="app.css">"#
        );
        assert_eq!(
            run(
                r#"<script crossorigin="anonymous" integrity="sha512-y" nonce="n" src="app.js"></script>"#,
                &[("http://h/dir/app.js", "app.js")]
            ),
            r#"<script src="app.js"></script>"#
        );
    }

    #[test]
    fn neutralizes_content_security_policy_meta() {
        // A CSP meta whitelisting the original https hosts (not file:) would
        // block the extracted local resources; it must be removed. Other
        // http-equiv metas (and CSP delivered as a normal attribute value that
        // is not http-equiv) are left untouched.
        assert_eq!(
            run(
                r#"<meta http-equiv="Content-Security-Policy" content="default-src https://x"><p>k</p>"#,
                &[]
            ),
            "<p>k</p>"
        );
        assert_eq!(
            run(
                r#"<meta http-equiv="content-security-policy-report-only" content="default-src https://x">"#,
                &[]
            ),
            ""
        );
        let keep = r#"<meta http-equiv="X-UA-Compatible" content="IE=edge">"#;
        assert_eq!(run(keep, &[]), keep);
    }

    #[test]
    fn neutralizes_base_href() {
        assert_eq!(run(r#"<base href="http://x/">"#, &[]), "<base>");
    }

    #[test]
    fn base_href_before_reference_sets_effective_base() {
        // The reference precedes <base>, so the pre-scan must apply the base.
        assert_eq!(
            run(
                r#"<a href="p.html">x</a><base href="http://cdn/">"#,
                &[("http://cdn/p.html", "p.html")]
            ),
            r#"<a href="p.html">x</a><base>"#
        );
    }

    #[test]
    fn base_href_injected_into_head_when_document_has_none() {
        // With a base_href and no existing <base>, one is injected at the top of
        // <head>; the (relative) reference is left relative, resolving against it.
        let out = rewrite_html(
            b"<html><head><title>t</title></head><body><a href=\"other.html\">x</a></body></html>",
            &base(),
            None,
            Some("https://cdn/"),
            &resolver(&[("http://h/dir/other.html", "other.html")]),
        );
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("<head><base href=\"https://cdn/\"><title>"),
            "got: {s}"
        );
        assert!(s.contains("href=\"other.html\""), "ref stays relative: {s}");
    }

    #[test]
    fn base_href_without_trailing_slash_is_normalized_to_a_directory() {
        // A base that does not end in '/' would drop its last path segment when
        // a bare reference resolves against it, so it must be normalized.
        let out = rewrite_html(
            b"<html><head></head><body></body></html>",
            &base(),
            None,
            Some("https://cdn/assets"),
            &resolver(&[]),
        );
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("<base href=\"https://cdn/assets/\">"),
            "got: {s}"
        );
    }

    #[test]
    fn base_href_replaces_existing_base_without_injecting_a_second() {
        // Resolution still uses the document's own <base href="http://orig/">;
        // the emitted <base> is replaced with the caller's value, and no extra
        // <base> is injected.
        let out = rewrite_html(
            b"<head><base href=\"http://orig/\"></head><a href=\"p.html\">x</a>",
            &base(),
            None,
            Some("https://cdn/"),
            &resolver(&[("http://orig/p.html", "p.html")]),
        );
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("<base href=\"https://cdn/\">"), "got: {s}");
        assert_eq!(s.matches("<base").count(), 1, "exactly one base: {s}");
        assert!(s.contains("href=\"p.html\">"), "ref stays relative: {s}");
    }

    #[test]
    fn rewrites_srcset_candidates_preserving_descriptors() {
        assert_eq!(
            run(
                r#"<img srcset="a.png 1x, b.png 2x">"#,
                &[
                    ("http://h/dir/a.png", "img/a.png"),
                    ("http://h/dir/b.png", "img/b.png"),
                ]
            ),
            r#"<img srcset="img/a.png 1x, img/b.png 2x">"#
        );
    }

    #[test]
    fn srcset_comma_inside_url_is_not_a_candidate_boundary() {
        // A single candidate whose URL legally contains a comma must not be
        // split: the tail after the comma is not a separate candidate, so a
        // map key for that tail must never rewrite part of this URL.
        assert_eq!(
            run(
                r#"<img srcset="a,b.png 1x">"#,
                &[("http://h/dir/b.png", "sub/b.png")]
            ),
            r#"<img srcset="a,b.png 1x">"#
        );
    }

    #[test]
    fn srcset_mapped_comma_url_is_rewritten_whole() {
        // A candidate URL that itself contains a comma and is mapped must be
        // rewritten in one piece, not severed at the comma.
        assert_eq!(
            run(
                r#"<img srcset="img/a,b.png 1x">"#,
                &[("http://h/dir/img/a,b.png", "local/ab.png")]
            ),
            r#"<img srcset="local/ab.png 1x">"#
        );
    }

    #[test]
    fn rewrites_svg_image_and_use_href_and_xlink_href() {
        // SVG <image> (raster) and <use> (sprite) reference subresources via
        // `href` and the legacy `xlink:href`; both must be repointed to the
        // local file so they do not hit the (sealed) network.
        assert_eq!(
            run(
                r#"<svg><image href="photo.png"></image></svg>"#,
                &[("http://h/dir/photo.png", "img/photo.png")]
            ),
            r#"<svg><image href="img/photo.png"></image></svg>"#
        );
        assert_eq!(
            run(
                r#"<svg><image xlink:href="http://h/p2.png"></image></svg>"#,
                &[("http://h/p2.png", "../p2.png")]
            ),
            r#"<svg><image xlink:href="../p2.png"></image></svg>"#
        );
        assert_eq!(
            run(
                r#"<svg><use href="sprite.svg#i"></use></svg>"#,
                &[("http://h/dir/sprite.svg", "sprite.svg")]
            ),
            r#"<svg><use href="sprite.svg#i"></use></svg>"#
        );
        assert_eq!(
            run(
                r#"<svg><use xlink:href="sprite.svg#i"></use></svg>"#,
                &[("http://h/dir/sprite.svg", "s.svg")]
            ),
            r#"<svg><use xlink:href="s.svg#i"></use></svg>"#
        );
    }

    #[test]
    fn rewrites_link_imagesrcset_candidates() {
        // <link rel=preload as=image imagesrcset="..."> carries a srcset-format
        // value that must be rewritten like img/source srcset.
        assert_eq!(
            run(
                r#"<link rel="preload" as="image" imagesrcset="a.png 1x, b.png 2x" href="a.png">"#,
                &[
                    ("http://h/dir/a.png", "img/a.png"),
                    ("http://h/dir/b.png", "img/b.png"),
                ]
            ),
            r#"<link rel="preload" as="image" imagesrcset="img/a.png 1x, img/b.png 2x" href="img/a.png">"#
        );
    }

    #[test]
    fn rewrites_style_element_text() {
        assert_eq!(
            run(
                r#"<style>a{background:url(bg.png)}</style>"#,
                &[("http://h/dir/bg.png", "bg.png")]
            ),
            r#"<style>a{background:url("bg.png")}</style>"#
        );
    }

    #[test]
    fn rewrites_style_attribute() {
        let out = run(
            r#"<div style="background:url(bg.png)">x</div>"#,
            &[("http://h/dir/bg.png", "bg.png")],
        );
        // lol_html escapes the CSS double-quotes for the attribute context.
        assert_eq!(
            out,
            r#"<div style="background:url(&quot;bg.png&quot;)">x</div>"#
        );
    }

    #[test]
    fn unmapped_and_special_references_untouched() {
        let html = r##"<a href="#top">t</a><a href="mailto:a@b.com">m</a><img src="data:image/png;base64,AA=="><a href="unmapped.html">u</a>"##;
        assert_eq!(run(html, &[]), html);
    }

    #[test]
    fn declared_charset_preserves_non_ascii_bytes_while_rewriting() {
        // A windows-1252 document: the 0xE9 (é) inside the <style> comment must
        // survive as the same byte while the url() is rewritten. Decoding it as
        // UTF-8 by default would corrupt it, so the charset must be honored.
        let mut input = b"<style>/*".to_vec();
        input.push(0xE9);
        input.extend_from_slice(b"*/a{background:url(bg.png)}</style>");

        let out = rewrite_html(
            &input,
            &base(),
            Some("windows-1252"),
            None,
            &resolver(&[("http://h/dir/bg.png", "bg.png")]),
        );

        let mut expected = b"<style>/*".to_vec();
        expected.push(0xE9);
        expected.extend_from_slice(b"*/a{background:url(\"bg.png\")}</style>");
        assert_eq!(out, expected);
    }

    #[test]
    fn untouched_markup_passes_through_byte_identical() {
        // Comments, doctype, odd casing/quoting, void elements, entities, and an
        // unmapped href must all survive unchanged.
        let html = concat!(
            "<!DOCTYPE html>\n<!-- keep me --><HTML lang=EN>\n",
            "<A HREF='unmapped'   data-x=1 >hi</A>\n",
            "<img src=\"nomatch.png\" alt=\"a &amp; b\">\n",
            "<p title='&quot;q&quot;'>&copy;</p>\n</HTML>\n"
        );
        assert_eq!(run(html, &[]), html);
    }

    #[test]
    fn decode_entities_handles_refs_and_leaves_the_rest() {
        assert!(matches!(decode_entities("no entities"), Cow::Borrowed(_)));
        assert_eq!(decode_entities("a&amp;b"), "a&b");
        assert_eq!(decode_entities("&lt;&gt;&quot;&apos;"), "<>\"'");
        assert_eq!(decode_entities("x&#38;y&#x26;z"), "x&y&z");
        // Unrecognized or malformed references are preserved verbatim.
        assert_eq!(decode_entities("a&copy;b"), "a&copy;b");
        assert_eq!(decode_entities("a&b&c"), "a&b&c");
        assert_eq!(
            decode_entities("q=a&verylongthing;b"),
            "q=a&verylongthing;b"
        );
    }

    #[test]
    fn amp_entity_in_query_string_resolves() {
        // lol_html hands attribute values back with character references still
        // encoded, so a multi-parameter query (`?a=1&amp;b=2` — how every such
        // URL is written in HTML) must be entity-decoded before it can match a
        // map keyed by the real URL. Without this, the reference stays live and
        // the extracted page fetches it from the network.
        assert_eq!(
            run(
                r#"<link rel="stylesheet" href="http://h/w/load.php?a=1&amp;b=2">"#,
                &[("http://h/w/load.php?a=1&b=2", "w/load.css")]
            ),
            r#"<link rel="stylesheet" href="w/load.css">"#
        );
        // In srcset, too.
        assert_eq!(
            run(
                r#"<img srcset="http://h/i.php?w=1&amp;h=2 2x">"#,
                &[("http://h/i.php?w=1&h=2", "i.png")]
            ),
            r#"<img srcset="i.png 2x">"#
        );
        // Numeric character references decode as well.
        assert_eq!(
            run(
                r#"<a href="http://h/x?a=1&#38;b=2">z</a>"#,
                &[("http://h/x?a=1&b=2", "x.html")]
            ),
            r#"<a href="x.html">z</a>"#
        );
    }
}
