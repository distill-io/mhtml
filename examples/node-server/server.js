// Minimal demo: serve an MHTML archive over HTTP using the wasm parser.
//
//   node server.js <file.mhtml> [port=8000]
//
// Resources are named with the content-hash strategy: each is served under a
// flat "/<sha256>.<ext>" key, which is exactly where the rewritten HTML/CSS
// points its (bare, relative) references. The entry document is served at `/`;
// because it sits at the root, its bare keys resolve to `/<key>` with no
// `<base>` needed (base_href is null). Zero npm dependencies: plain node:http
// and the wasm build (Node imports its named export straight from the .js).

import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { MhtmlArchive } from "../../crates/wasm/pkg-node/mhtml_wasm.js";

const STRATEGY = "hash";

const [, , file, portArg] = process.argv;
if (!file) {
  console.error("usage: node server.js <file.mhtml> [port=8000]");
  process.exit(1);
}
const port = Number(portArg ?? 8000);

let archive;
try {
  archive = MhtmlArchive.parse(readFileSync(file));
} catch (err) {
  console.error(`parse failed: ${err.message}`);
  process.exit(1);
}

// Map each content-hash key to its resource index, for path-based lookup.
const byKey = new Map();
for (let i = 0; i < archive.resource_count(); i++) {
  const key = archive.resource_key(i, STRATEGY);
  if (key !== undefined) byKey.set(key, i);
}

function notFound(res) {
  res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
  res.end("404 Not Found");
}

function sendResource(res, contentType, body) {
  if (contentType === undefined || body === undefined) {
    return notFound(res);
  }
  res.writeHead(200, { "Content-Type": contentType });
  res.end(Buffer.from(body));
}

const server = createServer((req, res) => {
  const path = req.url ?? "/";

  if (path === "/") {
    const index = archive.entry_index();
    if (index === undefined) return notFound(res);
    return sendResource(
      res,
      archive.content_type_at(index),
      archive.entry_rewritten(STRATEGY, undefined),
    );
  }

  const key = path.slice(1);
  const index = byKey.get(key);
  if (index === undefined) return notFound(res);
  return sendResource(
    res,
    archive.content_type_at(index),
    archive.rewritten_at(index, STRATEGY, undefined),
  );
});

server.listen(port, "127.0.0.1", () => {
  console.log(`serving ${file} at http://127.0.0.1:${port}/`);
  const index = archive.entry_index();
  console.log(
    index === undefined
      ? "entry: (none)"
      : `entry: ${archive.entry_url()} [${archive.content_type_at(index)}]`,
  );
});
