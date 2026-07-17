# Conservation harness

Captures real websites as MHTML with Playwright, then checks that our
parser/extractor **conserves content** — by rendering our extracted
reconstruction and diffing it against the browser's own native render of the *same*
archive. Any difference is our bug, not live-web noise.

This is a test/analysis tool, not part of the shipped crates. The corpus and
results are disposable (gitignored); the durable output is the regression tests
that real findings get distilled into (see "Findings" below).

## Layout

| File | Role |
|------|------|
| `sites.json` | Curated site list: `modern` (shadow DOM / `<template>` / lazy images), `lang` (20+ scripts & directions via Wikipedia), `charset` (legacy non-UTF-8 via Wayback `id_`), `archive` (archive.org + defunct snapshots). |
| `lib/signals.js` | In-page signal extractor (text, images, computed styles, shadow/template counts) + text similarity. |
| `lib/capture.js` | Live page → MHTML via CDP `Page.captureSnapshot`. |
| `lib/render.js` | Load a URL (JS off) and collect signals. OURS is network-sealed. |
| `lib/extract.js` | Run `mhtml extract`; resolve the entry document. |
| `lib/compare.js` | REF-vs-OURS pass criteria. |
| `lib/wasmparity.js` | Byte-identity of image resources via the wasm build. |
| `run/capture.js`, `run/verify.js` | CLI entry points. |

## Usage

```bash
make conservation-setup                 # npm install + playwright chromium
make capture ARGS="lang charset"        # capture selected categories → corpus/
make verify                             # REF vs OURS over corpus/ → results/
make conservation                       # build cli + capture all + verify
# or directly:
node run/capture.js --only wiki-ja
node run/verify.js ../../crates/cli/tests/fixtures/simple.mhtml
```

## Verification model

For each archive: **REF** = the browser rendering the `.mhtml` natively (`file://`,
JS disabled) vs **OURS** = `mhtml extract` → offline render of the entry (JS
disabled, external network sealed). Pass criteria:

- **images** — same width×height multiset, plus wasm byte-parity (every decoded
  image part is byte-identical on disk).
- **text** — Sørensen–Dice over word bigrams ≥ 0.98.
- **css** — computed-style agreement over probe selectors ≥ 0.95. (Not
  `cssRules.length`: every external `file://` stylesheet is an opaque origin, so
  reading its rules throws `SecurityError` even when fully applied.)
- **shadow / templates** — reconstructed shadow-host and `<template>` counts
  match exactly.

## Extractor rewrite/neutralize coverage

What `crates/serve/src/rewrite_html.rs` (+ `rewrite_css.rs`) repoints or defuses
so the reconstruction renders offline, network-sealed:

- **URL attributes** repointed to the extracted local file (`URL_ATTRS`):
  `a[href]`, `link[href]`, `area[href]`, `img[src]`, `script[src]`,
  `iframe[src]`, `frame[src]`, `embed[src]`, `audio[src]`, `video[src]`,
  `source[src]`, `input[src]`, `track[src]`, `video[poster]`, `object[data]`,
  `body[background]`, `button[formaction]`, `input[formaction]`.
- **`srcset`-format lists** — `img[srcset]`, `source[srcset]`, and
  `link[imagesrcset]` — rewritten candidate-by-candidate, preserving descriptors
  and whitespace (a comma inside a URL is not a candidate boundary).
- **SVG references** — `<image>` and `<use>`, via both `href` and legacy
  `xlink:href`.
- **CSS references** — `url(...)` tokens and `@import` in `<style>` elements,
  `style="…"` attributes, and standalone `.css` parts.
- **Entity decoding before resolution** — HTML character references in URL
  values (e.g. `?a=1&amp;b=2`) are decoded so they match the real URL key.
- **`<base href>` neutralized** — otherwise it re-anchors every relative
  reference back to the live web.
- **Declarative shadow DOM restored** — `template[shadowmode]` /
  `shadowdelegatesfocus` renamed to the standard `shadowrootmode` /
  `shadowrootdelegatesfocus`.
