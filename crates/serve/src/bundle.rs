//! The ready-to-serve archive model. [`Bundle::from_bytes`] parses an archive
//! (strict) into a flat list of [`Resource`]s plus the lookup
//! structure a server needs: request-URL matching (normalized, so default-port
//! and `cid:` variants resolve) and entry-point selection, with per-resource
//! MIME already resolved. [`Bundle::rewritten`] re-points a document's in-bundle
//! references at a serving route so it renders against the bundle.

use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
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

/// [`NamingStrategy::ContentHash`] naming for a whole bundle, computed once on
/// first use: every resource's `<sha256>.<ext>` key, and the in-bundle resources
/// each one references.
struct HashNaming {
    keys: Vec<String>,
    children: Vec<Vec<usize>>,
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
    hash_naming: OnceCell<HashNaming>,
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
            hash_naming: OnceCell::new(),
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

    /// The relative storage/serving key for resource `index` under `strategy`:
    /// `<sha256>.<ext>` for [`NamingStrategy::ContentHash`], a URL-mirrored path
    /// for [`NamingStrategy::MirrorPath`]. `None` for an invalid index, or —
    /// under `MirrorPath` — a resource with no usable URL.
    ///
    /// Under `ContentHash` the hash of a `text/html`/`text/css` document is over
    /// its *rewritten* bytes, i.e. the bytes [`Bundle::rewritten`] serves, so it
    /// changes whenever anything the document renders with changes. Only a
    /// document in a reference cycle is the exception (see `hash_naming`).
    pub fn resource_key(&self, index: usize, strategy: &NamingStrategy) -> Option<String> {
        let resource = self.resources.get(index)?;
        match strategy {
            NamingStrategy::ContentHash => Some(self.hash_naming().keys[index].clone()),
            NamingStrategy::MirrorPath => naming::resource_key(
                strategy,
                resource.url.as_deref(),
                &resource.mime,
                &resource.body,
            ),
        }
    }

    /// The in-bundle resources that `index` references, in document order, each
    /// once; empty for anything but a `text/html`/`text/css` document. `None` for
    /// an invalid index.
    pub fn children(&self, index: usize) -> Option<&[usize]> {
        self.hash_naming().children.get(index).map(Vec::as_slice)
    }

    /// The lowercase-hex SHA-256 of resource `index`'s decoded original body.
    pub fn source_hash(&self, index: usize) -> Option<String> {
        self.resources
            .get(index)
            .map(|resource| naming::content_hash(&resource.body))
    }

    /// Serve-ready bytes for resource `index` under `strategy`. `None` only for
    /// an invalid index.
    ///
    /// `text/html`, `application/xhtml+xml`, and `text/css` resources are
    /// rewritten: every reference that
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
        self.resources.get(index)?;

        // The referrer's own key; irrelevant under ContentHash (bare target key),
        // the diff origin under MirrorPath.
        let from_key = self.resource_key(index, strategy).unwrap_or_default();

