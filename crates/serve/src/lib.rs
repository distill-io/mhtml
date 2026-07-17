//! Consumption layer over `parser`: entry-point selection, reference
//! resolution, and HTML/CSS document rewriting needed to turn a parsed archive
//! into something that renders offline. No MIME parsing here (that is
//! `parser`), no disk I/O here (that is the CLI's `naming`/`extract`).

pub mod bundle;
pub mod ctype;
pub mod entry;
pub mod locate;
pub mod mime;
pub mod naming;
pub mod rewrite_css;
pub mod rewrite_html;
