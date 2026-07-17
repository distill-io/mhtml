//! Reference resolution for rewriting: turn a raw HTML/CSS reference string,
//! resolved against the document's base URL, into the relative on-disk path of
//! the extracted target — or `None` to leave the reference untouched.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use url::Url;

/// Resolves references found in a single document against the rewrite map,
/// producing the relative on-disk path of the extracted target.
///
/// A `Resolver` is bound to one referencing document: `map` is the shared
/// normalized-URL → output-path index for the whole archive, and `from_dir` is
/// the output directory of the file being rewritten (references are made
/// relative to it).
pub struct Resolver<'a> {
    map: &'a HashMap<String, PathBuf>,
    from_dir: &'a Path,
}

impl<'a> Resolver<'a> {
    /// Bind a resolver to the archive's rewrite `map` and the output directory
    /// of the document currently being rewritten.
    pub fn new(map: &'a HashMap<String, PathBuf>, from_dir: &'a Path) -> Self {
        Self { map, from_dir }
    }

    /// Resolve a raw reference `raw` against document base `base`.
    ///
    /// Returns `Some(replacement)` when the reference resolves to a mapped
    /// target (the returned string is the relative path plus any original
    /// `#fragment`), or `None` to leave the reference untouched.
    pub fn resolve(&self, base: &Url, raw: &str) -> Option<String> {
        let (url, fragment) = mhtml_serve::locate::resolve_reference(base, raw)?;
        let target = self.map.get(&url)?;
        let rel = relative_path(self.from_dir, target);
        Some(format!("{rel}{fragment}"))
    }
}

/// The relative path from one output directory to a target output file, using
/// `/` separators and `../` to climb out of the referrer's directory.
///
/// Both inputs are output-root-relative paths built by the `naming` module, so
/// they only contain `Normal` components; anything else is ignored. This flattens
/// each `Path` to a `/`-joined key and delegates to `mhtml_serve::naming::relative_path`,
/// the single source of truth for the diff.
fn relative_path(from_dir: &Path, to: &Path) -> String {
    fn flatten(p: &Path) -> String {
        p.components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/")
    }
    mhtml_serve::naming::relative_path(&flatten(from_dir), &flatten(to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(from: &str, to: &str) -> String {
        relative_path(&PathBuf::from(from), &PathBuf::from(to))
    }

    /// Build a rewrite map with `url` crate normalized keys, mirroring how the
    /// extraction flow keys the map.
    fn map(entries: &[(&str, &str)]) -> HashMap<String, PathBuf> {
        entries
            .iter()
            .map(|(url, path)| {
                (
                    Url::parse(url).unwrap().as_str().to_string(),
                    PathBuf::from(path),
                )
            })
            .collect()
    }

    fn base(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    #[test]
    fn absolute_url_hit_yields_relative_path_with_parents() {
        let m = map(&[("http://h/img/logo.png", "h/img/logo.png")]);
        let r = Resolver::new(&m, Path::new("h/a/b"));
        assert_eq!(
            r.resolve(&base("http://h/a/b/page.html"), "http://h/img/logo.png"),
            Some("../../img/logo.png".to_string())
        );
    }

    #[test]
    fn relative_ref_resolved_via_base() {
        let m = map(&[("http://h/dir/style.css", "h/dir/style.css")]);
        let r = Resolver::new(&m, Path::new("h/dir"));
        assert_eq!(
            r.resolve(&base("http://h/dir/page.html"), "style.css"),
            Some("style.css".to_string())
        );
    }

    #[test]
    fn cid_reference_hit() {
        let m = map(&[("cid:image1@example.com", "_cid/image1@example.com.png")]);
        let r = Resolver::new(&m, Path::new("h"));
        assert_eq!(
            r.resolve(&base("http://h/page.html"), "cid:image1@example.com"),
            Some("../_cid/image1@example.com.png".to_string())
        );
    }

    #[test]
    fn fragment_is_preserved_on_hit() {
        let m = map(&[("http://h/style.css", "h/style.css")]);
        let r = Resolver::new(&m, Path::new("h"));
        assert_eq!(
            r.resolve(&base("http://h/page.html"), "style.css#icons"),
            Some("style.css#icons".to_string())
        );
    }

    #[test]
    fn unmapped_reference_is_untouched() {
        let m = map(&[("http://h/style.css", "h/style.css")]);
        let r = Resolver::new(&m, Path::new("h"));
        assert_eq!(r.resolve(&base("http://h/page.html"), "other.css"), None);
    }

    #[test]
    fn sibling_in_same_dir_is_bare_filename() {
        assert_eq!(rel("example.com", "example.com/style.css"), "style.css");
    }

    #[test]
    fn descends_into_subdirectory() {
        assert_eq!(
            rel("example.com", "example.com/img/logo.png"),
            "img/logo.png"
        );
    }

    #[test]
    fn climbs_out_of_one_directory() {
        assert_eq!(
            rel("example.com/a", "example.com/style.css"),
            "../style.css"
        );
    }

    #[test]
    fn climbs_and_descends_on_divergent_paths() {
        assert_eq!(rel("a/b", "c/d/x.png"), "../../c/d/x.png");
    }

    #[test]
    fn deep_referrer_to_shallow_target() {
        assert_eq!(rel("h/a/b", "h/img/logo.png"), "../../img/logo.png");
    }
}
