//! Thin JS bindings; all behavior is implemented and tested in the serve crate.

use mhtml_serve::bundle::Bundle;
use mhtml_serve::mime::content_type_header;
use mhtml_serve::naming::NamingStrategy;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct MhtmlArchive(Bundle);

/// Map a JS strategy label to a [`NamingStrategy`]. `"hash"` selects the
/// content-addressed scheme; anything else is the URL-mirroring default.
fn strategy_for(label: &str) -> NamingStrategy {
    match label {
        "hash" => NamingStrategy::ContentHash,
        _ => NamingStrategy::MirrorPath,
    }
}

#[wasm_bindgen]
impl MhtmlArchive {
    pub fn parse(bytes: &[u8]) -> Result<MhtmlArchive, JsError> {
        Bundle::from_bytes(bytes)
            .map(MhtmlArchive)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    pub fn entry_index(&self) -> Option<usize> {
        self.0.entry_index()
    }

    pub fn entry_url(&self) -> Option<String> {
        self.0.entry().and_then(|r| r.url.clone())
    }

    pub fn urls(&self) -> Vec<String> {
        self.0.urls().map(str::to_string).collect()
    }

    pub fn content_type(&self, url: &str) -> Option<String> {
        self.0
            .get(url)
            .map(|r| content_type_header(&r.mime, r.charset.as_deref()))
    }

    pub fn body(&self, url: &str) -> Option<Vec<u8>> {
        self.0.get(url).map(|r| r.body.clone())
    }

    pub fn content_type_at(&self, index: usize) -> Option<String> {
        self.0
            .get_index(index)
            .map(|r| content_type_header(&r.mime, r.charset.as_deref()))
    }

    pub fn body_at(&self, index: usize) -> Option<Vec<u8>> {
        self.0.get_index(index).map(|r| r.body.clone())
    }

    /// The number of resources (parts) in the archive, in order.
    pub fn resource_count(&self) -> usize {
        self.0.resource_count()
    }

    /// The relative storage/serving key for resource `index` under `strategy`
    /// (`"hash"` or `"mirror"`).
    pub fn resource_key(&self, index: usize, strategy: &str) -> Option<String> {
        self.0.resource_key(index, &strategy_for(strategy))
    }

    pub fn resource_mime(&self, index: usize) -> Option<String> {
        self.0.get_index(index).map(|r| r.mime.clone())
    }

    pub fn resource_charset(&self, index: usize) -> Option<String> {
        self.0.get_index(index).and_then(|r| r.charset.clone())
    }

    /// The raw decoded original body of resource `index` (for uploading as-is).
    pub fn resource_bytes(&self, index: usize) -> Option<Vec<u8>> {
        self.0.get_index(index).map(|r| r.body.clone())
    }

    /// Serve-ready bytes for resource `index` under `strategy`, with `base_href`
    /// applied to the emitted `<base>` on HTML output only.
    pub fn rewritten_at(
        &self,
        index: usize,
        strategy: &str,
        base_href: Option<String>,
    ) -> Option<Vec<u8>> {
        self.0
            .rewritten(index, &strategy_for(strategy), base_href.as_deref())
    }

    /// Serve-ready bytes for the entry document under `strategy`.
    pub fn entry_rewritten(&self, strategy: &str, base_href: Option<String>) -> Option<Vec<u8>> {
        let index = self.0.entry_index()?;
        self.0
            .rewritten(index, &strategy_for(strategy), base_href.as_deref())
    }
}
