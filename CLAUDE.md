# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`gpui-mcp-inspector` — an MCP server (`gpui-mcp-server` binary) that lets an AI
agent inspect and drive a running GPUI app, plus the wire types
(`gpui_mcp_protocol` lib) shared with the in-app side.

Three documents, three audiences — keep whichever ones a change touches in
sync:

- **README.md** — for someone using or installing it. The authoritative
  description of behaviour.
- **docs/ARCHITECTURE.md** — for someone changing it: the measurements the
  design rests on, the frame contract, and which apparent simplifications are
  load-bearing. Read it before altering behaviour; add to it when a decision
  is made that a later reader would otherwise undo.
- **PLAN.md** — the roadmap, with stages marked done as they land.

A fourth lives in the code: `src/docs.rs` is what the server tells the *agent*,
and a test fails if it stops matching the tool list.

```
MCP client ──stdio JSON-RPC──▶ gpui-mcp-server ──Unix socket, NDJSON──▶ gpui_component::mcp (in the app)
```

- `src/main.rs` — the server: MCP stdio loop, socket discovery, tool list,
  forwards each tool call as one IPC request on a fresh connection.
- `src/protocol.rs` — `IpcRequest`/`IpcResponse`, `UiElement`, param structs,
  `methods::*` constants. Tool names == IPC method names.
- `src/script.rs` — recording a session to a file and replaying it, plus the
  command-line `replay` mode. Server-local: a script is a list of tool calls,
  so nothing about it reaches the app.
- `src/docs.rs` — what the server tells an agent about itself: the
  `initialize` instructions, the `gpui_guide` tool, the guide resources and the
  `onboard` prompt, all generated from one topic table. Server-local: no IPC
  method, so the guide works with no app running. See PLAN.md for why.
- The in-app side is **not** here: it is the `mcp` module/feature of
  [stefan-siebert/gpui-component](https://github.com/stefan-siebert/gpui-component),
  which depends on this repo by git (`package = "gpui-mcp-inspector"`, imports
  `gpui_mcp_protocol::protocol::*`).
- Below that sits a third checkout, `../gpui-fork` (the zed fork, branch
  `gpui-mcp-patches-v2`), which gpui-component patches in by path. `a11y_tree`
  needs `Window::set_a11y_force_active` from it. A change there must be
  recorded in the fork's `FORK_CHANGES.md`, and must be type-checked by
  building gpui-component — the fork's own workspace does not build on Windows
  (an unrelated dependency exceeds the path limit).

## Rules

- **Do not rename** the package (`gpui-mcp-inspector`) or the lib
  (`gpui_mcp_protocol`): gpui-component and CI builds resolve both by name.
- Changes to `protocol.rs` are wire changes. Additions must be `#[serde(default)]`
  so an older app and a newer server (or vice versa) keep talking. Check
  `../gpui-component/crates/ui/src/mcp.rs` before removing or renaming anything.
- Adding a tool: add the method constant and `methods::ALL` entry in
  `protocol.rs`, a params struct if needed, the JSON schema in `tools_list()`
  in `main.rs`, the handler in gpui-component, the row in README.md, and the
  entry in the `tools` topic of `docs.rs` — a test fails until that last one
  exists — and a row in the tool table of gpui-component's `docs/docs/mcp.md`
  (plus its `zh-CN` mirror, which that repo requires).
- Docs and code comments in English.
- `wait_for` and `batch` are answered asynchronously by the app (it waits for
  frames); every other method is answered from the frame already painted. Input
  methods answer only after the frame showing their effect — that is protocol
  v2, and it is why the version was bumped despite the types staying
  compatible.

## Commands

```sh
cargo build --release                       # → target/release/gpui-mcp-server
cargo test
cargo clippy --all-targets -- -D warnings   # CI gate
cargo fmt --check                           # CI gate
```

## Environment

- `GPUI_MCP_APP` — app name passed to `init_mcp`; restricts discovery.
- `GPUI_MCP_PID` — with `GPUI_MCP_APP`: exact socket, no discovery.
- Sockets: `{temp_dir}/gpui-mcp-{app}-{pid}.sock`. Discovery deletes stale ones.
- Diagnostics go to stderr; stdout is reserved for MCP JSON-RPC.
