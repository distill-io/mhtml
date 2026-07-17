//! The ready-to-serve archive model. [`Bundle::from_bytes`] parses an archive
//! (strict) into a flat list of [`Resource`]s plus the lookup
//! structure a server needs: request-URL matching (normalized, so default-port
//! and `cid:` variants resolve) and entry-point selection, with per-resource
//! MIME already resolved. [`Bundle::rewritten`] re-points a document's in-bundle
//! references at a serving route so it renders against the bundle.

use std::collections::HashMap;

use mhtml::{Archive, content_id_to_cid_url};
use url::Url;

use crate::entry::select_entry;
use crate::locate::{normalize_url, resolve_reference};
use crate::mime::mime_for;
use crate::naming::{self, NamingStrategy};
use crate::rewrite_css::{css_encoding, rewrite_css};
use crate::rewrite_html::rewrite_html;

/// One servable part: its normalized `Content-Location` (absent when the part
/// carried none), the MIME type to serve it with, its charset, and the decoded
/// body.
pub struct Resource {
    pub url: Option<String>,
    pub mime: String,
    pub charset: Option<String>,
    pub body: Vec<u8>,
}

/// One resource ready to publish under a [`NamingStrategy`]: its relative
/// storage/serving `key`, MIME and charset, the `bytes` to write at that key
/// (the *rewritten* variant for `text/html`/`text/css`, the raw decoded body
/// otherwise), and whether it is the archive's entry document. Produced by
/// [`Bundle::manifest`].
pub struct Asset {
    pub key: String,
    pub mime: String,
    pub charset: Option<String>,
    pub bytes: Vec<u8>,
    pub is_entry: bool,
}

/// A parsed archive in ready-to-serve form. Resources are addressed by request
/// URL (via [`Bundle::get`]) or index (via [`Bundle::get_index`]); the entry
/// document is pre-selected.
pub struct Bundle {
    resources: Vec<Resource>,
    /// Normalized location and `cid:` URL → resource index, first-occurrence
    /// wins (matching the extract policy).
    by_url: HashMap<String, usize>,
    entry: Option<usize>,
    /// `Snapshot-Content-Location`, the base for rewriting a document that has
    /// no `Content-Location` of its own.
    fallback_base: Option<String>,
}

impl Bundle {
    /// Parse `data` into a bundle. Strict: a malformed archive
    /// is an `Err` (never a panic).
    pub fn from_bytes(data: &[u8]) -> Result<Bundle, mhtml::Error> {
        let archive = Archive::parse(data)?;
        let parts = archive.parse_all()?;
        let fallback_base = archive.snapshot_content_location().map(str::to_string);

        let mut resources: Vec<Resource> = Vec::with_capacity(parts.len());
        let mut by_url: HashMap<String, usize> = HashMap::new();
        // Raw `Content-Type` strings drive entry selection (an absent header is
        // an empty string, never a document); mirrors the extract policy.
        let mut content_types: Vec<String> = Vec::with_capacity(parts.len());

        for part in parts {
            let index = resources.len();
            let url = part.content_location.as_deref().and_then(normalize_url);
            if let Some(key) = &url {
                by_url.entry(key.clone()).or_insert(index);
            }
            if let Some(id) = part.content_id.as_deref()
                && let Some(cid) = content_id_to_cid_url(id)
                && let Some(key) = normalize_url(&cid)
            {
                by_url.entry(key).or_insert(index);
            }
            let mime = mime_for(&part.content_type, part.content_location.as_deref());
            content_types.push(part.content_type);
            resources.push(Resource {
                url,
                mime,
                charset: part.charset,
                body: part.body,
            });
        }

        let ct_refs: Vec<&str> = content_types.iter().map(String::as_str).collect();
        let entry = select_entry(&ct_refs);

        Ok(Bundle {
            resources,
            by_url,
            entry,
            fallback_base,
        })
    }

    /// The index of the entry (main) resource, if the archive has one.
    pub fn entry_index(&self) -> Option<usize> {
        self.entry
    }

    /// The entry (main) resource, if the archive has one.
    pub fn entry(&self) -> Option<&Resource> {
        self.entry.and_then(|i| self.resources.get(i))
    }

    /// Look up a resource by request URL. The query is normalized first, so a
    /// default-port or otherwise non-canonical form still matches the part
    /// stored under its normalized `Content-Location`; `cid:` URLs also resolve.
    pub fn get(&self, url: &str) -> Option<&Resource> {
        let key = normalize_url(url)?;
        let index = *self.by_url.get(&key)?;
        self.resources.get(index)
    }

    /// The index of the resource addressable by `url` (normalized like
    /// [`Bundle::get`]), for pairing with the index-based [`Bundle::rewritten`].
    /// `None` when nothing matches.
    pub fn index_of(&self, url: &str) -> Option<usize> {
        let key = normalize_url(url)?;
        self.by_url.get(&key).copied()
    }

