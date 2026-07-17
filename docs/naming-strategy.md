# Configurable resource-URL naming strategy

How an archive's resources are *referenced and stored* is configurable, so the
same bundle can be extracted into a URL-mirroring tree (the historical default)
or into a flat, content-addressed layout whose file names double as S3 object
keys. Two strategies, one mechanism.

- **MirrorPath** — preserve the URL path hierarchy (`host/dir/file.ext`). This is
  the historical extract/serve behavior: relative references resolve the way the
  browser resolved them online.
- **ContentHash** — content-addressed. Every resource is stored and referenced as
  `<sha256-hex>.<ext>`, `ext` derived from its MIME type. The URL is ignored
  entirely (which also sidesteps mislabeled-extension URLs). Flat namespace →
  ideal S3 keys; identical bytes collapse to one key.

Invariant honored everywhere: references written into rewritten HTML/CSS are
**relative and never start with `/`**. ContentHash emits the bare key
`<hash>.<ext>`; MirrorPath emits a `../`-relative path between two keys. This is
what lets both a bare relative layout and a CDN/bucket prefix work:

- A stylesheet's `url(...)`/`@import` refs resolve against the **stylesheet's own
  URL**. With flat co-location every hash key sits in the same prefix, so a bare
  `<hash>.<ext>` next to it just resolves — no `<base>` needed.
- The entry HTML's refs resolve against its `<base href>` when one is set (see
  §4), so the whole page can be served from an S3/CDN prefix; with no base they
  resolve relative to wherever the entry document itself is served.

---

## 1. `mhtml_serve::naming`

`crates/serve/src/naming.rs` is the single source of truth for the naming logic
and for the content-type→extension map. It is pure string/byte mapping — no I/O.
(`sha2 = "0.10"` backs the hashing; SHA-256 is not hand-rolled.)

```rust
/// How a resource is referenced and stored when served or uploaded.
pub enum NamingStrategy {
    /// Preserve the URL path hierarchy (`host/dir/file.ext`).
    MirrorPath,
    /// Content-addressed flat key (`<sha256>.<ext>`), ignoring the URL.
    ContentHash,
}

/// Lowercase-hex SHA-256 of `bytes`. The ContentHash key stem.
pub fn content_hash(bytes: &[u8]) -> String;

/// File extension for a `Content-Type` (parameters ignored), `"bin"` fallback.
/// Single source of truth: the CLI's disk `naming` delegates here.
pub fn ext_for_mime(content_type: &str) -> &'static str;

/// The relative serving/storage KEY for one resource under `strategy`.
/// - ContentHash: `format!("{}.{}", content_hash(body), ext_for_mime(mime))`
///   — always `Some` (the URL is ignored, so it need not exist).
/// - MirrorPath: a URL-derived relative path (`host/dir/file.ext`), or `None`
///   when `url` is `None`/unparseable/pathless (e.g. `cid:`).
pub fn resource_key(
    strategy: &NamingStrategy,
    url: Option<&str>,
    mime: &str,
    body: &[u8],
) -> Option<String>;

/// The reference to write into a document stored at `from_key` when it points at
/// the resource stored at `to_key`. Always relative, never leading `/`.
/// - ContentHash: the bare `to_key` (flat co-location / entry `<base href>`
///   resolve it); the referrer is irrelevant.
/// - MirrorPath: the relative path from `dir(from_key)` to `to_key`
///   (`../img/x.png`).
pub fn reference(from_key: &str, to_key: &str, strategy: &NamingStrategy) -> String;

/// The `/`-separated relative path from directory `from_dir` to file `to`,
/// climbing with `..`. The core the CLI's disk `resolve` delegates to after
/// flattening its `Path`s to `/`-joined strings.
pub fn relative_path(from_dir: &str, to: &str) -> String;
```

Private helpers: `mirror_key(url, mime) -> Option<String>` builds the MirrorPath
key (`host[_port]` head + percent-encoded path segments; trailing-slash/empty
final segment → `index.html`; extension forced to `css`/`js` for those
render-strict types via `forced_ext`, otherwise the path's own extension, else
inferred from MIME by `mirror_filename`). It is URL-safe by construction with no
filesystem sanitization.

**MirrorPath split note.** Two mirror mappings coexist by design, for two
targets. `mhtml_serve::naming::mirror_key` produces a URL-safe *serving* key (no dedup,
no Windows-reserved handling); `mhtml_cli::naming::output_path` produces a *disk-safe*
path (sanitization + dedup). The CLI's on-disk mirror extraction keeps using
`mhtml_cli::naming`; `mhtml_serve::naming::mirror_key` backs Bundle/wasm serving. The shared
pieces (`ext_for_mime`, `relative_path`) live in `mhtml_serve::naming` so both sides
share one implementation; the disk-safety layer stays CLI-only.

