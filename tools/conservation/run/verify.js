// Verify conservation for every archive in the corpus (or a single archive
// passed as an argument).
//
//   node run/verify.js                     # all of corpus/**/*.mhtml
//   node run/verify.js path/to/one.mhtml   # just that archive
//
// For each archive: render it natively in Chromium (REF), extract it with our
// CLI and render the reconstruction (OURS), check image byte-parity via wasm,
// then diff. Writes results/results.json and per-failure signal dumps, and
// prints a ranked summary. Exit code is the number of failing archives
// (clamped to 1) so it can gate CI.

import { readdir, mkdir, writeFile, stat } from "node:fs/promises";
import { join, basename, dirname, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { launchBrowser, render } from "../lib/render.js";
import { extract, resolveEntry } from "../lib/extract.js";
import { wasmParity } from "../lib/wasmparity.js";
import { compare } from "../lib/compare.js";
import { formatSummary, rankResults } from "../lib/report.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const CORPUS = join(ROOT, "corpus");
const RESULTS = join(ROOT, "results");

async function listArchives(arg) {
  if (arg) return [arg];
  const out = [];
  async function walk(d) {
    let ents;
    try {
      ents = await readdir(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of ents) {
      const p = join(d, e.name);
      if (e.isDirectory()) await walk(p);
      else if (e.name.endsWith(".mhtml") || e.name.endsWith(".mht")) out.push(p);
    }
  }
  await walk(CORPUS);
  return out.sort();
}

async function verifyOne(browser, mhtmlPath) {
  const id = basename(mhtmlPath).replace(/\.(mhtml|mht)$/, "");

  const ref = await render(browser, pathToFileURL(mhtmlPath).href);
  if (!ref.ok) return { id, path: mhtmlPath, status: "render-error", error: `ref: ${ref.error}` };

  const ex = await extract(mhtmlPath);
  try {
    if (!ex.ok || !ex.indexPath) {
      return { id, path: mhtmlPath, status: "extract-error", error: ex.error || "no entry" };
    }
    const entryPath = await resolveEntry(ex.indexPath);
    const ours = await render(browser, pathToFileURL(entryPath).href, { sealNetwork: true });
    if (!ours.ok) return { id, path: mhtmlPath, status: "render-error", error: `ours: ${ours.error}` };

    let parity;
    try {
      parity = await wasmParity(mhtmlPath, ex.outDir);
    } catch (e) {
      parity = { pass: false, checked: 0, mismatches: [{ url: "(wasm error)", mime: e.message }] };
    }

    const cmp = compare(ref.signals, ours.signals, parity);
    return {
      id,
      path: mhtmlPath,
      status: cmp.pass ? "pass" : "fail",
      compare: cmp,
      refSignals: ref.signals,
      oursSignals: ours.signals,
    };
  } finally {
    await ex.cleanup();
  }
}

async function main() {
  const arg = process.argv[2];
  const archives = await listArchives(arg);
  if (archives.length === 0) {
    console.error(`No archives found${arg ? `: ${arg}` : ` under ${relative(process.cwd(), CORPUS)}`}`);
    process.exit(1);
  }

  await mkdir(RESULTS, { recursive: true });
  const browser = await launchBrowser();
  const results = [];
  try {
    for (const a of archives) {
      const r = await verifyOne(browser, a);
      results.push(r);
      const tag =
        r.status === "pass" ? "PASS" : r.status === "fail" ? "FAIL" : "ERR ";
      process.stdout.write(
        `  ${tag}  ${r.id}${r.status === "fail" ? "  " + r.compare.diffs.join("; ") : ""}\n`,
      );
      // Dump full signals for anything that didn't cleanly pass.
      if (r.status !== "pass") {
        const dir = join(RESULTS, r.id);
        await mkdir(dir, { recursive: true });
        await writeFile(join(dir, "result.json"), JSON.stringify(r, null, 2));
      }
    }
  } finally {
    await browser.close();
  }

  // Trim heavy signal blobs from the aggregate file; keep them in per-id dumps.
  const slim = results.map(({ refSignals, oursSignals, ...rest }) => rest);
  await writeFile(join(RESULTS, "results.json"), JSON.stringify(rankResults(slim), null, 2));

  console.log("\n" + formatSummary(results));
  const fails = results.filter((r) => r.status !== "pass").length;
  process.exit(fails > 0 ? 1 : 0);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
