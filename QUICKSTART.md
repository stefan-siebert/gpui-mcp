# Quick start

Superseded by [README.md](README.md), which is current: build the server
(`cargo build --release`), add `gpui-component` with feature `mcp` to the
app and call `gpui_component::mcp::init_mcp(cx, "<app>")`, then register the
server with `claude mcp add --transport stdio gpui-inspector --env
GPUI_MCP_APP=<app> -- /abs/path/to/gpui-mcp-server`.

The earlier text here described a hand-written `AppInterface` + tokio
integration that no longer exists.
