// Verify conservation for CONTENT-HASH extraction — the flat, content-addressed
// naming mode (`mhtml extract --naming hash`). This is the hash-mode sibling of
// run/verify.js (which proves the MIRROR mode). It exists to prove that
// switching the resource-naming strategy to `<sha256>.<ext>` does NOT lose any
// rendered content: the same archive, extracted flat, must render the same text,
// images, computed styles, and shadow/template structure as Chromium's native
// render.
//
//   node run/verify-hash.js                     # a few representative archives
//   node run/verify-hash.js path/to/one.mhtml   # just that archive
//
// For each archive:
//   REF       = Chromium rendering the .mhtml natively (JS off, UNsealed —
//               MHTML is self-contained so it needs no network anyway).
//   OURS_HASH = `mhtml extract <archive> -o <tmp> --naming hash`, then Chromium
//               rendering <tmp>/index.html offline (JS off, network SEALED).
// Because hash mode is flat and index.html is co-located with the <hash>.<ext>
// files, the bare relative refs it emits resolve straight off file:// with no
// <base> and no directory hierarchy.
//
// Exit code is the number of failing archives (clamped to 1) so it can gate CI.

import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, basename, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

import { launchBrowser, render } from "../lib/render.js";
import { compare } from "../lib/compare.js";
import { wasmParity } from "../lib/wasmparity.js";

const execFileP = promisify(execFile);

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const CORPUS = join(ROOT, "corpus");
const REPO_ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const MHTML_BIN = join(REPO_ROOT, "target", "release", "mhtml");

// Representative defaults: a modern doc-heavy page, a non-Latin (Japanese) page
// that stresses charset handling, and the tiny hand-authored fixture. Enough to
// exercise text, images, CSS, and encodings without running the whole corpus.
const DEFAULT_ARCHIVES = [
  join(CORPUS, "modern", "mdn.mhtml"),
  join(CORPUS, "lang", "wiki-ja.mhtml"),
  join(REPO_ROOT, "crates", "cli", "tests", "fixtures", "simple.mhtml"),
];

/** Extract `mhtmlPath` into a fresh temp dir using CONTENT-HASH naming. */
async function extractHash(mhtmlPath) {
  const outDir = await mkdtemp(join(tmpdir(), "mhtml-hash-"));
  const cleanup = () => rm(outDir, { recursive: true, force: true });
  try {
    const { stdout } = await execFileP(MHTML_BIN, [
      "extract",
      mhtmlPath,
      "-o",
      outDir,
      "--naming",
      "hash",
    ]);
    // Hash mode prints the absolute path of the co-located index.html entry.
    const indexPath = stdout.trim().split("\n").pop().trim();
    return { ok: true, outDir, indexPath, cleanup };
  } catch (e) {
    return { ok: false, outDir, indexPath: null, error: e.stderr ? e.stderr.trim() : e.message, cleanup };
  }
}

async function verifyOne(browser, mhtmlPath) {
  const id = basename(mhtmlPath).replace(/\.(mhtml|mht)$/, "");

  const ref = await render(browser, pathToFileURL(mhtmlPath).href);
  if (!ref.ok) return { id, path: mhtmlPath, status: "render-error", error: `ref: ${ref.error}` };

  const ex = await extractHash(mhtmlPath);
  try {
    if (!ex.ok || !ex.indexPath) {
      return { id, path: mhtmlPath, status: "extract-error", error: ex.error || "no entry" };
    }
    // In hash mode index.html IS the entry (co-located with the <hash>.<ext>
    // files), so we render it directly — no meta-refresh stub to chase.
    const ours = await render(browser, pathToFileURL(ex.indexPath).href, { sealNetwork: true });
    if (!ours.ok) return { id, path: mhtmlPath, status: "render-error", error: `ours: ${ours.error}` };

    let parity;
    try {
      parity = await wasmParity(mhtmlPath, ex.outDir);
    } catch (e) {
      parity = { pass: false, checked: 0, mismatches: [{ url: "(wasm error)", mime: e.message }] };
    }

    const cmp = compare(ref.signals, ours.signals, parity);
    return { id, path: mhtmlPath, status: cmp.pass ? "pass" : "fail", compare: cmp };
  } finally {
    await ex.cleanup();
  }
}

async function main() {
  const arg = process.argv[2];
  const archives = arg ? [arg] : DEFAULT_ARCHIVES;

  const browser = await launchBrowser();
  const results = [];
  try {
    for (const a of archives) {
      const r = await verifyOne(browser, a);
      results.push(r);
      const tag = r.status === "pass" ? "PASS" : r.status === "fail" ? "FAIL" : "ERR ";
      const detail =
        r.status === "fail" ? "  " + r.compare.diffs.join("; ") : r.status.endsWith("error") ? "  " + r.error : "";
      process.stdout.write(`  ${tag}  ${id_pad(r.id)}${detail}\n`);
    }
  } finally {
    await browser.close();
  }

  const pass = results.filter((r) => r.status === "pass").length;
  const fail = results.length - pass;
  console.log(`\nHash-mode conservation: ${results.length} archives — ${pass} pass, ${results.length - pass} not-pass`);
  process.exit(fail > 0 ? 1 : 0);
}

function id_pad(s) {
  return s.length >= 24 ? s : s + " ".repeat(24 - s.length);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
