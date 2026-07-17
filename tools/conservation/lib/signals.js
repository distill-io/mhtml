// Shared DOM "signals" extractor + text-similarity metric.
//
// A signal set is the small, comparable summary of what a rendered page
// contains: its visible text, the images that actually decoded, its
// stylesheet rule counts, and the shadow-DOM / <template> structure. The
// same extractor runs against BOTH the reference render (Chromium opening the
// .mhtml natively) and OURS (our extracted index.html), so any difference is
// attributable to our parser/extractor rather than to the live web.
//
// The collector below is injected into the page via page.evaluate and runs in
// the browser context — keep it self-contained (no Node imports, no closures
// over module scope).

/** Collect the signal set from an already-loaded page. */
export async function collectSignals(page) {
  return page.evaluate(() => {
    // Walk the light DOM and every open shadow root, invoking `visit` on each
    // element exactly once.
    function walk(root, visit) {
      const els = root.querySelectorAll("*");
      for (const el of els) {
        visit(el);
        if (el.shadowRoot) walk(el.shadowRoot, visit);
      }
    }

    const images = [];
    let shadowHosts = 0;
    let templates = 0;

    walk(document, (el) => {
      if (el.shadowRoot) shadowHosts++;
      const tag = el.tagName;
      if (tag === "TEMPLATE") templates++;
      if (tag === "IMG") {
        // naturalWidth > 0 means the bytes actually decoded to a raster.
        if (el.naturalWidth > 0) {
          images.push({
            url: el.currentSrc || el.src || "",
            w: el.naturalWidth,
            h: el.naturalHeight,
          });
        }
      }
    });

    // NOTE: sheet.cssRules is NOT a usable conservation signal — every
    // external stylesheet loaded from a file:// URL is an opaque origin, so
    // reading cssRules throws SecurityError even when the sheet is fully
    // applied. We instead fingerprint *computed styles* of representative
    // elements: if a stylesheet failed to apply (e.g. wrong extension so the
    // browser rejects it), those computed values fall back to UA defaults and
    // diverge from the reference. Keep only a stylesheet count for context.
    const stylesheetCount = document.styleSheets.length;

    // Document-level selectors only. Native form controls (button/input/…) are
    // deliberately excluded: their computed style is dominated by browser
    // user-agent defaults (e.g. a :disabled control's translucent color), which
    // the archive does not carry — so a difference there is not a conservation
    // failure and would only add false negatives.
    const STYLE_SELECTORS = [
      "body", "h1", "h2", "h3", "p", "a", "div", "header", "footer", "nav",
      "ul", "li", "table", "main", "article", "section", "span",
    ];
    const STYLE_PROPS = [
      "fontFamily", "fontSize", "fontWeight", "fontStyle", "color",
      "backgroundColor", "display", "marginTop", "marginLeft", "paddingTop",
      "maxWidth", "textAlign", "direction", "lineHeight", "borderTopWidth",
    ];
    const computedStyles = {};
    for (const sel of STYLE_SELECTORS) {
      const el = document.querySelector(sel);
      if (!el) continue;
      const cs = getComputedStyle(el);
      computedStyles[sel] = STYLE_PROPS.map((p) => cs[p]).join("|");
    }

    // innerText reflects rendered text and includes shadow-DOM content.
    const rawText = document.body ? document.body.innerText : "";

    return {
      text: rawText.replace(/\s+/g, " ").trim(),
      images,
      stylesheetCount,
      computedStyles,
      shadowHosts,
      templates,
      charset: document.characterSet || null,
      lang: document.documentElement.getAttribute("lang") || null,
      title: document.title || null,
    };
  });
}

/** Lowercased word bigrams of a string, as a multiset map bigram -> count. */
function wordBigrams(s) {
  const words = s.toLowerCase().split(/\s+/).filter(Boolean);
  const grams = new Map();
  if (words.length === 1) {
    grams.set(words[0], 1);
    return grams;
  }
  for (let i = 0; i < words.length - 1; i++) {
    const g = words[i] + " " + words[i + 1];
    grams.set(g, (grams.get(g) || 0) + 1);
  }
  return grams;
}

/**
 * Sørensen–Dice similarity over word bigrams, in [0, 1]. Two empty strings are
 * defined as identical (1); one empty and one not is 0.
 */
export function dice(a, b) {
  if (a === b) return 1;
  if (!a.length || !b.length) return 0;
  const ga = wordBigrams(a);
  const gb = wordBigrams(b);
  let inter = 0;
  let sizeA = 0;
  let sizeB = 0;
  for (const n of ga.values()) sizeA += n;
  for (const n of gb.values()) sizeB += n;
  for (const [g, na] of ga) {
    const nb = gb.get(g);
    if (nb) inter += Math.min(na, nb);
  }
  return (2 * inter) / (sizeA + sizeB);
}
