// Render a URL in headless Chromium with JavaScript disabled and return its
// signal set. Both the reference (native .mhtml) and OURS (extracted
// index.html) go through here, so the two renders are configured identically —
// same engine, same JS-off policy — and only the input archive differs.

import { chromium } from "playwright";
import { collectSignals } from "./signals.js";

/** Launch one shared browser for a batch of renders. */
export function launchBrowser() {
  return chromium.launch({ headless: true });
}

/**
 * Load `url` (a file:// URL) with JS disabled and collect its signals.
 * Returns { ok:true, signals } or { ok:false, error } — never throws, so one
 * bad archive can't abort a batch.
 */
export async function render(browser, url, { timeoutMs = 20000, sealNetwork = false } = {}) {
  const ctx = await browser.newContext({ javaScriptEnabled: false });
  // Seal ONLY our extracted render from the live web. Our extracted page is a
  // file:// document whose in-archive references now point at local files
  // (allowed), but any *unmapped* reference is still a live http(s) URL that a
  // file:// page would fetch for real — inflating it with resources the
  // archive never contained. A native .mhtml render can't reach those (MHTML
  // is sealed), so blocking them here makes the two renders comparable.
  // The reference render must NOT be intercepted: Chromium serves the
  // archive's own subresources via their original http(s) URLs through the
  // network stack, so aborting those would leave the reference unstyled.
  if (sealNetwork) {
    await ctx.route("**/*", (route) => {
      const u = route.request().url();
      if (u.startsWith("file:") || u.startsWith("data:")) route.continue();
      else route.abort();
    });
  }
  const page = await ctx.newPage();
  try {
    await page.goto(url, { waitUntil: "load", timeout: timeoutMs });
    // Give sub-resources (stylesheets/images inside the archive) a moment to
    // attach; they're all local so this settles fast.
    await page.waitForTimeout(250);
    const signals = await collectSignals(page);
    return { ok: true, signals };
  } catch (e) {
    return { ok: false, error: e.message };
  } finally {
    await ctx.close();
  }
}
