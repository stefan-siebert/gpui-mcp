.PHONY: build release test clean install

# Build in debug mode
build:
	cargo build

# Build in release mode
release:
	cargo build --release

# Run tests
test:
	cargo test

# Run example
example:
	cargo run --example gpui_integration

# Clean build artifacts
clean:
	cargo clean
	rm -f /tmp/gpui-mcp.sock

# Install to /usr/local/bin (requires sudo)
install: release
	sudo cp target/release/gpui-mcp-server /usr/local/bin/
	@echo "Installed to /usr/local/bin/gpui-mcp-server"
	@echo ""
	@echo "Update Claude Desktop config to use:"
	@echo '  "command": "/usr/local/bin/gpui-mcp-server"'

# Install for current user only
install-user: release
	mkdir -p ~/.local/bin
	cp target/release/gpui-mcp-server ~/.local/bin/
	@echo "Installed to ~/.local/bin/gpui-mcp-server"
	@echo ""
	@echo "Make sure ~/.local/bin is in your PATH"
	@echo "Update Claude Desktop config to use:"
	@echo '  "command": "$(HOME)/.local/bin/gpui-mcp-server"'

# Development: watch and rebuild on changes
watch:
	cargo watch -x build

# Format code
fmt:
	cargo fmt

# Check code without building
check:
	cargo check

# Run clippy lints
lint:
	cargo clippy -- -D warnings

# Show all available MCP servers (for debugging)
list-config:
	@echo "Claude Desktop config location:"
	@echo "  macOS: ~/Library/Application Support/Claude/claude_desktop_config.json"
	@echo "  Linux: ~/.config/Claude/claude_desktop_config.json"
	@echo "  Windows: %APPDATA%/Claude/claude_desktop_config.json"

help:
	@echo "Available targets:"
	@echo "  build         - Build in debug mode"
	@echo "  release       - Build in release mode"
	@echo "  test          - Run tests"
	@echo "  example       - Run integration example"
	@echo "  clean         - Remove build artifacts"
	@echo "  install       - Install to /usr/local/bin (requires sudo)"
	@echo "  install-user  - Install to ~/.local/bin"
	@echo "  watch         - Watch and rebuild on changes"
	@echo "  fmt           - Format code"
	@echo "  check         - Check code without building"
	@echo "  lint          - Run clippy lints"
	@echo "  list-config   - Show Claude Desktop config location"
