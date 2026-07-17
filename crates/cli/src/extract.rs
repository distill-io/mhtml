//! The `mhtml extract <file> [-o DIR] [--strict]` subcommand: unpack an archive
//! into a directory tree that mirrors the original URL hierarchy, rewriting
//! HTML/CSS references to point at the extracted files so the entry document
//! renders offline.
//!
//! Two passes over the (lenient) part iterator: pass 1 assigns each part its
//! output path, builds the normalized-URL rewrite map, and writes every
//! non-`text/html`/`text/css` part immediately; pass 2 rewrites the buffered
//! HTML/CSS parts against the completed map and writes them. See [`run`].

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use mhtml::{Archive, content_id_to_cid_url};
use url::Url;

use mhtml_serve::bundle::Bundle;
use mhtml_serve::ctype::content_type_essence;
use mhtml_serve::entry::select_entry;
use mhtml_serve::naming::NamingStrategy;
use mhtml_serve::rewrite_css::{css_encoding, rewrite_css};
use mhtml_serve::rewrite_html::rewrite_html;

use crate::naming::{Dedup, output_path};
use crate::resolve::Resolver;

/// What the extraction achieved, mapped by `main` to a process exit code.
pub enum Outcome {
    /// The archive was fully extracted (exit 0).
    Success,
    /// The archive was truncated/corrupt: in the default mode the salvaged
    /// files were kept, under `--strict` the output dir was removed (exit 1).
    Failed,
}

/// The two text formats rewritten in pass 2; every other type is written
/// verbatim in pass 1.
enum TextKind {
    Html,
    Css,
}

/// A buffered text part awaiting its pass-2 rewrite: the decoded body plus the
/// metadata needed to resolve its references and place it on disk.
struct Pending {
    kind: TextKind,
    body: Vec<u8>,
    charset: Option<String>,
    /// The raw `Content-Location`, used as the rewrite base URL.
    location: Option<String>,
    /// The already-assigned, output-root-relative path.
    path: PathBuf,
}

/// Classify a part by its content-type (parameters ignored). `Some` marks the
/// two formats buffered for rewriting; `None` means write-through.
fn text_kind(content_type: &str) -> Option<TextKind> {
    match content_type_essence(content_type).as_str() {
        "text/html" => Some(TextKind::Html),
        "text/css" => Some(TextKind::Css),
        _ => None,
    }
}

/// The default output directory for an input archive: its filename stem, as a
/// sibling of the input (`/a/b/page.mhtml` → `/a/b/page`).
pub fn default_out_dir(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or(input.as_os_str());
    match input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(stem),
        _ => PathBuf::from(stem),
    }
}

/// Insert a normalized-URL → path mapping, first-occurrence-wins. Unparseable
/// URLs are skipped (they can never match a `url`-resolved reference anyway).
fn insert_key(map: &mut HashMap<String, PathBuf>, raw: Option<&str>, path: &Path) {
    if let Some(raw) = raw
        && let Some(key) = mhtml_serve::locate::normalize_url(raw)
    {
        map.entry(key).or_insert_with(|| path.to_path_buf());
    }
}

/// Write `bytes` to `out`/`rel`, creating parent directories as needed.
fn write_file(out: &Path, rel: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let full = out.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    fs::write(&full, bytes).with_context(|| format!("writing {}", full.display()))
}

/// Rewrite a buffered text part against the completed `map`. A part with no (or
/// an unparseable) `Content-Location` has no base to resolve against and is
/// returned verbatim.
fn rewrite_pending(p: &Pending, map: &HashMap<String, PathBuf>) -> Vec<u8> {
    let Some(base) = p.location.as_deref().and_then(|l| Url::parse(l).ok()) else {
        return p.body.clone();
    };
    let from_dir = p.path.parent().unwrap_or_else(|| Path::new(""));
    let resolver = Resolver::new(map, from_dir);
    let resolve = |b: &Url, raw: &str| resolver.resolve(b, raw);
    match p.kind {
        TextKind::Html => rewrite_html(&p.body, &base, p.charset.as_deref(), None, &resolve),
        // Decode with the declared charset (mirroring the HTML path) so a single
        // high byte in a comment/string doesn't suppress every url()/@import
        // rewrite; re-encode with the same charset to preserve those bytes.
        TextKind::Css => {
            let encoding = css_encoding(p.charset.as_deref());
            let (css, _, _) = encoding.decode(&p.body);
            let rewritten = rewrite_css(&css, &base, &resolve);
            encoding.encode(&rewritten).0.into_owned()
        }
    }
}