    /// Look up a resource by index (an entry may carry no `Content-Location`, so
    /// it is only reachable this way).
    pub fn get_index(&self, index: usize) -> Option<&Resource> {
        self.resources.get(index)
    }

    /// The normalized URLs (locations and `cid:`) under which resources are
    /// addressable.
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        self.by_url.keys().map(String::as_str)
    }

    /// The number of resources in the bundle (parts, in archive order).
    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }

    /// The relative storage/serving key for resource `index` under `strategy`
    /// (`<sha256>.<ext>` for [`NamingStrategy::ContentHash`], a URL-mirrored path
    /// for [`NamingStrategy::MirrorPath`]). `None` for an invalid index, or —
    /// under `MirrorPath` — a resource with no usable URL. The hash is over the
    /// resource's decoded *original* body, so the key is stable regardless of any
    /// later rewriting of the served bytes.
    pub fn resource_key(&self, index: usize, strategy: &NamingStrategy) -> Option<String> {
        let resource = self.resources.get(index)?;
        naming::resource_key(
            strategy,
            resource.url.as_deref(),
            &resource.mime,
            &resource.body,
        )
    }

    /// Serve-ready bytes for resource `index` under `strategy`. `None` only for
    /// an invalid index.
    ///
    /// `text/html` and `text/css` resources are rewritten: every reference that
    /// resolves to an in-bundle resource becomes that target's key under
    /// `strategy` (via [`naming::reference`], always relative and never leading
    /// `/`) plus the reference's original fragment; anything resolving outside
    /// the bundle is left untouched. Any other MIME type — or a document with no
    /// usable base URL — is returned verbatim. `base_href` sets the emitted
    /// `<base>` on `text/html` output (see [`rewrite_html`]); it is `None`-only
    /// for other types. The resolution base is the resource's own URL, else the
    /// archive's `Snapshot-Content-Location`.
    pub fn rewritten(
        &self,
        index: usize,
        strategy: &NamingStrategy,
        base_href: Option<&str>,
    ) -> Option<Vec<u8>> {
        let resource = self.resources.get(index)?;

        let base = resource
            .url
            .as_deref()
            .or(self.fallback_base.as_deref())
            .and_then(|b| Url::parse(b).ok());
        let Some(base) = base else {
            return Some(resource.body.clone());
        };

        // The referrer's own key; irrelevant under ContentHash (bare target key),
        // the diff origin under MirrorPath.
        let from_key = self.resource_key(index, strategy).unwrap_or_default();

        let closure = |b: &Url, raw: &str| -> Option<String> {
            let (target, fragment) = resolve_reference(b, raw)?;
            let target_index = *self.by_url.get(&target)?;
            let to_key = self.resource_key(target_index, strategy)?;
            Some(format!(
                "{}{fragment}",
                naming::reference(&from_key, &to_key, strategy)
            ))
        };

        let bytes = match resource.mime.as_str() {
            "text/html" => rewrite_html(
                &resource.body,
                &base,
                resource.charset.as_deref(),
                base_href,
                &closure,
            ),
            "text/css" => {
                // Decode/re-encode with the declared charset (mirroring extract)
                // so a stray high byte can't suppress every rewrite.
                let encoding = css_encoding(resource.charset.as_deref());
                let (css, _, _) = encoding.decode(&resource.body);
                let rewritten = rewrite_css(&css, &base, &closure);
                encoding.encode(&rewritten).0.into_owned()
            }
            _ => resource.body.clone(),
        };
        Some(bytes)
    }

    /// Every resource ready to publish under `strategy`, as [`Asset`]s. Each
    /// carries its relative `key`, MIME/charset, and the bytes to store there:
    /// the rewritten variant for `text/html`/`text/css`, the raw body otherwise.
    /// `base_href` is applied to the entry document only (other resources resolve
    /// their bare keys by flat co-location). Resources with no key under
    /// `strategy` (only possible under `MirrorPath`) are omitted.
    pub fn manifest(&self, strategy: &NamingStrategy, base_href: Option<&str>) -> Vec<Asset> {
        (0..self.resources.len())
            .filter_map(|index| {
                let key = self.resource_key(index, strategy)?;
                let is_entry = self.entry == Some(index);
                let bytes =
                    self.rewritten(index, strategy, is_entry.then_some(base_href).flatten())?;
                let resource = &self.resources[index];
                Some(Asset {
                    key,
                    mime: resource.mime.clone(),
                    charset: resource.charset.clone(),
                    bytes,
                    is_entry,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `multipart/related` archive: one HTML root plus one PNG
    /// subresource, both with `Content-Location`s.
    const SIMPLE: &[u8] = b"\
From: <Saved by Blink>\r\n\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/html\r\n\
Content-Location: http://example.com/index.html\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<img src=3D\"logo.png\">\r\n\
--B\r\n\
Content-Type: image/png\r\n\
Content-Location: http://example.com/logo.png\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--B--\r\n";

    #[test]
    fn from_bytes_reads_every_part() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        assert_eq!(bundle.resources.len(), 2);
    }

    #[test]
    fn resource_metadata_is_populated() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        let png = bundle.get_index(1).expect("second resource");
        assert_eq!(png.url.as_deref(), Some("http://example.com/logo.png"));
        assert_eq!(png.mime, "image/png");
        assert_eq!(
            png.body,
            vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn mime_falls_back_to_extension_when_content_type_absent() {
        // A part with no Content-Type: mime_for guesses from the .css location.
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Location: http://example.com/style.css\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
body{}\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        assert_eq!(bundle.get_index(0).expect("resource").mime, "text/css");
    }

    #[test]
    fn get_normalizes_the_query_url() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        // Explicit default port must still match the part stored without it.
        let hit = bundle
            .get("http://example.com:80/logo.png")
            .expect("default-port query matches");
        assert_eq!(hit.mime, "image/png");
        assert!(bundle.get("http://example.com/missing").is_none());
    }

    #[test]
    fn index_of_maps_url_to_resource_index() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        assert_eq!(bundle.index_of("http://example.com/logo.png"), Some(1));
        // Normalized like get(): a default-port query still matches.
        assert_eq!(bundle.index_of("http://example.com:80/logo.png"), Some(1));
        assert!(bundle.index_of("http://example.com/missing").is_none());
    }

    #[test]
    fn get_resolves_cid_urls() {
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: image/png\r\n\
Content-ID: <img1@example.com>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        let hit = bundle.get("cid:img1@example.com").expect("cid resolves");
        assert_eq!(hit.mime, "image/png");
    }

    #[test]
    fn duplicate_locations_are_first_wins() {
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/plain\r\n\
Content-Location: http://example.com/dup\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
first\r\n\
--B\r\n\
Content-Type: text/plain\r\n\
Content-Location: http://example.com/dup\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
second\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        assert_eq!(
            bundle.get("http://example.com/dup").expect("hit").body,
            b"first"
        );
    }

    #[test]
    fn entry_is_the_first_html_part() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        assert_eq!(bundle.entry_index(), Some(0));
        assert_eq!(bundle.entry().expect("entry").mime, "text/html");
    }

    #[test]
    fn lone_image_is_the_entry() {
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: image/jpeg\r\n\
Content-Location: http://example.com/photo.jpg\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
/9j/2wA=\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        assert_eq!(bundle.entry_index(), Some(0));
    }

    #[test]
    fn entry_without_location_is_reachable_by_index() {
        // The HTML root carries no Content-Location, so it is only addressable
        // via its (entry) index, not get().
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/html\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
<html></html>\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        let i = bundle.entry_index().expect("entry");
        let entry = bundle.get_index(i).expect("reachable by index");
        assert_eq!(entry.url, None);
        assert_eq!(entry.mime, "text/html");
    }

    #[test]
    fn content_hash_rewritten_entry_references_hash_key() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        let out = bundle
            .rewritten(0, &NamingStrategy::ContentHash, None)
            .expect("valid index");
        let html = String::from_utf8(out).expect("utf-8");
        // logo.png resolves to the in-bundle PNG; under ContentHash its reference
        // becomes the bare "<sha256>.png" key (relative, no leading '/').
        let png = bundle.get_index(1).expect("png");
        let key = format!("{}.png", naming::content_hash(&png.body));
        assert!(html.contains(&format!("src=\"{key}\"")), "got: {html}");
        assert!(!html.contains("http://example.com/logo.png"), "got: {html}");
    }

    #[test]
    fn mirror_rewritten_entry_reference_is_relative_path() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        let out = bundle
            .rewritten(0, &NamingStrategy::MirrorPath, None)
            .expect("valid index");
        let html = String::from_utf8(out).expect("utf-8");
        // entry example.com/index.html -> logo example.com/logo.png => "logo.png".
        assert!(html.contains("src=\"logo.png\""), "got: {html}");
    }

    #[test]
    fn content_hash_rewritten_preserves_fragment_and_leaves_external_untouched() {
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/html\r\n\
Content-Location: http://example.com/index.html\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
<a href=\"style.css#top\">x</a><a href=\"http://other.com/x\">y</a>\r\n\
--B\r\n\
Content-Type: text/css\r\n\
Content-Location: http://example.com/style.css\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
body{}\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        let css = bundle.get_index(1).expect("css");
        let key = format!("{}.css", naming::content_hash(&css.body));
        let html = String::from_utf8(
            bundle
                .rewritten(0, &NamingStrategy::ContentHash, None)
                .expect("valid"),
        )
        .expect("utf-8");
        assert!(
            html.contains(&format!("href=\"{key}#top\"")),
            "fragment preserved: {html}"
        );
        assert!(
            html.contains("href=\"http://other.com/x\""),
            "external untouched: {html}"
        );
    }

    #[test]
    fn content_hash_rewritten_css_url_points_at_image_hash_key() {
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/css\r\n\
Content-Location: http://example.com/style.css\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
a{background:url(logo.png)}\r\n\
--B\r\n\
Content-Type: image/png\r\n\
Content-Location: http://example.com/logo.png\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        let png = bundle.get_index(1).expect("png");
        let key = format!("{}.png", naming::content_hash(&png.body));
        let css = String::from_utf8(
            bundle
                .rewritten(0, &NamingStrategy::ContentHash, None)
                .expect("valid"),
        )
        .expect("utf-8");
        assert!(css.contains(&format!("url(\"{key}\")")), "got: {css}");
    }

    #[test]
    fn resource_key_reflects_strategy() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        let png = bundle.get_index(1).expect("png");
        assert_eq!(
            bundle.resource_key(1, &NamingStrategy::ContentHash),
            Some(format!("{}.png", naming::content_hash(&png.body)))
        );
        assert_eq!(
            bundle
                .resource_key(1, &NamingStrategy::MirrorPath)
                .as_deref(),
            Some("example.com/logo.png")
        );
        assert!(
            bundle
                .resource_key(99, &NamingStrategy::ContentHash)
                .is_none()
        );
    }

    #[test]
    fn manifest_has_one_asset_per_resource_with_served_bytes() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        let assets = bundle.manifest(&NamingStrategy::ContentHash, None);
        assert_eq!(assets.len(), 2);

        let png = bundle.get_index(1).expect("png");
        let png_key = format!("{}.png", naming::content_hash(&png.body));

        let entry = assets.iter().find(|a| a.is_entry).expect("entry asset");
        assert_eq!(entry.mime, "text/html");
        assert_eq!(
            entry.key,
            bundle
                .resource_key(0, &NamingStrategy::ContentHash)
                .unwrap()
        );
        // The entry's served bytes are the rewritten variant: the img now points
        // at the image's content-hash key.
        let entry_html = String::from_utf8(entry.bytes.clone()).expect("utf-8");
        assert!(
            entry_html.contains(&png_key),
            "entry rewritten: {entry_html}"
        );

        let image = assets.iter().find(|a| !a.is_entry).expect("image asset");
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.key, png_key);
        // A non-document asset carries the raw decoded body verbatim.
        assert_eq!(image.bytes, png.body);
    }

    #[test]
    fn base_href_sets_base_on_entry_and_keeps_refs_relative() {
        let data = b"\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: text/html\r\n\
Content-Location: http://example.com/index.html\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
<html><head><title>t</title></head><body><img src=\"logo.png\"></body></html>\r\n\
--B\r\n\
Content-Type: image/png\r\n\
Content-Location: http://example.com/logo.png\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--B--\r\n";
        let bundle = Bundle::from_bytes(data).expect("archive parses");
        let png = bundle.get_index(1).expect("png");
        let key = format!("{}.png", naming::content_hash(&png.body));
        let html = String::from_utf8(
            bundle
                .rewritten(
                    0,
                    &NamingStrategy::ContentHash,
                    Some("https://cdn.example/p/"),
                )
                .expect("valid"),
        )
        .expect("utf-8");
        assert!(
            html.contains("<base href=\"https://cdn.example/p/\">"),
            "base injected: {html}"
        );
        // The reference stays the bare relative key (resolves against <base>).
        assert!(
            html.contains(&format!("src=\"{key}\"")),
            "relative ref: {html}"
        );
    }

    #[test]
    fn rewritten_non_document_passes_through_unmodified() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        // The PNG part is served verbatim.
        let out = bundle
            .rewritten(1, &NamingStrategy::ContentHash, None)
            .expect("valid index");
        assert_eq!(out, vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    }

    #[test]
    fn rewritten_invalid_index_is_none() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        assert!(
            bundle
                .rewritten(99, &NamingStrategy::ContentHash, None)
                .is_none()
        );
    }

    #[test]
    fn urls_lists_every_addressable_key() {
        let bundle = Bundle::from_bytes(SIMPLE).expect("archive parses");
        let mut urls: Vec<&str> = bundle.urls().collect();
        urls.sort_unstable();
        assert_eq!(
            urls,
            vec![
                "http://example.com/index.html",
                "http://example.com/logo.png"
            ]
        );
    }

    #[test]
    fn from_bytes_rejects_malformed_archive_without_panicking() {
        // A multipart header with no boundary parameter: the one hard
        // header-level error.
        let data = b"Content-Type: multipart/related\r\n\r\nbody\r\n";
        assert!(Bundle::from_bytes(data).is_err());
    }
}
