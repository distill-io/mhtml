// Produce a content-hash-extracted directory from an MHTML archive.
//
//   node hash-extract.js <archive.mhtml> <outdir> [--base-href URL]
//
// This emits the SAME layout the CLI's `mhtml extract <archive> -o <outdir>
// --naming hash` produces, but drives the shared serve layer through the
// in-repo wasm build. Use the CLI once its --naming flag is available; this
// helper keeps the demo runnable (and CI-verifiable) meanwhile.
//
//   <outdir>/index.html          entry document, refs rewritten to bare keys
//   <outdir>/<sha256>.<ext>      every resource under its content-hash key
//   <outdir>/manifest.json       [{ key, mime, url|null }, ...]
//
// Each file's name is exactly its S3 object key: upload <outdir>/* to a bucket
// and the page loads with no rewriting at serve time.

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { MhtmlArchive } from "../../crates/wasm/pkg-node/mhtml_wasm.js";

const args = process.argv.slice(2);
let baseHref;
const positional = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--base-href") baseHref = args[++i];
  else positional.push(args[i]);
}
const [archivePath, outDir] = positional;
if (!archivePath || !outDir) {
  console.error("usage: node hash-extract.js <archive.mhtml> <outdir> [--base-href URL]");
  process.exit(1);
}

let archive;
try {
  archive = MhtmlArchive.parse(readFileSync(archivePath));
} catch (err) {
  console.error(`parse failed: ${err.message}`);
  process.exit(1);
}

mkdirSync(outDir, { recursive: true });

const STRATEGY = "hash";
const entryIndex = archive.entry_index();
const manifest = [];

for (let i = 0; i < archive.resource_count(); i++) {
  const key = archive.resource_key(i, STRATEGY);
  if (key === undefined) continue; // hash strategy always yields a key
  const isEntry = i === entryIndex;
  const bytes = isEntry
    ? archive.entry_rewritten(STRATEGY, baseHref)
    : archive.rewritten_at(i, STRATEGY, undefined);
  writeFileSync(join(outDir, key), Buffer.from(bytes));
  // The wasm surface exposes a per-index url only for the entry; the CLI's own
  // manifest populates url for every resource. The server does not read url.
  manifest.push({
    key,
    mime: archive.resource_mime(i),
    url: isEntry ? (archive.entry_url() ?? null) : null,
  });
}

if (entryIndex !== undefined) {
  writeFileSync(
    join(outDir, "index.html"),
    Buffer.from(archive.entry_rewritten(STRATEGY, baseHref)),
  );
}

writeFileSync(join(outDir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");

console.log(`wrote ${manifest.length} objects + index.html + manifest.json to ${outDir}`);
