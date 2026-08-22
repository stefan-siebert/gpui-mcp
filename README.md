# gpui-mcp — an MCP inspector for GPUI apps

Lets an AI agent (Claude Code, Claude Desktop, anything that speaks the
[Model Context Protocol](https://modelcontextprotocol.io)) **look at and drive a
running [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
application**: read the element tree with bounds and source locations, see
what has focus and which key contexts are active, type, press keys, click,
dispatch named actions, take screenshots, and read an app-defined state
snapshot. The agent verifies a GUI change the way a person would — by using
the app — instead of guessing from the code or blind-firing `SendKeys`.

Works on Linux, macOS and Windows.

## How it fits together

```
 Claude Code / Claude Desktop
        │  MCP over stdio (JSON-RPC)
        ▼
 gpui-mcp-server            ← this repo: src/main.rs
        │  one JSON request per connection over a Unix-domain socket
        │  {temp_dir}/gpui-mcp-{app}-{pid}.sock   (uds_windows on Windows)
        ▼
 your GPUI app
   gpui_component::mcp       ← the in-app side: a listener thread queues each
                               request; the GPUI main thread polls the queue
                               and answers from gpui's inspector data
```

Three pieces, in three repositories:

| piece | where | what it is |
|---|---|---|
| `gpui-mcp-server` binary | this repo, `src/main.rs` | the MCP server an agent talks to; discovers the app's socket, forwards tool calls |
| `gpui_mcp_protocol` library | this repo, `src/lib.rs`, `src/protocol.rs` | the request/response types shared by both ends |
| `gpui_component::mcp` module | [stefan-siebert/gpui-component](https://github.com/stefan-siebert/gpui-component), feature `mcp` | the in-app server: socket, main-thread dispatch, element tree, screenshots, `init_mcp()` |

The element tree and screenshots come from gpui's inspector API, which
upstream exposes only partially — the
[stefan-siebert/zed](https://github.com/stefan-siebert/zed) fork, branch
`gpui-mcp-patches-v2`, carries the patches (`window.inspector_elements()`,
`window.render_to_image()`). gpui-component's `mcp` feature enables gpui's
`inspector` feature and depends on that fork, so an app gets the whole stack
by depending on gpui-component alone.

## Building the server

```sh
cargo build --release
# → target/release/gpui-mcp-server   (.exe on Windows)
```

No runtime dependencies. The binary is small and stateless: it re-discovers
the app's socket on **every** tool call, so restarting the app (new PID, new
socket name) needs no restart of the server or of the agent session.

## Putting it into an app

1. Depend on gpui-component with the feature on — behind a feature of your
   own, so the shipped binary does not carry an IPC server:

   ```toml
   [features]
   mcp = ["gpui-component/mcp"]

   [dependencies]
   gpui = { git = "https://github.com/stefan-siebert/zed", branch = "gpui-mcp-patches-v2" }
   gpui-component = { git = "https://github.com/stefan-siebert/gpui-component", branch = "main" }
   ```

   gpui-component names that same git source for gpui, so the two resolve to
   **one** gpui. A second copy (a path dependency next to the git one, or
   upstream gpui from crates.io) makes every type mismatch.

2. Start the in-app server once, after `gpui_component::init`:

   ```rust
   app.run(|cx| {
       gpui_component::init(cx);
       #[cfg(feature = "mcp")]
       {
           // The name is how the server finds this app among others.
           gpui_component::mcp::init_mcp(cx, "my-app");
           // Optional: whatever `get_app_state` should report for your app.
           gpui_component::mcp::mcp_set_app_state_provider(|cx| {
               serde_json::json!({ "rows": 0, "selected": null })
           });
       }
       // ... windows, views ...
   });
   ```

   `init_mcp` binds `{temp_dir}/gpui-mcp-my-app-{pid}.sock` and spawns the
   listener thread. Requests are executed on gpui's main thread, so they see
   consistent state and can dispatch real input.
   `gpui_component::mcp::mcp_log(..)` appends to the 500-entry buffer that
   `get_logs` returns.

3. Build with the feature for development (`cargo build --features mcp`) and
   without it for release.

## Registering the server with an agent

**Claude Code** — one command per project, run inside the app's checkout.
`GPUI_MCP_APP` is the name you passed to `init_mcp`:

```sh
claude mcp add --transport stdio gpui-inspector --env GPUI_MCP_APP=my-app -- /abs/path/to/gpui-mcp-server
```

The default scope `local` records it privately for this project; `-s project`
writes a `.mcp.json` into the repo for everyone who checks it out; `-s user`
makes it global (then leave `GPUI_MCP_APP` out and let discovery pick the
newest running app). `claude mcp get gpui-inspector` shows the status —
"Connected" needs a running app with `init_mcp`; a Claude Code session that
was already open must be restarted before the `mcp__gpui-inspector__*` tools
appear.

**Claude Desktop** — add to `claude_desktop_config.json`
(macOS `~/Library/Application Support/Claude/`, Linux `~/.config/Claude/`,
Windows `%APPDATA%\Claude\`):

```json
{
  "mcpServers": {
    "gpui-inspector": {
      "command": "/abs/path/to/gpui-mcp-server",
      "env": { "GPUI_MCP_APP": "my-app" }
    }
  }
}
```

### Which app does the server talk to?

| environment | behaviour |
|---|---|
| `GPUI_MCP_APP` + `GPUI_MCP_PID` | exactly `{temp_dir}/gpui-mcp-{app}-{pid}.sock`, no discovery |
| `GPUI_MCP_APP` only | the newest running instance of that app |
| neither | the newest GPUI app found; a warning on stderr when there is more than one |

Discovery scans the OS temp directory for `gpui-mcp-*.sock`, probes each, and
deletes the ones nothing listens on (left behind by a crashed app).

## The tools

| tool | what it does |
|---|---|
| `get_windows` | open windows with id, title, bounds, active flag — the window ids the other tools take |
| `get_app_state` | window overview plus whatever the app's state provider returns (`app` key) |
| `inspect_ui_tree` | the element hierarchy: id, type (from the source file), bounds, `source_location`, children, text. Filters: `max_depth`, `window_id`, `root_element_id`, `element_type_filter`, `text_filter`, `format: compact` |
| `get_element` | one element with its full subtree |
| `get_focus_info` | focused element and the active key-context chain — the first thing to check when a key binding does not fire |
| `list_actions` | the app's gpui actions; `include_bindings` adds key bindings and docs, `only_available` keeps those whose context predicate matches the current focus |
| `execute_action` | dispatch a named action through the focus chain, as a keystroke would |
| `send_key` | one keystroke in gpui's notation (`enter`, `pagedown`, `f5`; modifiers as flags) |
| `type_text` | a string, one keystroke per character |
| `click_element` | left/right/middle click at an element's centre or at window pixel coordinates |
| `take_screenshot` | the window (or one element, cropped) rendered to PNG |
| `get_logs` | the in-app log buffer (≤500 lines) |

Every input tool (`send_key`, `type_text`, `click_element`, `execute_action`)
returns the app state and focus info *after* the event was dispatched, so the
agent usually sees the effect without a second round trip. The app's own
debounce or async work may not have finished yet — read `get_app_state` again
if a value looks stale.

**Element ids** come in three forms, all accepted wherever an id is taken: the
full id (`WindowId(1)/view-1.panel[0]`), the global id (`view-1.panel`), or a
suffix (`panel`) — the first match wins. Give the elements you want to target
stable ids in the app (`div().id("results")`).

## Platform notes

- **Windows:** the socket is a `uds_windows` AF_UNIX socket in `%TEMP%`; the
  server and the app must run as the same user. Building needs what gpui
  needs anyway (Windows SDK, DirectX).
- **Linux/macOS:** a plain `std::os::unix::net` socket in `$TMPDIR` (or
  `/tmp`).
- Screenshots are rendered by gpui itself (`render_to_image`, no screen
  capture), so they work on an overlapped window and need no accessibility
  permission.

## Troubleshooting

- *"No GPUI app found"* — the app is not running, was built without the
  feature, or its `init_mcp` did not run. The app prints
  `[MCP] IPC Server listening on …` to stderr when it did.
- *Tools do not appear in Claude Code* — restart the session after
  `claude mcp add`; check `claude mcp get gpui-inspector`.
- *Keys reach the wrong element* — `get_focus_info` shows the focus chain and
  key contexts. When an editor keeps focus, bind the list keys in the input's
  context too (gpui gives the later binding precedence).
- Server-side diagnostics go to stderr, which the agent client usually logs;
  `GPUI_MCP_APP=… gpui-mcp-server < /dev/null` shows discovery output directly.

## Security

The in-app server gives anything that can reach the socket full control of
the UI and a view of its state. It is a development tool: keep it behind a
feature flag, never enable it in a shipped build, and remember the socket
lives in a per-user temp directory but is not otherwise authenticated.

## Developing

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                 # protocol serde defaults + socket-name parsing
```

CI (`.github/workflows/ci.yml`) runs the same three on Linux and Windows.
This repo is deliberately small: the binary in `src/main.rs`, the wire types
in `src/protocol.rs`. The in-app logic (main-thread dispatch, how gpui's flat
inspector list becomes a tree, rendering screenshots) lives in
`gpui_component::mcp`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
