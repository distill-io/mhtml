//! Entry-point (main resource) selection: the first document part wins, a lone
//! image-only archive counts, and script/style parts never do.

use crate::ctype::content_type_essence;

fn is_document(content_type: &str) -> bool {
    matches!(
        content_type_essence(content_type).as_str(),
        "text/html" | "application/xhtml+xml"
    )
}

fn is_image(content_type: &str) -> bool {
    content_type_essence(content_type).starts_with("image/")
}

/// Choose the archive's main resource from its parts' content types.
///
/// The first document part (`text/html` / `application/xhtml+xml`) wins; a
/// lone image-only part also qualifies. JavaScript and CSS parts never do.
/// Returns the index of the entry part, or `None` if there is no suitable one.
pub fn select_entry(content_types: &[&str]) -> Option<usize> {
    if content_types.len() == 1 && is_image(content_types[0]) {
        return Some(0);
    }
    content_types.iter().position(|ct| is_document(ct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_html_part_is_entry() {
        assert_eq!(
            select_entry(&["image/png", "text/html", "text/html"]),
            Some(1)
        );
    }

    #[test]
    fn xhtml_qualifies() {
        assert_eq!(select_entry(&["application/xhtml+xml"]), Some(0));
    }

    #[test]
    fn content_type_parameters_ignored() {
        assert_eq!(select_entry(&["text/html; charset=utf-8"]), Some(0));
    }

    #[test]
    fn lone_image_qualifies() {
        assert_eq!(select_entry(&["image/jpeg"]), Some(0));
    }

    #[test]
    fn image_among_many_does_not_qualify() {
        assert_eq!(select_entry(&["image/png", "text/css"]), None);
    }

    #[test]
    fn css_and_js_never_qualify() {
        assert_eq!(select_entry(&["text/css", "application/javascript"]), None);
        assert_eq!(select_entry(&["text/javascript"]), None);
    }

    #[test]
    fn empty_archive_has_no_entry() {
        assert_eq!(select_entry(&[]), None);
    }
}
