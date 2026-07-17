# mhtml-parser

A Rust parser for MHTML (`.mhtml` / `.mht`), the single-file web archive a browser saves when you pick "Save page as → Webpage, Single File". An MHTML file is a MIME `multipart/related` document (RFC 2557). One file holds the page's HTML plus every resource it needs to render offline: CSS, images, fonts, and scripts.

Real archives are messy, so the parser is lenient where it has to be: odd quoted-printable, quirky part boundaries, folded headers, and non-UTF-8 text. A golden test suite checks its output byte for byte.

## Status

**Working.** The golden suite passes byte for byte ([`crates/parser/tests/golden.rs`](crates/parser/tests/golden.rs)). The `mhtml` CLI's `list` and `extract` commands work end to end. A [`cargo-fuzz`](crates/parser/fuzz/) target throws arbitrary bytes at the parser to make sure it never panics. The `mhtml-serve` layer and the wasm bindings run a small Node demo that serves an archive straight from memory.

## Workspace

| Crate | Path | Purpose |
|-------|------|---------|
| `mhtml` | `crates/parser/` | The core library. Parses MHTML from `&[u8]` with no I/O. Iterates parts lazily and decodes each body on demand (quoted-printable, base64, 7bit/8bit, binary). |
| `mhtml-serve` | `crates/serve/` | A shared layer over `mhtml`: MIME types, entry-point selection, URL matching, and HTML/CSS rewriting. No parsing, no disk I/O. Used by `mhtml-cli` and `mhtml-wasm`. |
| `mhtml-cli` (binary: `mhtml`) | `crates/cli/` | The `mhtml` command. Inspects an archive and extracts it to a folder you can open. |
| `mhtml-wasm` | `crates/wasm/` | `wasm-bindgen` bindings over `mhtml-serve` for npm, built with `wasm-pack` (`nodejs` and `web` targets). Bindings only, no logic of its own. |

## Library — `mhtml`

Parse an archive that is already in memory:

```rust
let data = std::fs::read("page.mhtml")?;
let archive = mhtml::Archive::parse(&data)?;     // reads the root header only

for part in archive.parts() {                    // lazy: parses parts on demand
    let part = part?;
    println!("{:?} {:?}", part.content_type, part.content_location);
    let body = part.body()?;                     // decodes on demand (Cow<[u8]>)
}
```

- **Lenient.** `parts()` yields each good part, then a single `Err` when it hits corruption. Everything before the error still stands, so you can salvage a damaged archive.
- **Strict.** `parse_all()` is all-or-nothing. One bad part fails the whole archive.
- Bad input returns an `Err`. It never panics.

The rest of the `Archive` API: `parse`, `parse_all`, `parts`, `creation_date`, `snapshot_content_location`, and the free function `mhtml::content_id_to_cid_url(id)`.

## Library — `mhtml-serve`