        let key_for = |target: usize| -> Option<String> {
            let to_key = self.resource_key(target, strategy)?;
            Some(naming::reference(&from_key, &to_key, strategy))
        };
        Some(
            self.rewrite_references(index, &key_for, base_href)
                .into_owned(),
        )
    }

    /// Resource `index`'s body with every in-bundle reference replaced by
    /// `key_for(target index)` plus its fragment; a reference `key_for` answers
    /// `None` for is left untouched. Only documents are rewritten (see
    /// [`Bundle::rewritten`]); anything else is borrowed as is.
    fn rewrite_references(
        &self,
        index: usize,
        key_for: &dyn Fn(usize) -> Option<String>,
        base_href: Option<&str>,
    ) -> Cow<'_, [u8]> {
        let resource = &self.resources[index];

        let base = resource
            .url
            .as_deref()
            .or(self.fallback_base.as_deref())
            .and_then(|b| Url::parse(b).ok());
        let Some(base) = base else {
            return Cow::Borrowed(&resource.body);
        };

        let closure = |b: &Url, raw: &str| -> Option<String> {
            let (target, fragment) = resolve_reference(b, raw)?;
            let target_index = *self.by_url.get(&target)?;
            Some(format!("{}{fragment}", key_for(target_index)?))
        };

        match resource.mime.as_str() {
            // xhtml counts as a document in entry.rs and naming::forced_ext too;
            // omitting it here made the entry we pick the one document never rewritten
            "text/html" | "application/xhtml+xml" => Cow::Owned(rewrite_html(
                &resource.body,
                &base,
                resource.charset.as_deref(),
                base_href,
                &closure,
            )),
            "text/css" => {
                // Decode/re-encode with the declared charset (mirroring extract)
                // so a stray high byte can't suppress every rewrite.
                let encoding = css_encoding(resource.charset.as_deref());
                let (css, _, _) = encoding.decode(&resource.body);
                let rewritten = rewrite_css(&css, &base, &closure);
                Cow::Owned(encoding.encode(&rewritten).0.into_owned())
            }
            _ => Cow::Borrowed(&resource.body),
        }
    }

    /// The in-bundle resources `index` references: a rewrite pass whose closure
    /// only records the targets it is asked about and changes nothing.
    fn children_of(&self, index: usize) -> Vec<usize> {
        let found: RefCell<Vec<usize>> = RefCell::new(Vec::new());
        let record = |target: usize| -> Option<String> {
            let mut found = found.borrow_mut();
            if !found.contains(&target) {
                found.push(target);
            }
            None
        };
        self.rewrite_references(index, &record, None);
        found.into_inner()
    }

    fn hash_naming(&self) -> &HashNaming {
        self.hash_naming.get_or_init(|| self.compute_hash_naming())
    }

    /// Keys are assigned bottom-up so that a document's key covers everything it
    /// renders with: a leaf is hashed as is, a document is hashed once all its
    /// children have keys, as its body rewritten with those keys. Documents that
    /// reference each other in a cycle have no bottom, so the cycle is keyed as
    /// a whole: each member's text is rewritten with the final keys of children
    /// outside the cycle and the *source* keys (`<sha256 of the body>.<ext>`) of
    /// those inside it, and a member's key is the hash of its own such text
    /// followed by a digest of every member's. A change anywhere in the cycle or
    /// below it therefore changes every member, while [`Bundle::rewritten`] still
    /// writes their final keys like everywhere else.
    fn compute_hash_naming(&self) -> HashNaming {
        let count = self.resources.len();
        let children: Vec<Vec<usize>> = (0..count).map(|i| self.children_of(i)).collect();

        let mut keys: Vec<Option<String>> = vec![None; count];
        // Components arrive after everything they reference, so a child outside
        // the component being keyed always has its key already.
        for component in strongly_connected_components(&children) {
            let is_cycle = component.len() > 1 || children[component[0]].contains(&component[0]);
            if !is_cycle {
                let index = component[0];
                let bytes = self.rewrite_references(index, &|target| keys[target].clone(), None);
                keys[index] = Some(naming::content_hash_key(
                    &self.resources[index].mime,
                    &bytes,
                ));
                continue;
            }

            let key_for = |target: usize| -> Option<String> {
                if component.contains(&target) {
                    let target = &self.resources[target];
                    return Some(naming::content_hash_key(&target.mime, &target.body));
                }
                keys[target].clone()
            };
            let texts: Vec<Vec<u8>> = component
                .iter()
                .map(|&index| self.rewrite_references(index, &key_for, None).into_owned())
                .collect();

            // sorted, so the order of the parts in the archive plays no role
            let mut text_hashes: Vec<String> = texts
                .iter()
                .map(|text| naming::content_hash(text))
                .collect();
            text_hashes.sort_unstable();
            let digest = naming::content_hash(text_hashes.concat().as_bytes());

            for (&index, mut text) in component.iter().zip(texts) {
                text.extend_from_slice(digest.as_bytes());
                keys[index] = Some(naming::content_hash_key(&self.resources[index].mime, &text));
            }
        }

        HashNaming {
            keys: keys.into_iter().flatten().collect(),
            children,
        }
    }

    /// Every resource ready to publish under `strategy`, as [`Asset`]s. Each
    /// carries its relative `key`, MIME/charset, and the bytes to store there:
    /// the rewritten variant for `text/html`/`text/css`, the raw body otherwise.
    /// No `<base>` is injected anywhere, so under [`NamingStrategy::ContentHash`]
    /// every asset's bytes hash to its key; a base belongs in a separate entry
    /// point copy. Resources with no key under `strategy` (only possible under
    /// `MirrorPath`) are omitted.
    pub fn manifest(&self, strategy: &NamingStrategy) -> Vec<Asset> {
        (0..self.resources.len())
            .filter_map(|index| {
                let key = self.resource_key(index, strategy)?;
                let is_entry = self.entry == Some(index);
                let bytes = self.rewritten(index, strategy, None)?;
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

/// Tarjan's strongly connected components over `children`, iterative so that a
/// long reference chain cannot overflow the stack. Components come out in
/// reverse topological order: each one after every component it references.
fn strongly_connected_components(children: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;
    let count = children.len();
    let mut visit_index = vec![UNVISITED; count];
    let mut lowlink = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<usize> = Vec::new();
    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut next_visit = 0;
    // the recursion, by hand: each entry is a node and how many of its children
    // have been visited so far
    let mut work: Vec<(usize, usize)> = Vec::new();

    let mut visit = |node: usize,
                     visit_index: &mut Vec<usize>,
                     lowlink: &mut Vec<usize>,
                     stack: &mut Vec<usize>,
                     on_stack: &mut Vec<bool>| {
        visit_index[node] = next_visit;
        lowlink[node] = next_visit;
        next_visit += 1;
        stack.push(node);
        on_stack[node] = true;
    };

    for root in 0..count {
        if visit_index[root] != UNVISITED {
            continue;
        }
        visit(
            root,
            &mut visit_index,
            &mut lowlink,
            &mut stack,
            &mut on_stack,
        );
        work.push((root, 0));

        while let Some(&(node, visited_children)) = work.last() {
            if let Some(&child) = children[node].get(visited_children) {
                work.last_mut().expect("just read").1 += 1;
                if visit_index[child] == UNVISITED {
                    visit(
                        child,
                        &mut visit_index,
                        &mut lowlink,
                        &mut stack,
                        &mut on_stack,
                    );
                    work.push((child, 0));
                } else if on_stack[child] {
                    lowlink[node] = lowlink[node].min(visit_index[child]);
                }
                continue;
            }

            work.pop();
            if let Some(&(parent, _)) = work.last() {
                lowlink[parent] = lowlink[parent].min(lowlink[node]);
            }
            if lowlink[node] == visit_index[node] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(component);
            }
        }
    }
    components
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

    /// The same archive with an `application/xhtml+xml` root — the MIME Blink
    /// writes for a document parsed as XML.
    const XHTML: &[u8] = b"\
From: <Saved by Blink>\r\n\
Content-Type: multipart/related; boundary=\"B\"\r\n\
\r\n\
--B\r\n\
Content-Type: application/xhtml+xml\r\n\
Content-Location: http://example.com/index.xhtml\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<html xmlns=3D\"http://www.w3.org/1999/xhtml\"><body><img src=3D\"logo.png\"/></body></html>\r\n\
--B\r\n\
Content-Type: image/png\r\n\
Content-Location: http://example.com/logo.png\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--B--\r\n";

    /// A `multipart/related` archive of 7bit parts: `(Content-Type,
    /// Content-Location, body)` each.
    fn archive(parts: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut data = String::from("Content-Type: multipart/related; boundary=\"B\"\r\n\r\n");
        for (content_type, location, body) in parts {
            data.push_str(&format!(
                "--B\r\nContent-Type: {content_type}\r\nContent-Location: {location}\r\n\
                 Content-Transfer-Encoding: 7bit\r\n\r\n{body}\r\n"
            ));
        }
        data.push_str("--B--\r\n");
        data.into_bytes()
    }

    fn hash_key(bundle: &Bundle, index: usize) -> String {
        bundle
            .resource_key(index, &NamingStrategy::ContentHash)
            .expect("valid index")
    }

    #[test]
    fn content_hash_key_of_a_document_changes_with_its_child() {
        let html = "<img src=\"logo.png\">";
        let with = |png: &str| {
            Bundle::from_bytes(&archive(&[
                ("text/html", "http://h/index.html", html),
                ("image/png", "http://h/logo.png", png),
            ]))
            .expect("archive parses")
        };
        let red = with("PNG red");
        let blue = with("PNG blue");
        // Same page bytes, different image: the page's identity must differ,
        // because the page is only the same page if it renders the same.
        assert_ne!(hash_key(&red, 0), hash_key(&blue, 0));
    }

    fn served(bundle: &Bundle, index: usize) -> String {
        let bytes = bundle
            .rewritten(index, &NamingStrategy::ContentHash, None)
            .expect("valid index");
        String::from_utf8(bytes).expect("utf-8")
    }

    /// page.html -> style.css -> logo.png
    fn chain() -> Bundle {
        Bundle::from_bytes(&archive(&[
            (
                "text/html",
                "http://h/page.html",
                "<link href=\"style.css\">",
            ),
            (
                "text/css",
                "http://h/style.css",
                "a{background:url(logo.png)}",
            ),
            ("image/png", "http://h/logo.png", "PNG"),
        ]))
        .expect("archive parses")
    }

    #[test]
    fn content_hash_key_is_the_hash_of_the_served_bytes() {
        let bundle = chain();
        for index in 0..3 {
            let key = hash_key(&bundle, index);
            let bytes = bundle
                .rewritten(index, &NamingStrategy::ContentHash, None)
                .expect("valid index");
            assert_eq!(
                key,
                naming::content_hash_key(&bundle.resources[index].mime, &bytes)
            );
        }
    }

    #[test]
    fn content_hash_document_references_its_child_by_the_childs_final_key() {
        let bundle = chain();
        let css_key = hash_key(&bundle, 1);
        // The stylesheet's key is over its rewritten text, not its source, so
        // the page naming the source key would point at nothing.
        let css_source_key = naming::content_hash_key("text/css", b"a{background:url(logo.png)}");
        assert_ne!(css_key, css_source_key);
        assert!(served(&bundle, 0).contains(&format!("href=\"{css_key}\"")));
    }

    #[test]
    fn children_are_the_in_bundle_references_in_order_each_once() {
        let bundle = Bundle::from_bytes(&archive(&[
            (
                "text/html",
                "http://h/page.html",
                "<img src=\"b.png\"><link href=\"s.css\"><img src=\"b.png\"><img src=\"http://other/x.png\">",
            ),
            ("text/css", "http://h/s.css", "a{background:url(b.png)}"),
            ("image/png", "http://h/b.png", "PNG"),
        ]))
        .expect("archive parses");
        assert_eq!(bundle.children(0), Some(&[2, 1][..]));
        assert_eq!(bundle.children(1), Some(&[2][..]));
        assert_eq!(bundle.children(2), Some(&[][..]));
        assert_eq!(bundle.children(3), None);
    }

    #[test]
    fn source_hash_is_over_the_original_body() {
        let bundle = chain();
        assert_eq!(
            bundle.source_hash(0),
            Some(naming::content_hash(b"<link href=\"style.css\">"))
        );
        assert_eq!(bundle.source_hash(3), None);
    }

    #[test]
    fn same_bytes_with_different_children_get_different_keys() {
        // a/style.css and b/style.css are byte-identical but url(img.png)
        // resolves to a different image under each directory.
        let bundle = Bundle::from_bytes(&archive(&[
            (
                "text/css",
                "http://h/a/style.css",
                "a{background:url(img.png)}",
            ),
            (
                "text/css",
                "http://h/b/style.css",
                "a{background:url(img.png)}",
            ),
            ("image/png", "http://h/a/img.png", "PNG A"),
            ("image/png", "http://h/b/img.png", "PNG B"),
        ]))
        .expect("archive parses");
        assert_ne!(hash_key(&bundle, 0), hash_key(&bundle, 1));
        assert!(served(&bundle, 0).contains(&hash_key(&bundle, 2)));
        assert!(served(&bundle, 1).contains(&hash_key(&bundle, 3)));
    }

    #[test]
    fn changing_a_leaf_changes_the_key_of_every_document_above_it() {
        let with = |png: &str| {
            Bundle::from_bytes(&archive(&[
                (
                    "text/html",
                    "http://h/page.html",
                    "<link href=\"style.css\">",
                ),
                (
                    "text/css",
                    "http://h/style.css",
                    "a{background:url(logo.png)}",
                ),
                ("image/png", "http://h/logo.png", png),
            ]))
            .expect("archive parses")
        };
        let before = with("PNG v1");
        let after = with("PNG v2");
        // Neither the page nor the stylesheet changed a byte, yet both are a
        // different object now, because what they render changed.
        for index in 0..3 {
            assert_ne!(
                hash_key(&before, index),
                hash_key(&after, index),
                "part {index}"
            );
        }
        // and each one is served pointing at the new key below it
        assert!(served(&after, 0).contains(&hash_key(&after, 1)));
        assert!(served(&after, 1).contains(&hash_key(&after, 2)));
    }

    #[test]
    fn documents_in_a_cycle_are_keyed_through_source_keys_and_served_with_final_keys() {
        let a = "@import url(b.css);";
        let b = "@import url(a.css);";
        let bundle = Bundle::from_bytes(&archive(&[
            ("text/html", "http://h/page.html", "<link href=\"a.css\">"),
            ("text/css", "http://h/a.css", a),
            ("text/css", "http://h/b.css", b),
        ]))
        .expect("archive parses");
        let a_key = hash_key(&bundle, 1);
        let b_key = hash_key(&bundle, 2);

        // There is no bottom to start from, so a is keyed over its text with
        // b's source key written in, plus a digest of the whole cycle (b's text
        // with a's source key in it): a change to either stylesheet, or to
        // anything either one references, changes both keys.
        let a_source_key = naming::content_hash_key("text/css", a.as_bytes());
        let b_source_key = naming::content_hash_key("text/css", b.as_bytes());
        let a_through_b = format!("@import url(\"{b_source_key}\");");
        let b_through_a = format!("@import url(\"{a_source_key}\");");
        assert_eq!(
            a_key,
            cycle_key("text/css", &a_through_b, &[&a_through_b, &b_through_a])
        );
        assert_ne!(a_key, b_key);

        // What is served carries the final keys, like outside a cycle.
        assert!(served(&bundle, 1).contains(&b_key));
        assert!(served(&bundle, 2).contains(&a_key));
        assert!(served(&bundle, 0).contains(&a_key));
    }

    /// Mirrors how `compute_hash_naming` keys a cycle member: its own text (the
    /// other members as source keys) followed by the digest of every member's
    /// such text, sorted so the order of the parts in the archive is irrelevant.
    fn cycle_key(mime: &str, own: &str, members: &[&str]) -> String {
        let mut member_hashes: Vec<String> = members
            .iter()
            .map(|text| naming::content_hash(text.as_bytes()))
            .collect();
        member_hashes.sort_unstable();
        let digest = naming::content_hash(member_hashes.concat().as_bytes());
        let mut keyed = own.as_bytes().to_vec();
        keyed.extend_from_slice(digest.as_bytes());
        naming::content_hash_key(mime, &keyed)
    }

    #[test]
    fn a_change_anywhere_in_a_cycle_changes_every_member() {
        // a -> b -> c -> a: a never names c, so a's own text cannot notice a
        // change in c; the cycle digest must.
        let with = |c: &str| {
            Bundle::from_bytes(&archive(&[
                ("text/html", "http://h/page.html", "<link href=\"a.css\">"),
                ("text/css", "http://h/a.css", "@import url(b.css);"),
                ("text/css", "http://h/b.css", "@import url(c.css);"),
                ("text/css", "http://h/c.css", c),
            ]))
            .expect("archive parses")
        };
        let before = with("@import url(a.css); p{color:red}");
        let after = with("@import url(a.css); p{color:blue}");
        for index in 0..4 {
            assert_ne!(
                hash_key(&before, index),
                hash_key(&after, index),
                "part {index}"
            );
        }
    }

    #[test]
    fn a_change_below_a_cycle_changes_its_members() {
        // a <-> b, and a also uses a stylesheet that uses an image: the image
        // is two steps below the cycle and outside it.
        let with = |png: &str| {
            Bundle::from_bytes(&archive(&[
                (
                    "text/css",
                    "http://h/a.css",
                    "@import url(b.css); @import url(d.css);",
                ),
                ("text/css", "http://h/b.css", "@import url(a.css);"),
                ("text/css", "http://h/d.css", "p{background:url(logo.png)}"),
                ("image/png", "http://h/logo.png", png),
            ]))
            .expect("archive parses")
        };
        let before = with("PNG v1");
        let after = with("PNG v2");
        for index in 0..4 {
            assert_ne!(
                hash_key(&before, index),
                hash_key(&after, index),
                "part {index}"
            );
        }
        // d is outside the cycle, so a names it by its final key even in a's
        // own hashed text, and serves it that way
        assert!(served(&after, 0).contains(&hash_key(&after, 2)));
    }

    #[test]
    fn document_referencing_itself_gets_a_key() {
        let bundle = Bundle::from_bytes(&archive(&[(
            "text/css",
            "http://h/a.css",
            "@import url(a.css);",
        )]))
        .expect("archive parses");
        assert!(served(&bundle, 0).contains(&hash_key(&bundle, 0)));
    }

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
    fn xhtml_entry_is_rewritten_like_html() {
        // entry.rs already treats application/xhtml+xml as a document, so a root
        // left unrewritten keeps absolute references and its subresources lose
        // their only pointer. lol_html keeps the self-closing slash, so routing
        // XML through the HTML rewriter does not cost well-formedness.
        let bundle = Bundle::from_bytes(XHTML).expect("archive parses");
        let out = bundle
            .rewritten(0, &NamingStrategy::ContentHash, None)
            .expect("valid index");
        let xhtml = String::from_utf8(out).expect("utf-8");
        let png = bundle.get_index(1).expect("png");
        let key = format!("{}.png", naming::content_hash(&png.body));
        assert!(xhtml.contains(&format!("src=\"{key}\"")), "got: {xhtml}");
        assert!(!xhtml.contains("src=\"logo.png\""), "got: {xhtml}");
        assert!(xhtml.contains("/>"), "self-closing tag lost: {xhtml}");
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
<link href=\"style.css#top\"><link href=\"http://other.com/x\">\r\n\
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
        let assets = bundle.manifest(&NamingStrategy::ContentHash);
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
            html.contains("<base href=\"https://cdn.example/p/\"/>"),
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