`forced_ext` (the css/js override for mislabeled *URL paths*) is MirrorPath-only.
ContentHash never needs it: its extension comes straight from the authoritative
MIME type, not the URL, so a `.php` that is really CSS still gets a `.css` key.

---

## 2. `mhtml_serve::bundle`

```rust
/// One resource ready to publish under a NamingStrategy.
pub struct Asset {
    pub key: String,          // where to PUT / serve it (relative, no leading '/')
    pub mime: String,
    pub charset: Option<String>,
    pub bytes: Vec<u8>,       // SERVED bytes: rewritten for html/css, raw otherwise
    pub is_entry: bool,       // the document a server exposes at `/`
}

impl Bundle {
    /// Number of resources (parts), in archive order.
    pub fn resource_count(&self) -> usize;

    /// Storage/serving key for resource `index` under `strategy`.
    /// `None` for an invalid index, or — under MirrorPath — a resource with no
    /// usable URL (ContentHash always yields `Some`). The hash is over the
    /// resource's decoded ORIGINAL body, so the key is stable regardless of any
    /// later rewriting of the served bytes.
    pub fn resource_key(&self, index: usize, strategy: &NamingStrategy) -> Option<String>;

    /// Serve-ready bytes for resource `index` under `strategy`. `None` only for
    /// an invalid index. html/css references that resolve in-bundle become
    /// `reference(doc_key, target_key, strategy)` + the original fragment;
    /// out-of-bundle refs are untouched; every other MIME type is returned
    /// verbatim. `base_href` sets the emitted `<base>` on html output only.
    pub fn rewritten(
        &self,
        index: usize,
        strategy: &NamingStrategy,
        base_href: Option<&str>,
    ) -> Option<Vec<u8>>;

    /// Every resource as an `Asset` for upload. `base_href` is applied to the
    /// ENTRY document only (subframes/css resolve bare refs by flat co-location);
    /// pass `Some(prefix)` when the entry will be served under a CDN path rather
    /// than at `/`. Resources with no key under `strategy` (only possible under
    /// MirrorPath) are OMITTED.
    pub fn manifest(&self, strategy: &NamingStrategy, base_href: Option<&str>) -> Vec<Asset>;
}
```

**Hashing nuance (documented in code).** The ContentHash `key` hashes the
resource's **decoded ORIGINAL bytes** (`Resource::body`) — a stable content
identity with no dependency on rewrite ordering. `Asset::bytes` for html/css is
the **rewritten** variant. So a stylesheet is stored *under the hash of its
original bytes* but *serves its rewritten bytes*; the route key and the served
body stay consistent because both are indexed by the same resource.

Inside `rewritten`, the referrer's own key is computed once and every in-bundle
reference is re-pointed through `naming::reference`:

```rust
let from_key = self.resource_key(index, strategy).unwrap_or_default();
let closure = |b: &Url, raw: &str| -> Option<String> {
    let (target, fragment) = resolve_reference(b, raw)?;
    let ti = *self.by_url.get(&target)?;
    let to_key = self.resource_key(ti, strategy)?;
    Some(format!("{}{fragment}", naming::reference(&from_key, &to_key, strategy)))
};
```