- **CORS/SRI/CSP metadata stripped** — `integrity`/`crossorigin`/`nonce` removed
  from `<link>`/`<script>`, and any `<meta http-equiv="Content-Security-Policy">`
  (or `-Report-Only`) removed, so the browser applies the local copies.

Naming (`crates/cli/src/naming.rs`) forces the canonical `.css`/`.js` extension
for `text/css` / `text/javascript` parts, since the browser sniffs those by
extension (not content) from `file://`.

Inherent limits it can *not* fix: adopted/constructable stylesheets (not
serialized in MHTML) and browser user-agent-default rendering of native form
controls — see "Known residuals" below.

## Findings distilled into regression tests

Four real extractor bugs this harness surfaced, each fixed with a committed,
network-free regression:

1. **CSS/JS served from non-`.css`/`.js` URLs weren't applied offline.**
   Wikipedia's `…/load.php?…` stylesheets extracted as `load.q<hash>.php`;
   the browser refuses to apply a `.php` file as CSS from `file://`. Fixed by
   forcing the canonical extension for `text/css` / `text/javascript`
   (`crates/cli/src/naming.rs`, `forces_css_js_extension_over_misleading_url_extension`).

2. **Multi-parameter query URLs never rewrote.** lol_html returns raw
   attribute values, so `href="…?a=1&amp;b=2"` reached the resolver with the
   `&amp;` still encoded and missed the map key — leaving the reference live.
   Fixed by decoding HTML character references before resolution
   (`crates/serve/src/rewrite_html.rs`, `amp_entity_in_query_string_resolves`).

3. **Shadow DOM was lost.** the browser serializes shadow roots in MHTML as the
   non-standard `<template shadowmode="open">`; served as plain HTML no browser
   reconstructs them. Fixed by rewriting `shadowmode`→`shadowrootmode` (and the
   `delegatesfocus` companion) (`crates/serve/src/rewrite_html.rs`,
   `rewrites_declarative_shadow_dom_attributes`).

4. **CORS/SRI/CSP metadata blocked local stylesheets and scripts.** GitHub's
   archive links its CSS as
   `<link crossorigin="anonymous" integrity="sha512-…" rel="stylesheet" href="…">`,
   and many sites ship a `<meta http-equiv="Content-Security-Policy">` that
   whitelists the original https hosts. Once extracted to `file://`, none of
   these can be satisfied — the CSS was `url()`-rewritten so its Subresource
   Integrity hash no longer matches, a CORS fetch from an opaque `file:` origin
   cannot succeed, and the CSP names hosts the extract never loads from — so the
   browser loads but refuses to *apply* the local stylesheet/script (GitHub:
   0 of 35 sheets applied). Fixed by stripping `integrity`/`crossorigin`/`nonce`
   from `<link>`/`<script>` and removing any CSP `<meta http-equiv>`
   (`crates/serve/src/rewrite_html.rs`,
   `strips_integrity_crossorigin_nonce_from_link_and_script`,
   `neutralizes_content_security_policy_meta`). GitHub's 35 stylesheets now
   apply.

### Known residuals (inherent, not bugs)

- **Adopted (constructable) stylesheets** attached to a shadow root are not part
  of declarative-shadow-DOM serialization, so an element styled only by one can
  differ after extraction — an inherent limit of serving browser-generated MHTML as
  standalone HTML.
- **UA-default form-control rendering.** The css check's remaining misses are a
  single probe selector — `button` — whose computed style differs by a browser
  user-agent default (a native control's color), not by any style the archive
  carried. It reads as a css failure in several sites (e.g. `github-repo`,
  `mdn`, `npm`, `stripe`, `go-dev`, `spectrum-wc-button`) even though every
  stylesheet is present and applied (sheet counts match, agreement ≈ 0.92–0.94).
- **Legacy-charset `a` selector.** `cs-cts-big5` retains a single-sheet `a`
  computed-style residual (agreement ≈ 0.80).
- **MediaWiki download navigation.** One archive (`wiki-nqo`) extracts an
  `index.php` entry that the browser treats as a download rather than a page when
  opened from `file://`, so REF/OURS cannot be rendered for comparison.