`Bundle` turns an archive into something you can serve or upload. The CLI, the wasm bindings, and the S3 demo all build on it. It parses strictly, gives you each resource with the metadata to serve it, and rewrites the HTML/CSS references to point wherever you choose (see [naming strategies](#resource-naming-strategies)).

```rust
use mhtml_serve::bundle::Bundle;
use mhtml_serve::naming::NamingStrategy;

let data = std::fs::read("page.mhtml")?;
let bundle = Bundle::from_bytes(&data)?;         // strict parse; Err on malformed input

// Content-address every resource and get the whole S3 upload plan in one call:
for asset in bundle.manifest(&NamingStrategy::ContentHash, None) {
    // upload `asset.bytes` to      s3://bucket/{asset.key}   (key = "<sha256>.<ext>")
    // with  Content-Type: {asset.mime}[; charset={asset.charset}]
    // `asset.is_entry == true` marks the rewritten root HTML document.
}
```

Or reach for the pieces directly:

```rust
bundle.entry_index()        // -> Option<usize>            root document index
bundle.entry()              // -> Option<&Resource>        { url, mime, charset, body }
bundle.get(url)             // -> Option<&Resource>        by normalized URL (incl. cid:)
bundle.urls()               // -> impl Iterator<Item=&str>
bundle.resource_count()     // -> usize
bundle.resource_key(i, &NamingStrategy::ContentHash)   // -> Option<String>  "<hash>.<ext>"
bundle.rewritten(i, &strategy, base_href)              // -> Option<Vec<u8>> html/css, refs re-pointed
bundle.manifest(&strategy, base_href)                  // -> Vec<Asset> { key, mime, charset, bytes, is_entry }
```

`MirrorPath` keeps the original URL layout. `ContentHash` gives flat `<sha256>.<ext>` keys. `base_href` adds a `<base href>` to the entry page, so its relative links resolve against a bucket or CDN prefix. See [naming strategies](#resource-naming-strategies).

## CLI

```console
$ mhtml list page.mhtml
  #  TYPE        ENCODING          SIZE  LOCATION
  0  text/html   quoted-printable  52K   https://example.com/blog/post
  1  text/css    quoted-printable  8K    https://example.com/style/main.css
  2  image/png   base64            34K   https://example.com/img/logo.png

$ mhtml extract page.mhtml -o out/
```

`extract` writes each part to a path that mirrors its original URL, so relative links keep working. It rewrites absolute and `cid:` links in the HTML and CSS to local paths. The HTML rewriting uses [lol_html](https://crates.io/crates/lol_html).

```
out/
├── index.html                → redirect to the entry document
├── example.com/
│   ├── blog/post.html        ← entry (root document)
│   ├── style/main.css
│   └── img/logo.png
└── cdn.example.net/
    └── fonts/inter.woff2
```

Open the entry page from disk and it renders offline.

## Resource naming strategies

`extract` names resources one of two ways, set with `--naming`:

- **`mirror`** (default). Keep the original URL layout on disk (`host/dir/file.ext`). Relative links resolve just like they did online. This is the tree above.
- **`hash`**. Content-addressed. Each resource is stored and referenced as `<sha256>.<ext>`: the SHA-256 of its bytes, with an extension from its MIME type. The URL is ignored. You get a flat folder whose file names are ready-made S3 keys, and identical bytes share one key.

```console
$ mhtml extract page.mhtml -o out --naming hash
$ mhtml extract page.mhtml -o out --naming hash --base-href https://cdn.example/assets/
```

Hash mode writes `out/<hash>.<ext>` for each resource, a rewritten `out/index.html`, and `out/manifest.json` (`[{ "key", "mime", "url" }]`, the upload plan). Every link in the rewritten HTML/CSS is relative and never starts with `/`, so the output moves anywhere. Put the entry page next to the resources and the bare keys resolve as siblings, with no `<base>` needed. Or host the resources under a bucket or CDN prefix and pass `--base-href <URL>`, which adds a matching `<base href>` to `index.html`. See [`examples/s3-demo/`](examples/s3-demo/) for the full workflow and [`docs/naming-strategy.md`](docs/naming-strategy.md) for the design.

## WASM & Node server

`mhtml-serve` also compiles to WebAssembly (`mhtml-wasm`), so the same code runs in Node or the browser. You need the `wasm32-unknown-unknown` target and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

make wasm        # build pkg-node/ (nodejs) and pkg-web/ (web) into crates/wasm/
make wasm-test   # parse the checked-in fixture through the wasm API and assert
```

The bindings expose a small `MhtmlArchive` class. Here it publishes an archive to S3, where each file name is its own object key:

```js
import { readFile } from "node:fs/promises";
import { MhtmlArchive } from "./crates/wasm/pkg-node/mhtml_wasm.js";

const archive = MhtmlArchive.parse(await readFile("page.mhtml"));

for (let i = 0; i < archive.resource_count(); i++) {
  await bucket.put(archive.resource_key(i, "hash"), archive.rewritten_at(i, "hash"), {
    contentType: archive.resource_mime(i),
  });
}
```

`resource_key(i, "hash")` returns `"<sha256>.<ext>"`. `rewritten_at(i, "hash")` returns the resource's bytes with its in-archive links re-pointed to those keys (HTML and CSS are rewritten; everything else passes through). Pass `"mirror"` instead to keep the URL layout, and an optional `base_href` as the last argument for a bucket or CDN prefix. These are the same options as the Rust [`Bundle` API](#library--mhtml-serve). Other methods: `entry_url()`, `entry_index()`, `urls()`, `resource_charset(i)`, `body_at(i)` (raw, un-rewritten), `content_type_at(i)`, and `entry_rewritten("hash")` for just the root document. See [`docs/naming-strategy.md`](docs/naming-strategy.md) for the full design.

`make serve-demo` runs a small `node:http` server on those bindings, with no dependencies:

```bash
make serve-demo ARGS="page.mhtml 8000"
```

It serves the entry page at <http://127.0.0.1:8000/> and each resource at `/<sha256>.<ext>`, the same key the rewritten HTML/CSS points to. Every response carries the right `Content-Type`.

## Development

Strict red/green TDD, and code has to earn its place. The working agreement is in [`CLAUDE.md`](CLAUDE.md).

### Setup

You need a Rust toolchain from [rustup](https://rustup.rs) (stable, with `rustfmt` and `clippy`) and a C linker (`gcc`/`cc`):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Fuzzing is optional and needs the nightly toolchain plus [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz):

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

### Commands

```bash
cargo test                                  # golden + unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo run -p mhtml-cli -- <args>            # run the mhtml CLI
cargo +nightly fuzz run parse               # fuzz the parser (from crates/parser/)
```

## License

BSD 3-Clause — see [`LICENSE`](LICENSE).
