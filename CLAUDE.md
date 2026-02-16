# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**gpui-mcp-inspector** is a Rust MCP (Model Context Protocol) server that enables Claude to inspect and interact with GPUI-based applications (like Elane) via UI inspection, screenshots, and event simulation. Documentation is written in German.

### Two-Layer Communication Architecture

```
Claude Desktop ←→ gpui-mcp-server (stdio, JSON-RPC 2.0) ←→ GPUI App (Unix socket, newline-delimited JSON)
```

- **MCP Server** (`main.rs`): Binary that reads MCP requests from stdin, forwards them to the GPUI app over a Unix socket, and returns results on stdout. Synchronous stdio loop with async socket communication.
- **Protocol Library** (`protocol.rs`): Shared types (`UiElement`, `UiTree`, `WindowInfo`, `IpcRequest`/`IpcResponse`, etc.) published as the `gpui_mcp_protocol` crate.
- **GPUI Integration** (`gpui_integration.rs`): Reference implementation showing the `AppInterface` trait and `GpuiIpcServer`. GPUI apps implement `AppInterface` to expose their UI tree, handle clicks/keys, and provide state.

## Build & Dev Commands

```bash
make build       # cargo build (debug)
make release     # cargo build --release
make test        # cargo test
make lint        # cargo clippy -- -D warnings
make fmt         # cargo fmt
make check       # cargo check (type-check only)
make example     # cargo run --example gpui_integration
make watch       # cargo watch -x build (requires cargo-watch)
```

## Key Trait: `AppInterface`

Defined in `gpui_integration.rs`. Any GPUI app must implement this to be inspectable:
- `get_ui_tree()`, `get_element()`, `get_windows()`, `take_screenshot()` — inspection
- `click_element()`, `send_key()`, `execute_action()` — automation
- `get_app_state()`, `get_logs()` — debugging

## MCP Tools Exposed

Inspection: `inspect_ui_tree`, `get_element`, `get_windows`, `take_screenshot`
Automation: `click_element`, `send_key`, `execute_action`
State: `get_app_state`, `get_logs`

Method constants are in `protocol::methods`.

## Environment Variables

- `GPUI_MCP_SOCKET` — Unix socket path (default: `/tmp/gpui-mcp.sock`)

## Design Notes

- Each MCP request opens a new socket connection (stateless)
- Cross-thread data uses `Arc<Mutex<>>`
- Debug output goes to stderr (useful for debugging via `2>/tmp/mcp-debug.log`)
- Local-only security model via Unix sockets — not for production builds