The html branch passes `base_href` into `rewrite_html`; the css branch does not
(CSS has no `<base>`; its bare refs resolve against the CSS file's own URL). The
resolution base is the resource's own URL, else the archive's
`Snapshot-Content-Location`; a document with neither is returned verbatim.

---

## 3. `mhtml_serve::rewrite_html` — base_href

```rust
pub fn rewrite_html<R>(
    html: &[u8],
    base: &Url,
    charset: Option<&str>,
    base_href: Option<&str>,
    resolve: &R,
) -> Vec<u8>
where R: Fn(&Url, &str) -> Option<String>;
```

- **Resolution is unchanged.** The document's effective base still comes from its
  Content-Location joined with any original `<base href>`, and all reference
  resolution uses it. `base_href` never affects which targets are found — it only
  changes what `<base>` the output carries.
- **`base_href == None`** (default): keep the historical behavior — the `<base>`
  handler removes `href` (neutralized), refs stay relative to the document.
- **`base_href == Some(url)`**: the `<base>` handler sets `href = url`; if the
  document had no `<base>`, one is injected at the start of `<head>`. Rewritten
  refs remain the bare relative keys and now resolve against `url` (the S3/CDN
  prefix).

Both `mhtml_cli::extract::rewrite_pending` (always `None` on the disk-mirror path) and
`Bundle::rewritten` call `rewrite_html` with this `base_href` argument.

---

## 4. wasm surface (`crates/wasm/src/lib.rs`)

Strategy crosses the boundary as a string (`"hash"` → ContentHash, anything else
→ MirrorPath); `base_href` as `Option<String>`. Methods keep Rust's snake_case
names.

```rust
pub fn parse(bytes: &[u8]) -> Result<MhtmlArchive, JsError>;
pub fn entry_index(&self) -> Option<usize>;
pub fn entry_url(&self) -> Option<String>;
pub fn urls(&self) -> Vec<String>;
pub fn content_type(&self, url: &str) -> Option<String>;          // full header
pub fn body(&self, url: &str) -> Option<Vec<u8>>;                 // raw decoded
pub fn content_type_at(&self, index: usize) -> Option<String>;
pub fn body_at(&self, index: usize) -> Option<Vec<u8>>;
pub fn resource_count(&self) -> usize;
pub fn resource_key(&self, index: usize, strategy: &str) -> Option<String>;
pub fn resource_mime(&self, index: usize) -> Option<String>;      // essence, e.g. "image/png"
pub fn resource_charset(&self, index: usize) -> Option<String>;
pub fn resource_bytes(&self, index: usize) -> Option<Vec<u8>>;    // raw decoded original (for S3 PUT)
pub fn rewritten_at(&self, index: usize, strategy: &str, base_href: Option<String>) -> Option<Vec<u8>>;
pub fn entry_rewritten(&self, strategy: &str, base_href: Option<String>) -> Option<Vec<u8>>;
```

Two use patterns: **(a) list for S3 upload** — iterate `0..resource_count()`,
read `resource_key(i,"hash")`, `resource_mime(i)`, `resource_charset(i)`,
`resource_bytes(i)`. **(b) serve rewritten html/css** under a strategy +
base_href — `entry_rewritten` / `rewritten_at`.

---

## 5. cli surface

```
mhtml extract <file> -o DIR [--naming mirror|hash] [--base-href URL]
```

- `--naming mirror` (default): disk-mirror extraction via
  `mhtml_cli::naming::output_path` + `mhtml_cli::resolve::Resolver`. `--base-href` is not
  applicable here and is **rejected** in combination with `mirror`.
- `--naming hash`: content-addressed flat output, via `Bundle::from_bytes` +
  `Bundle::manifest(&NamingStrategy::ContentHash, base_href)`. It writes:
  - `DIR/<hash>.<ext>` — one file per distinct `Asset` key (identical bytes share
    a key and are written once).
  - `DIR/index.html` — the entry `Asset`'s served (rewritten) bytes, for local
    viewing; `<base href>` set iff `--base-href` was given.
  - `DIR/manifest.json` — a JSON array of `{ "key", "mime", "url" }` (`url` = the
    resource's normalized `Content-Location`, or `null`), the PUT plan for S3.

`main.rs` stays a thin clap adapter: a `Naming { Mirror, Hash }` value-enum plus
`--naming`/`--base-href` args, delegating to `extract::run(input, out, strict,
naming, base_href)`.

---

## 6. Node demos

`examples/node-server/server.js` — serves an archive straight from memory using
the content-hash strategy. The entry document is at `/` (bare `<hash>.<ext>` refs
resolve to `/<hash>.<ext>`, no `<base>` needed → `base_href = undefined`); every
other resource is at `/<hash>.<ext>` via a `key → index` map built from
`resource_key(i,"hash")`, served through `rewritten_at`.

`examples/s3-demo/` — the end-to-end S3 workflow. `hash-extract.js` (or
`mhtml extract … --naming hash`) writes the flat `<dir>`; `server.js` serves it
as a static bucket, reading `Content-Type` from `manifest.json`. See
[`../examples/s3-demo/README.md`](../examples/s3-demo/README.md).

---

## 7. S3 workflow

The ContentHash output is a ready-made bucket:

1. Extract: `mhtml extract page.mhtml -o out --naming hash`
   (add `--base-href https://cdn.example/assets/` to bake a bucket prefix into
   the entry's `<base>`).
2. Upload every asset to its key — the file **name is the object key**:
   `aws s3 cp out/ s3://my-bucket/ --recursive`. Because keys are the content
   hash, uploads are idempotent and dedupe across archives: identical bytes
   always produce the same key.
3. Serve the entry HTML either **co-located** in the bucket root (bare
   `<hash>.<ext>` refs resolve to sibling keys — no `<base>` needed) or from
   anywhere with a `<base href>` pointing at the bucket/CDN prefix (use
   `--base-href`, or set it per call via `Bundle::rewritten` /
   `entry_rewritten`). Either way every ref stays relative and never starts with
   `/`.

Programmatically, `Bundle::manifest(&NamingStrategy::ContentHash, base_href)`
returns one `Asset { key, mime, charset, bytes, is_entry }` per resource: PUT
each `bytes` at `key` with `Content-Type` from `mime`/`charset`, and the entry
`Asset`'s rewritten HTML references everything by its `<hash>.<ext>` key.
