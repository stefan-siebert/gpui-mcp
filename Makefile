.PHONY: build release test lint fmt check clean install install-user help

build:          ## Debug build
	cargo build

release:        ## Release build → target/release/gpui-mcp-server
	cargo build --release

test:           ## Run tests
	cargo test

lint:           ## Clippy with warnings denied (CI gate)
	cargo clippy --all-targets -- -D warnings

fmt:            ## Format code
	cargo fmt

check:          ## fmt check + clippy + test, as CI runs them
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

clean:          ## Remove build artifacts
	cargo clean

install: release   ## Install to /usr/local/bin (needs sudo)
	sudo install -m 755 target/release/gpui-mcp-server /usr/local/bin/
	@echo "Installed to /usr/local/bin/gpui-mcp-server"

install-user: release   ## Install to ~/.local/bin
	install -d ~/.local/bin
	install -m 755 target/release/gpui-mcp-server ~/.local/bin/
	@echo "Installed to ~/.local/bin/gpui-mcp-server (make sure it is on PATH)"

help:           ## Show this help
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed -E 's/:.*##\s*/\t/' | column -t -s $$'\t'
