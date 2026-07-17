// Compare a reference signal set (Chromium's native render of the .mhtml)
// against ours (our extracted reconstruction). Both were produced by the same
// engine with JS disabled from the same archive, so a divergence here is a
// conservation defect in our parser/extractor.
//
// Cross-render URL matching is deliberately avoided: the reference keeps each
// resource's ORIGINAL url (http://…) while ours points at LOCAL files
// (file://…/host/path). Images are therefore matched on their decoded raster
// fingerprint (a multiset of width×height), and exact byte-identity of image
// resources is proven separately by wasmparity.js against the archive's own
// URLs. That keeps this comparator robust to the URL rewriting we intend.

import { dice } from "./signals.js";

const TEXT_THRESHOLD = 0.98;

function dimsMultiset(images) {
  const m = new Map();
  for (const im of images) {
    const k = `${im.w}x${im.h}`;
    m.set(k, (m.get(k) || 0) + 1);
  }
  return m;
}

function multisetDiff(a, b) {
  const keys = new Set([...a.keys(), ...b.keys()]);
  const diff = [];
  for (const k of keys) {
    const na = a.get(k) || 0;
    const nb = b.get(k) || 0;
    if (na !== nb) diff.push({ dim: k, ref: na, ours: nb });
  }
  return diff;
}

// Fraction of shared probe selectors whose computed-style fingerprint is
// identical between the two renders. Same engine + same viewport means an
// applied stylesheet yields byte-identical computed values; a stylesheet that
// failed to apply falls back to UA defaults and diverges.
const CSS_THRESHOLD = 0.95;

function styleAgreement(refStyles, oursStyles) {
  const sels = Object.keys(refStyles).filter((s) => s in oursStyles);
  if (sels.length === 0) return { ratio: 1, shared: 0, mismatched: [] };
  const mismatched = sels.filter((s) => refStyles[s] !== oursStyles[s]);
  return { ratio: (sels.length - mismatched.length) / sels.length, shared: sels.length, mismatched };
}

/**
 * @param {object} ref   signals from the native .mhtml render
 * @param {object} ours  signals from our extracted index.html render
 * @param {object} [parity] optional wasmparity result { pass, mismatches }
 * @returns {{ pass:boolean, checks:object, diffs:string[] }}
 */
export function compare(ref, ours, parity) {
  const checks = {};
  const diffs = [];

  // Images: same count and same width×height multiset.
  {
    const ra = dimsMultiset(ref.images);
    const oa = dimsMultiset(ours.images);
    const d = multisetDiff(ra, oa);
    const pass = ref.images.length === ours.images.length && d.length === 0;
    checks.images = { pass, ref: ref.images.length, ours: ours.images.length, diff: d };
    if (!pass) {
      diffs.push(
        `images: ref=${ref.images.length} ours=${ours.images.length}` +
          (d.length ? ` dims ${JSON.stringify(d)}` : ""),
      );
    }
  }

  // Text: rendered innerText similarity.
  {
    const score = dice(ref.text, ours.text);
    const pass = score >= TEXT_THRESHOLD;
    checks.text = { pass, score: Number(score.toFixed(4)), threshold: TEXT_THRESHOLD };
    if (!pass) diffs.push(`text: dice=${score.toFixed(4)} < ${TEXT_THRESHOLD}`);
  }

  // Stylesheets: computed-style agreement across probe elements.
  {
    const a = styleAgreement(ref.computedStyles, ours.computedStyles);
    const pass = a.ratio >= CSS_THRESHOLD;
    checks.css = {
      pass,
      agreement: Number(a.ratio.toFixed(3)),
      shared: a.shared,
      mismatched: a.mismatched,
      refSheets: ref.stylesheetCount,
      oursSheets: ours.stylesheetCount,
    };
    if (!pass) {
      diffs.push(`css: style-agreement ${a.ratio.toFixed(3)} < ${CSS_THRESHOLD} (differ: ${a.mismatched.join(",")})`);
    }
  }

  // Shadow hosts and templates: exact structural equality.
  {
    const pass = ref.shadowHosts === ours.shadowHosts;
    checks.shadow = { pass, ref: ref.shadowHosts, ours: ours.shadowHosts };
    if (!pass) diffs.push(`shadowHosts: ${ref.shadowHosts} -> ${ours.shadowHosts}`);
  }
  {
    const pass = ref.templates === ours.templates;
    checks.templates = { pass, ref: ref.templates, ours: ours.templates };
    if (!pass) diffs.push(`templates: ${ref.templates} -> ${ours.templates}`);
  }

  // Byte-identity of image resources (from wasmparity), when supplied.
  if (parity) {
    checks.parity = { pass: parity.pass, mismatches: parity.mismatches };
    if (!parity.pass) {
      diffs.push(`byte-parity: ${parity.mismatches.length} image(s) differ`);
    }
  }

  const pass = Object.values(checks).every((c) => c.pass);
  return { pass, checks, diffs };
}
