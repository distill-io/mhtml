# S3-style content-hash demo

This demo proves the end goal of the content-hash naming strategy: an MHTML
archive is extracted into a **flat directory whose file names are ready-made S3
object keys**, and the page renders offline with no server-side rewriting.

## The layout

`mhtml extract <archive> -o <dir> --naming hash` writes:

```
<dir>/index.html            the entry document; its refs are bare "<hash>.<ext>"
<dir>/<sha256>.<ext>        every resource, named by SHA-256 of its bytes
<dir>/manifest.json         [{ "key": "<hash>.<ext>", "mime": "...", "url": ... }]
```

Under `--naming hash` every reference the extractor writes into the HTML and CSS
is a **bare, relative** key like `4c4b6a...dab6.png` — never absolute, never
starting with `/`. That single property is what makes the output portable:

- **Co-located (this demo).** Serve `index.html` at `/`, and its bare
  `<hash>.<ext>` refs resolve to `/<hash>.<ext>`. A CSS file's `url(...)` refs
  resolve against the CSS file's own URL — and since everything is flat in one
  directory, they land next to it. No `<base>` tag is needed.
- **CDN / bucket prefix.** Upload `<dir>/*` to a bucket and serve the entry with
  `<base href="https://bucket.example/prefix/">`; the same bare keys now resolve
  under that prefix. Pass `--base-href <URL>` at extract time to bake that
  `<base>` into `index.html`.

## The S3 mapping

Each file's **name is its object key**. To publish:

```
aws s3 cp <dir>/ s3://my-bucket/ --recursive
```

Then either co-locate the entry in the same bucket root, or serve `index.html`
anywhere with a `<base href>` pointing at the bucket. Because keys are the
content hash, uploads are idempotent and safe to dedupe across archives:
identical bytes always produce the same key.

`manifest.json` records the `mime` for each key so a static host (or this demo
server) can send the right `Content-Type` without sniffing.

## Run it

```
make demo                       # extract the sample archive + serve at :8000
# or, explicitly:
node hash-extract.js <archive.mhtml> out
node server.js out 8000
curl -s http://127.0.0.1:8000/          # entry HTML, refs are bare "<hash>.<ext>"
curl -si http://127.0.0.1:8000/<key>    # a resource, correct Content-Type
```

`server.js` is plain `node:http` with zero dependencies: it serves `out/` as a
static bucket, reading `Content-Type` from `manifest.json` and refusing any path
not listed there.

### Producing the directory

- **Canonical:** `mhtml extract <archive> -o out --naming hash`
  (`--base-href <URL>` to target a bucket prefix). Build the binary with
  `cargo build --release -p cli`.
- **`hash-extract.js` (used here):** drives the same shared `serve` layer
  through the in-repo wasm build (`crates/wasm/pkg-node`), so the demo is
  runnable and CI-verifiable independent of the CLI flag. It emits the identical
  directory layout. Rebuild the wasm with `make wasm` if the Rust changes.
