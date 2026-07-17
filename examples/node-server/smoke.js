// Server-free smoke test for the wasm parser: parse the checked-in fixture and
// assert the JS API returns what the fixture actually contains. Exits non-zero
// on any failure; prints "smoke: ok" on success.
//
//   node smoke.js

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { MhtmlArchive } from "../../crates/wasm/pkg-node/mhtml_wasm.js";

// Resolve the fixture relative to this file, not the process cwd.
const fixture = fileURLToPath(
  new URL("../../crates/cli/tests/fixtures/simple.mhtml", import.meta.url),
);

const archive = MhtmlArchive.parse(readFileSync(fixture));

// simple.mhtml has three parts: the root document plus two subresources.
assert.equal(archive.urls().length, 3, "expected 3 parts");

// The root document's Content-Location.
assert.equal(archive.entry_url(), "http://example.com/", "entry url");

// A known resource's declared Content-Type (the style.css part is `text/css`).
assert.equal(
  archive.content_type("http://example.com/style.css"),
  "text/css",
  "style.css content type",
);

// Raw (unrewritten) bodies: base64 "MTIzYWJj" decodes to "123abc", and the
// entry fetched by index still carries its original relative reference.
assert.equal(
  Buffer.from(archive.body("http://example.com/style.css")).toString("utf8"),
  "123abc",
  "style.css raw body",
);
assert.ok(
  Buffer.from(archive.body_at(archive.entry_index()))
    .toString("utf8")
    .includes('href="style.css"'),
  "raw entry body keeps its original reference",
);

// The content-hash strategy exposes every resource by index with a flat
// "<sha256>.<ext>" key. Locate the style.css resource and confirm its key is
// the SHA-256 of its raw body plus the ".css" extension.
import { createHash } from "node:crypto";

assert.equal(archive.resource_count(), 3, "expected 3 resources");

// Every content-hash key is a flat "<64 lowercase hex>.<ext>" name.
const HASH_KEY = /^[0-9a-f]{64}\.[a-z0-9]+$/;
for (let i = 0; i < archive.resource_count(); i++) {
  const key = archive.resource_key(i, "hash");
  assert.match(key, HASH_KEY, `resource ${i} key shape`);
}

let cssIndex;
for (let i = 0; i < archive.resource_count(); i++) {
  if (archive.resource_mime(i) === "text/css") cssIndex = i;
}
assert.notEqual(cssIndex, undefined, "style.css resource found");

const cssBody = Buffer.from(archive.resource_bytes(cssIndex));
const expectedCssKey = `${createHash("sha256").update(cssBody).digest("hex")}.css`;
assert.equal(
  archive.resource_key(cssIndex, "hash"),
  expectedCssKey,
  "content-hash key = sha256(body).css",
);

// The rewritten entry document must re-point its in-archive reference at the
// bare content-hash key and must not leak the raw absolute subresource URL.
const entry = Buffer.from(
  archive.entry_rewritten("hash", undefined),
).toString("utf8");
assert.ok(
  entry.includes(expectedCssKey),
  "rewritten entry should reference the css content-hash key",
);
assert.ok(
  !entry.includes("http://example.com/style.css"),
  "rewritten entry should not contain the raw subresource URL",
);

console.log("smoke: ok");