/// The output-root-relative path as a forward-slash URL reference.
fn path_to_url(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Escape a string for an HTML double-quoted attribute value.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A minimal `index.html` that meta-refreshes to `href` (already root-relative).
fn meta_refresh(href: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"refresh\" content=\"0; url={}\">\n",
        html_escape(href)
    )
}

/// True when `path` exists and is a directory containing at least one entry, or
/// exists but is not a directory (either way, unsafe to extract into).
fn occupied(path: &Path) -> anyhow::Result<bool> {
    if !path.is_dir() {
        return Ok(true);
    }
    let mut entries = fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(entries.next().is_some())
}

/// Remove the output directory for a `--strict` failure. Only a directory this
/// run created is removed; a pre-existing one is refused rather than deleted.
fn remove_output(out: &Path, created_root: bool) -> anyhow::Result<()> {
    if created_root {
        fs::remove_dir_all(out).with_context(|| format!("removing {}", out.display()))
    } else {
        bail!(
            "refusing to delete output directory {} that this run did not create",
            out.display()
        )
    }
}

/// The resource-naming mode for extraction.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Naming {
    /// Mirror the original URL hierarchy on disk (default; salvage-friendly).
    Mirror,
    /// Content-addressed flat `<hash>.<ext>` files plus a `manifest.json`.
    Hash,
}

/// A single `manifest.json` record: a resource's flat key, resolved MIME, and
/// original URL (normalized `Content-Location`, or `null` when it had none).
struct ManifestEntry {
    key: String,
    mime: String,
    url: Option<String>,
}

/// Extract `input` into `out` under `naming`. Mirror mode preserves the URL
/// hierarchy on disk (two-pass, salvage-friendly). Hash mode writes flat
/// content-hash files, a `manifest.json`, and a rewritten `index.html`.
/// `base_href` sets the entry document's emitted `<base>` and is only valid in
/// hash mode.
pub fn run(
    input: &Path,
    out: &Path,
    strict: bool,
    naming: Naming,
    base_href: Option<&str>,
) -> anyhow::Result<Outcome> {
    if matches!(naming, Naming::Mirror) && base_href.is_some() {
        bail!("--base-href is only supported with --naming hash");
    }
    let data = fs::read(input).with_context(|| format!("reading {}", input.display()))?;

    // Never clobber: refuse a non-empty (or non-directory) output target.
    let preexisting = out.exists();
    if preexisting && occupied(out)? {
        bail!(
            "output directory {} already exists and is not empty; pass -o to choose another",
            out.display()
        );
    }
    let created_root = !preexisting;
    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;

    match naming {
        Naming::Mirror => extract_mirror(&data, out, strict, created_root),
        Naming::Hash => extract_hash(&data, out, base_href),
    }
}

/// Content-hash extraction: parse the archive into a [`Bundle`], write one flat
/// `<hash>.<ext>` file per distinct key, a `manifest.json` describing every
/// resource, and the rewritten entry document at `index.html`. Bundle parsing is
/// all-or-nothing, so a malformed archive is an error rather
/// than a salvage.
fn extract_hash(data: &[u8], out: &Path, base_href: Option<&str>) -> anyhow::Result<Outcome> {
    let bundle = Bundle::from_bytes(data).context("parsing MHTML archive")?;

    // Serve-ready assets: one per resource, the entry carrying any <base href>.
    let assets = bundle.manifest(&NamingStrategy::ContentHash, base_href);
    let mut written: HashSet<&str> = HashSet::new();
    for asset in &assets {
        // Identical bytes share a key; write each distinct key once.
        if written.insert(asset.key.as_str()) {
            write_file(out, Path::new(&asset.key), &asset.bytes)?;
        }
        // The entry is also served at the conventional index.html entrypoint.
        if asset.is_entry {
            write_file(out, Path::new("index.html"), &asset.bytes)?;
        }
    }

    // manifest.json: every resource's key, MIME, and original URL.
    let entries: Vec<ManifestEntry> = (0..bundle.resource_count())
        .filter_map(|i| {
            let key = bundle.resource_key(i, &NamingStrategy::ContentHash)?;
            let resource = bundle.get_index(i)?;
            Some(ManifestEntry {
                key,
                mime: resource.mime.clone(),
                url: resource.url.clone(),
            })
        })
        .collect();
    write_file(
        out,
        Path::new("manifest.json"),
        manifest_json(&entries).as_bytes(),
    )?;

    if bundle.entry_index().is_some() {
        println!("{}", out.join("index.html").display());
    } else {
        eprintln!("warning: no entry document found; extracted resources only");
    }

    Ok(Outcome::Success)
}

