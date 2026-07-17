# S3-style content-hash demo.
#
#   make demo            # extract the sample archive, then serve it
#   make extract         # just produce ./out/ from ARCHIVE
#   make serve           # serve ./out/ (run `make extract` first)
#   make clean
#
# Override the archive or output dir:
#   make demo ARCHIVE=/path/to/page.mhtml OUT=/tmp/bucket PORT=9000

ROOT    := ../..
ARCHIVE ?= $(ROOT)/crates/cli/tests/fixtures/simple.mhtml
OUT     ?= out
PORT    ?= 8000

.PHONY: demo extract serve clean wasm cli-extract

# Build the wasm the helper drives (needed once, or after Rust changes).
wasm:
	wasm-pack build $(ROOT)/crates/wasm --target nodejs --out-dir pkg-node

# Produce ./out/ via the in-repo wasm build (runnable today).
extract:
	node hash-extract.js $(ARCHIVE) $(OUT)

# The canonical path, once the CLI --naming flag has landed:
#   cargo build --manifest-path $(ROOT)/Cargo.toml --release -p cli
#   $(ROOT)/target/release/mhtml extract $(ARCHIVE) -o $(OUT) --naming hash
cli-extract:
	$(ROOT)/target/release/mhtml extract $(ARCHIVE) -o $(OUT) --naming hash

serve:
	node server.js $(OUT) $(PORT)

demo: extract serve

clean:
	rm -rf $(OUT)
