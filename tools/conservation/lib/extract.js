// Run OUR product — the `mhtml` CLI's extract command — over an archive and
// report where the offline-renderable entry landed. This is the code under
// test: everything downstream (render + compare) measures what this produced.

import { execFile } from "node:child_process";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileP = promisify(execFile);

// Prefer the release binary (fast, prebuilt); the harness builds it once up
// front via the Makefile / a preflight, so we don't shell out to `cargo run`
// per archive.
const REPO_ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const MHTML_BIN = join(REPO_ROOT, "target", "release", "mhtml");

/**
 * Extract `mhtmlPath` into a fresh temp dir. Returns
 * { ok, outDir, indexPath, stdout, cleanup } — indexPath is the offline entry
 * to render. Caller must await cleanup() when done with outDir.
 */
export async function extract(mhtmlPath) {
  const outDir = await mkdtemp(join(tmpdir(), "mhtml-conserve-"));
  const cleanup = () => rm(outDir, { recursive: true, force: true });
  try {
    const { stdout } = await execFileP(MHTML_BIN, ["extract", mhtmlPath, "-o", outDir]);
    // extract prints the absolute path of the root index.html on success.
    const indexPath = stdout.trim().split("\n").pop().trim();
    return { ok: true, outDir, indexPath, stdout, cleanup };
  } catch (e) {
    // Non-zero exit (truncated/corrupt archive) still leaves salvaged files;
    // report failure but keep outDir for inspection until cleanup.
    return {
      ok: false,
      outDir,
      indexPath: null,
      error: e.stderr ? e.stderr.trim() : e.message,
      cleanup,
    };
  }
}

// The root index.html extract writes is often a meta-refresh stub pointing at
// the real entry document (e.g. <out>/<host>/index.html). Rendering the stub
// and snapshotting before the refresh navigates (especially with JS disabled)
// measures a half-loaded page. Resolve the stub to the real entry file up
// front and render that directly, so `load` waits for its stylesheets/images.
export async function resolveEntry(indexPath) {
  let current = indexPath;
  for (let hop = 0; hop < 3; hop++) {
    let html;
    try {
      html = await readFile(current, "utf8");
    } catch {
      return current;
    }
    const m = html.match(/http-equiv=["']?refresh["']?[^>]*content=["'][^"']*url=([^"']+)["']/i);
    if (!m) return current;
    const target = m[1].replace(/&amp;/g, "&").replace(/&#39;/g, "'").replace(/&quot;/g, '"');
    current = resolve(dirname(current), target);
  }
  return current;
}

export { MHTML_BIN };
