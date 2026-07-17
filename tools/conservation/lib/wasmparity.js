// Byte-identity check via the wasm build: every image resource the parser
// decodes must appear, byte-for-byte, in the extracted output tree. This is
// independent of rendering — it proves the extractor wrote exactly what the
// parser produced, using the archive's own URLs (no cross-render matching).

import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { MhtmlArchive } from "../../../crates/wasm/pkg-node/mhtml_wasm.js";

function sha256(buf) {
  return createHash("sha256").update(buf).digest("hex");
}

async function hashTree(dir) {
  const hashes = new Set();
  async function walk(d) {
    for (const ent of await readdir(d, { withFileTypes: true })) {
      const p = join(d, ent.name);
      if (ent.isDirectory()) await walk(p);
      else if (ent.isFile()) hashes.add(sha256(await readFile(p)));
    }
  }
  await walk(dir);
  return hashes;
}

/**
 * @param {string} mhtmlPath  the source archive
 * @param {string} outDir     the extracted tree
 * @returns {{ pass:boolean, checked:number, mismatches:{url:string,mime:string}[] }}
 */
export async function wasmParity(mhtmlPath, outDir) {
  const archive = MhtmlArchive.parse(await readFile(mhtmlPath));
  const diskHashes = await hashTree(outDir);

  const seen = new Set();
  const mismatches = [];
  let checked = 0;

  for (const url of archive.urls()) {
    const ct = archive.content_type(url) || "";
    if (!ct.startsWith("image/")) continue;
    const body = archive.body(url);
    if (!body) continue;
    const h = sha256(Buffer.from(body));
    if (seen.has(h)) continue; // dedup identical images / alias urls
    seen.add(h);
    checked++;
    if (!diskHashes.has(h)) mismatches.push({ url, mime: ct });
  }

  archive.free();
  return { pass: mismatches.length === 0, checked, mismatches };
}
