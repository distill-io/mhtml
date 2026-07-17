// Capture a live page as MHTML using Chromium's DevTools protocol — the exact
// same Page.captureSnapshot that "Save as → Webpage, Single File" invokes, so
// the archives are representative of what our parser must handle in the wild.
//
// Capture runs with JavaScript ENABLED (we want the fully-rendered page baked
// into the archive). Verification later renders with JS disabled, comparing
// two static views of that same archive.

import { collectSignals } from "./signals.js";

// A normal desktop Chrome UA — the default headless UA advertises
// "HeadlessChrome" and trips more bot walls.
const UA =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) " +
  "Chrome/149.0.0.0 Safari/537.36";

/**
 * Capture one site. Never throws — returns { ok:true, mhtml, signals } or
 * { ok:false, reason } so a single bad site can't abort the batch.
 *
 * @param {import('playwright').Browser} browser
 * @param {{url:string, waitUntil?:string, settleMs?:number}} site
 */
export async function capture(browser, site, { timeoutMs = 30000 } = {}) {
  const ctx = await browser.newContext({
    userAgent: UA,
    viewport: { width: 1280, height: 900 },
    locale: "en-US",
    ignoreHTTPSErrors: true,
  });
  const page = await ctx.newPage();
  try {
    const waitUntil = site.waitUntil || "load";
    await page.goto(site.url, { waitUntil, timeout: timeoutMs });
    // Let late/lazy resources attach before snapshotting.
    await page.waitForTimeout(site.settleMs ?? 1500);

    const signals = await collectSignals(page);

    const client = await ctx.newCDPSession(page);
    const { data } = await client.send("Page.captureSnapshot", { format: "mhtml" });
    if (!data || data.length < 200) {
      return { ok: false, reason: "empty snapshot" };
    }
    return { ok: true, mhtml: data, signals, finalUrl: page.url() };
  } catch (e) {
    return { ok: false, reason: e.message.split("\n")[0] };
  } finally {
    await ctx.close();
  }
}
