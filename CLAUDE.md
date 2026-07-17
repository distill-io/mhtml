# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust MHTML parser. MHTML (`.mhtml` / `.mht`) is the single-file web archive a browser writes via "Save as → Webpage, Single File" — a MIME `multipart/related` document (RFC 2557 + RFC 2045/2046): one root HTML part plus every subresource (CSS, images, fonts, scripts) needed to render offline. The goal is faithful, lossless parsing so the original page can be reconstructed.

A Cargo **workspace** (crates under `crates/`), layered by responsibility. The split is a hard boundary — logic belongs in the lowest layer that can own it:
- `crates/parser` (crate `mhtml`) — the library. All **MHTML/MIME parsing** lives here. No I/O beyond what a caller hands in.
- `crates/serve` (crate `mhtml-serve`) — the **shared consumption layer** over `mhtml` (`bundle`, `ctype`, `entry`, `locate`, `mime`, `rewrite_html`, `rewrite_css`): MIME resolution, entry-point selection, URL matching, HTML/CSS rewriting. No parsing, no disk I/O.
- `crates/cli` (crate `mhtml-cli`, binary `mhtml`) — disk extraction and listing (`naming`, `extract`, `list`, thin `resolve`). Zero parsing; `main.rs` is a thin clap adapter over `lib.rs`.
- `crates/wasm` (crate `mhtml-wasm`) — `wasm-bindgen` bindings over `mhtml-serve`. **Bindings only**: any real logic belongs in `mhtml-serve`.

If you find yourself parsing MIME outside `mhtml`, or duplicating consumption logic in `mhtml-cli`/`mhtml-wasm` instead of sharing it through `mhtml-serve`, it belongs in the lower layer.

## Non-negotiable working agreement

These rules are the reason this file exists. Follow them literally.

### Red/green TDD is the way
Every behavior change, one behavior at a time:
1. **Red** — write exactly one failing test for the next small behavior. Run it. Confirm it fails *for the reason you expect* (an assertion, not an unrelated compile error).
2. **Green** — the minimum code to pass. Nothing more — no fields, params, error variants, or config "for later".
3. **Refactor** — clean up with the test as a safety net; tests stay green.

Don't write production code without a failing test demanding it. Don't write more test than you need to fail. Code no test exercises gets deleted, or its test written first. Keep one test running constantly during the loop — that tight feedback is the point.

### Every line of code must earn its place
Every line is a liability paid for by a concrete, present need.
- No speculative generality — no trait, generic, or abstraction with a single implementor added "in case".
- No dead code, no `pub` wider than a caller needs, no options or flags nothing uses.
- Prefer deleting code to adding it. If a test passes with less, use less.
- Reach for a dependency only when it clearly beats a small amount of owned code, and justify it — but don't reimplement battle-tested primitives (e.g. base64).

### Verify, don't assume
Don't guess how the code, a dependency, or a tool behaves — check it. Read the actual source, run the actual command, write a throwaway probe. A confirmed fact beats a plausible assumption. When you make a claim, be ready to say how you verified it.

### Concise and human-readable
- **Comments** explain *why*, not *what* — the code already says what. Keep them short, delete stale ones, and match the surrounding density and idiom.
- **Commit messages** are concise and human-readable: a short imperative subject, and a brief *why* only when it isn't obvious. Describe the change, not a file-by-file changelog.
- Write code that reads like the code around it.

### Quality bar
- `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass before a change is done.
- Errors are typed and meaningful (`thiserror`). The library never `panic!`s or `unwrap()`s on malformed input — that is a normal, tested `Err`. `unwrap`/`expect` only where an invariant is genuinely impossible, and then say why.
- Parsing is fallible and adversarial: real archives are truncated, mislabeled, wrongly-encoded, and huge. Robustness on bad input is a first-class, tested requirement — not an afterthought.
- The golden acceptance suite (`crates/parser/tests/golden.rs`) pins the parser's byte-exact output. Never weaken or delete a test to make a change pass — if a change conflicts with a test, the change is wrong until proven otherwise.

## Commands

```bash
cargo test --workspace                # all tests (unit + integration + doctests)
cargo test -p mhtml                   # just the library crate
cargo test <substring>                # tests whose name contains <substring>
cargo test -- --exact path::to::test  # one specific test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo run -p mhtml-cli -- <args>      # run the mhtml CLI
(cd crates/parser && cargo +nightly fuzz run parse)   # fuzz the parse surface (nightly + cargo-fuzz)
```

## Architecture notes for the parser

The MHTML/MIME shape matters more than any single file:
- The archive is a top-level MIME message. Headers (`Content-Type: multipart/related`, `boundary=...`, `Content-Location`) drive everything; `boundary` delimits parts.
- Each part has its own headers — most importantly `Content-Type`, `Content-Location` (the original URL, the key for resolving references), and `Content-Transfer-Encoding` (`quoted-printable` for text, `base64` for binary).
- Header parsing handles folded (continued) lines, quoted parameter values, and charset in `Content-Type`.
- Decoding is per-part, driven by `Content-Transfer-Encoding`; hand callers the *decoded* bytes plus the metadata needed to place the resource (its Content-Location and type).
- Keep the layers distinct and independently tested: boundary splitting → header parsing → transfer-decoding → the part model → the whole-archive model (root part + resource map keyed by Content-Location). Don't collapse them into one pass.

### Tests and fixtures
- Unit tests live next to code (`#[cfg(test)] mod tests`) for the fine-grained layers (header parsing, quoted-printable/base64 decoding, boundary splitting).
- Integration tests (`tests/`) exercise the public API against small, checked-in `.mhtml` fixtures. Prefer minimal hand-authored archives that isolate one concern over large real-world captures; document what each fixture proves.
