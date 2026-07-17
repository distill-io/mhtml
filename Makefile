# mhtml-parser — common developer targets.
# Run `make` or `make help` to list them.

CARGO ?= cargo
# Args passed to `make run`, e.g. `make run ARGS="list page.mhtml"`.
ARGS ?=

.DEFAULT_GOAL := help

.PHONY: help build test fmt fmt-check clippy lint check run fuzz wasm wasm-test serve-demo \
	conservation-setup capture verify conservation publish-dry publish clean

help: ## List available targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

build: ## Build the whole workspace
	$(CARGO) build --workspace

test: ## Run all tests (unit + integration + golden)
	$(CARGO) test --workspace

fmt: ## Format all code in place
	$(CARGO) fmt --all

fmt-check: ## Check formatting without writing changes
	$(CARGO) fmt --all -- --check

clippy: ## Lint with warnings denied
	$(CARGO) clippy --all-targets -- -D warnings

lint: fmt-check clippy ## Formatting + clippy checks (no edits)

check: lint test ## Full quality gate: fmt-check, clippy, tests

run: ## Run the mhtml CLI: make run ARGS="list page.mhtml"
	$(CARGO) run -p mhtml-cli -- $(ARGS)

fuzz: ## Fuzz the parser (needs nightly + cargo-fuzz)
	cd crates/parser && $(CARGO) +nightly fuzz run parse

wasm: ## Build the wasm bindings for node and web (needs wasm-pack)
	wasm-pack build crates/wasm --target nodejs --out-dir pkg-node
	wasm-pack build crates/wasm --target web --out-dir pkg-web

wasm-test: ## Run the server-free wasm smoke test
	node examples/node-server/smoke.js

serve-demo: ## Serve an archive: make serve-demo ARGS="page.mhtml 8000"
	node examples/node-server/server.js $(ARGS)

conservation-setup: ## Install the conservation harness (Playwright + Chromium)
	cd tools/conservation && npm install && npx playwright install chromium

capture: ## Capture sites.json into corpus/: make capture ARGS="lang charset"
	cd tools/conservation && node run/capture.js $(ARGS)

verify: ## Verify conservation of captured archives (REF vs extracted)
	$(CARGO) build --release -p mhtml-cli
	cd tools/conservation && node run/verify.js $(ARGS)

conservation: ## Full pass: build cli, capture all sites, verify, report
	$(CARGO) build --release -p mhtml-cli
	cd tools/conservation && node run/capture.js && node run/verify.js

# crates.io publish. `--workspace` publishes every crate in dependency order,
# waiting for each to land in the index before the next.
publish-dry: ## Dry-run: package every crate without uploading (validates metadata + order)
	$(CARGO) publish --workspace --dry-run

publish: check ## Publish all crates to crates.io in order (run `cargo login` first)
	$(CARGO) publish --workspace

clean: ## Remove build artifacts
	$(CARGO) clean