/// Serialize `s` as a JSON string literal (surrounding quotes included).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render the manifest as a JSON array of `{key, mime, url}` objects (`url` is
/// `null` for a resource that carried no `Content-Location`).
fn manifest_json(entries: &[ManifestEntry]) -> String {
    if entries.is_empty() {
        return "[]\n".to_string();
    }
    let items: Vec<String> = entries
        .iter()
        .map(|e| {
            let url = match &e.url {
                Some(u) => json_string(u),
                None => "null".to_string(),
            };
            format!(
                "  {{\"key\": {}, \"mime\": {}, \"url\": {}}}",
                json_string(&e.key),
                json_string(&e.mime),
                url
            )
        })
        .collect();
    format!("[\n{}\n]\n", items.join(",\n"))
}

/// Mirror-path extraction. See the module docs for the two-pass flow.
fn extract_mirror(
    data: &[u8],
    out: &Path,
    strict: bool,
    created_root: bool,
) -> anyhow::Result<Outcome> {
    let archive = Archive::parse(data).context("parsing MHTML archive")?;

    // Pass 1: assign paths, build the rewrite map, write non-text parts.
    let mut dedup = Dedup::new();
    let mut map: HashMap<String, PathBuf> = HashMap::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut content_types: Vec<String> = Vec::new();
    let mut assigned: Vec<PathBuf> = Vec::new();
    let mut written: Vec<PathBuf> = Vec::new();
    let mut parse_error: Option<mhtml::Error> = None;
    let mut write_failed = false;

    for (index, item) in archive.parts().enumerate() {
        let part = match item {
            Ok(p) => p,
            Err(e) => {
                parse_error = Some(e);
                break;
            }
        };
        // Undecodable body (e.g. invalid base64): skip this part, keep going.
        let Ok(body) = part.body() else {
            continue;
        };

        let path = output_path(
            part.content_location.as_deref(),
            part.content_id.as_deref(),
            &part.content_type,
            index,
            &mut dedup,
        );
        insert_key(&mut map, part.content_location.as_deref(), &path);
        if let Some(id) = part.content_id.as_deref()
            && let Some(cid) = content_id_to_cid_url(id)
        {
            insert_key(&mut map, Some(&cid), &path);
        }
        content_types.push(part.content_type.clone());
        assigned.push(path.clone());

        match text_kind(&part.content_type) {
            Some(kind) => pending.push(Pending {
                kind,
                body: body.into_owned(),
                charset: part.charset.clone(),
                location: part.content_location.clone(),
                path,
            }),
            // A per-part write failure (e.g. a Content-Location whose path
            // collides with a sibling used as a directory) is treated like an
            // unsalvageable body: warn, skip it, and keep extracting the rest,
            // rather than aborting the whole archive with a raw OS error.
            None => match write_file(out, &path, &body) {
                Ok(()) => written.push(path),
                Err(e) => {
                    eprintln!("warning: skipping {}: {e:#}", path.display());
                    write_failed = true;
                }
            },
        }
    }

    // Strict mode is all-or-nothing: any parse error unwinds the whole extract.
    if let Some(e) = &parse_error
        && strict
    {
        remove_output(out, created_root)?;
        eprintln!("error: strict extraction aborted: {e}");
        return Ok(Outcome::Failed);
    }

    // Pass 2: rewrite and write the buffered text parts.
    for p in &pending {
        let bytes = rewrite_pending(p, &map);
        write_file(out, &p.path, &bytes)?;
        written.push(p.path.clone());
    }

    // Entry point: redirect the output root at the main resource unless it is
    // already the root `index.html`.
    let ct_refs: Vec<&str> = content_types.iter().map(String::as_str).collect();
    let mut entry_index: Option<PathBuf> = None;
    match select_entry(&ct_refs).and_then(|i| assigned.get(i)) {
        Some(entry) => {
            let index = Path::new("index.html");
            if entry.as_path() != index {
                write_file(out, index, meta_refresh(&path_to_url(entry)).as_bytes())?;
                written.push(index.to_path_buf());
            }
            entry_index = Some(out.join("index.html"));
        }
        None => eprintln!("warning: no entry document found; extracted parts only"),
    }

    if let Some(e) = &parse_error {
        eprintln!("warning: archive truncated or corrupt: {e}");
        eprintln!("salvaged {} file(s):", written.len());
        for w in &written {
            eprintln!("  {}", out.join(w).display());
        }
        return Ok(Outcome::Failed);
    }

    // Some parts could not be written (already warned about, per part); the
    // salvaged files stand, but the run is a partial success (exit non-zero).
    if write_failed {
        return Ok(Outcome::Failed);
    }

    if let Some(entry) = entry_index {
        println!("{}", entry.display());
    }
    Ok(Outcome::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_out_dir_is_stem_sibling() {
        assert_eq!(
            default_out_dir(Path::new("/a/b/page.mhtml")),
            PathBuf::from("/a/b/page")
        );
        assert_eq!(
            default_out_dir(Path::new("page.mht")),
            PathBuf::from("page")
        );
    }

    #[test]
    fn text_kind_matches_html_and_css_only() {
        assert!(matches!(text_kind("text/html"), Some(TextKind::Html)));
        assert!(matches!(
            text_kind("text/html; charset=utf-8"),
            Some(TextKind::Html)
        ));
        assert!(matches!(text_kind("TEXT/CSS"), Some(TextKind::Css)));
        assert!(text_kind("image/png").is_none());
        assert!(text_kind("application/xhtml+xml").is_none());
    }

    #[test]
    fn path_to_url_uses_forward_slashes() {
        assert_eq!(
            path_to_url(&PathBuf::from("example.com/img/logo.png")),
            "example.com/img/logo.png"
        );
    }

    #[test]
    fn css_rewrite_honors_declared_charset_for_non_utf8_body() {
        // A windows-1252 stylesheet: the 0xE9 (é) in the comment makes it
        // invalid UTF-8, but the url() must still be rewritten by decoding the
        // body with its declared charset, and the high byte must round-trip.
        let mut body = b"/*".to_vec();
        body.push(0xE9);
        body.extend_from_slice(b"*/a{background:url(bg.png)}");

        let mut map: HashMap<String, PathBuf> = HashMap::new();
        map.insert(
            "http://h/dir/bg.png".to_string(),
            PathBuf::from("h/dir/bg.png"),
        );
        let p = Pending {
            kind: TextKind::Css,
            body,
            charset: Some("windows-1252".to_string()),
            location: Some("http://h/dir/page.css".to_string()),
            path: PathBuf::from("h/dir/page.css"),
        };

        let mut expected = b"/*".to_vec();
        expected.push(0xE9);
        expected.extend_from_slice(b"*/a{background:url(\"bg.png\")}");
        assert_eq!(rewrite_pending(&p, &map), expected);
    }

    #[test]
    fn meta_refresh_escapes_and_points_at_href() {
        let html = meta_refresh("a b/\"x\".html");
        assert!(html.contains("url=a b/&quot;x&quot;.html"));
        assert!(html.contains("http-equiv=\"refresh\""));
    }

    #[test]
    fn json_string_escapes_quotes_and_backslashes() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("tab\there"), "\"tab\\there\"");
    }

    #[test]
    fn manifest_json_serializes_entries_and_null_url() {
        let entries = vec![
            ManifestEntry {
                key: "abc.png".into(),
                mime: "image/png".into(),
                url: Some("http://h/x.png".into()),
            },
            ManifestEntry {
                key: "def.html".into(),
                mime: "text/html".into(),
                url: None,
            },
        ];
        let json = manifest_json(&entries);
        assert!(json.contains("\"key\": \"abc.png\""), "{json}");
        assert!(json.contains("\"mime\": \"image/png\""), "{json}");
        assert!(json.contains("\"url\": \"http://h/x.png\""), "{json}");
        assert!(json.contains("\"url\": null"), "{json}");
        assert!(json.trim_start().starts_with('['), "{json}");
        assert!(json.trim_end().ends_with(']'), "{json}");
    }

    #[test]
    fn manifest_json_empty_is_empty_array() {
        assert_eq!(manifest_json(&[]), "[]\n");
    }
}
