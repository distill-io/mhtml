// Capture sites from sites.json into corpus/<category>/<id>.mhtml.
//
//   node run/capture.js                 # all sites
//   node run/capture.js lang charset    # only these categories
//   node run/capture.js --only wiki-ja  # only this id
//
// Capture failures (timeouts, bot walls, empty snapshots) are non-fatal: they
// are recorded in results/capture-manifest.json and the run continues. The
// manifest also stores capture-time live signals, which later distinguishes a
// bad capture from a parser bug.

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { launchBrowser } from "../lib/render.js";
import { capture } from "../lib/capture.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const CORPUS = join(ROOT, "corpus");
const RESULTS = join(ROOT, "results");
const CONCURRENCY = 4;

function selectSites(all, args) {
  const only = [];
  const cats = [];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--only") only.push(args[++i]);
    else cats.push(args[i]);
  }
  let sites = all;
  if (cats.length) sites = sites.filter((s) => cats.includes(s.category));
  if (only.length) sites = sites.filter((s) => only.includes(s.id));
  return sites;
}

async function runPool(items, n, worker) {
  const results = [];
  let idx = 0;
  const workers = Array.from({ length: Math.min(n, items.length) }, async () => {
    while (idx < items.length) {
      const i = idx++;
      results[i] = await worker(items[i], i);
    }
  });
  await Promise.all(workers);
  return results;
}

async function main() {
  const cfg = JSON.parse(await readFile(join(ROOT, "sites.json"), "utf8"));
  const defaults = cfg.defaults || {};
  const sites = selectSites(cfg.sites, process.argv.slice(2));
  if (!sites.length) {
    console.error("No sites match selection.");
    process.exit(1);
  }

  await mkdir(RESULTS, { recursive: true });
  const browser = await launchBrowser();
  const manifest = [];
  try {
    await runPool(sites, CONCURRENCY, async (site) => {
      const opts = {
        waitUntil: site.waitUntil || defaults.waitUntil,
        settleMs: site.settleMs ?? defaults.settleMs,
      };
      const res = await capture(browser, { ...site, ...opts });
      if (res.ok) {
        const dir = join(CORPUS, site.category);
        await mkdir(dir, { recursive: true });
        const path = join(dir, `${site.id}.mhtml`);
        await writeFile(path, res.mhtml);
        const kb = Math.round(res.mhtml.length / 1024);
        console.log(`  OK   ${site.id.padEnd(22)} ${String(kb).padStart(6)} KB  ${site.category}`);
        manifest.push({
          id: site.id,
          category: site.category,
          url: site.url,
          finalUrl: res.finalUrl,
          bytes: res.mhtml.length,
          liveSignals: {
            textLen: res.signals.text.length,
            images: res.signals.images.length,
            stylesheets: res.signals.stylesheetCount,
            shadowHosts: res.signals.shadowHosts,
            templates: res.signals.templates,
            charset: res.signals.charset,
            lang: res.signals.lang,
          },
        });
      } else {
        console.log(`  SKIP ${site.id.padEnd(22)} ${res.reason}`);
        manifest.push({ id: site.id, category: site.category, url: site.url, skipped: res.reason });
      }
    });
  } finally {
    await browser.close();
  }

  await writeFile(join(RESULTS, "capture-manifest.json"), JSON.stringify(manifest, null, 2));
  const ok = manifest.filter((m) => !m.skipped).length;
  console.log(`\nCaptured ${ok}/${sites.length} → ${join("results", "capture-manifest.json")}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
