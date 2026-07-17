// S3-style static server for a content-hash-extracted archive.
//
//   node server.js <dir> [port=8000]
//
// <dir> is the output of `mhtml extract <archive> -o <dir> --naming hash`
// (or of ./hash-extract.js, which produces the identical layout):
//
//   <dir>/index.html                 the entry document (served at "/")
//   <dir>/<sha256>.<ext>             every resource, named by content hash
//   <dir>/manifest.json              [{ key, mime, url|null }, ...]
//
// This is the shape you would upload verbatim to an S3 bucket: each file's
// name IS its object key. Because the entry HTML is served at "/", the bare,
// relative "<hash>.<ext>" references its rewritten markup contains resolve to
// "/<hash>.<ext>" with no <base> tag needed. Content-Type is taken from
// manifest.json. Zero npm dependencies: plain node:http + node:fs.

import { createServer } from "node:http";
import { readFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

const [, , dir, portArg] = process.argv;
if (!dir) {
  console.error("usage: node server.js <dir> [port=8000]");
  process.exit(1);
}
const root = resolve(dir);
const port = Number(portArg ?? 8000);

const manifestPath = join(root, "manifest.json");
if (!existsSync(manifestPath)) {
  console.error(`no manifest.json in ${root}; is this a --naming hash output?`);
  process.exit(1);
}

// key -> mime, straight from the manifest the extractor emitted. This is the
// only allow-list of servable object keys, which also blocks path traversal.
const mimeByKey = new Map();
for (const { key, mime } of JSON.parse(readFileSync(manifestPath, "utf-8"))) {
  mimeByKey.set(key, mime);
}

// Text payloads carry a charset; binaries do not.
function contentType(mime) {
  return /^text\//.test(mime) || mime === "application/javascript"
    ? `${mime}; charset=utf-8`
    : mime;
}

function notFound(res) {
  res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
  res.end("404 Not Found");
}

function send(res, key, mime) {
  const body = readFileSync(join(root, key));
  res.writeHead(200, { "Content-Type": contentType(mime) });
  res.end(body);
}

const server = createServer((req, res) => {
  let path;
  try {
    path = decodeURIComponent((req.url ?? "/").split("?")[0]);
  } catch {
    return notFound(res);
  }

  if (path === "/") {
    // The entry document is always the HTML root, served from index.html.
    return send(res, "index.html", "text/html");
  }

  const key = path.replace(/^\/+/, "");
  const mime = mimeByKey.get(key);
  if (mime === undefined) return notFound(res);
  return send(res, key, mime);
});

server.listen(port, "127.0.0.1", () => {
  console.log(`serving ${root} at http://127.0.0.1:${port}/`);
});
