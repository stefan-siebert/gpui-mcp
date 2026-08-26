# gpui-mcp — an MCP inspector for GPUI apps

Lets an AI agent (Claude Code, Claude Desktop, anything that speaks the
[Model Context Protocol](https://modelcontextprotocol.io)) **look at and drive a
running [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)
application**: see what is on screen, click, type, press keys, dispatch named
actions, take screenshots, wait for something to happen, and read an
app-defined state snapshot. The agent checks a GUI change the way a person
would — by using the app — instead of guessing from the code or blind-firing
`SendKeys`.

What it is built around: **a tool call costs the agent a model turn, seconds
of it, while the socket underneath costs about a millisecond.** So the tools
are shaped to spend as few turns as possible. `ui_snapshot` reads a window in
a fraction of the tree's size, `wait_for` waits inside the app instead of
having the agent look again and again, `batch` puts a whole interaction in one
call, and an input does not answer until the frame showing its effect has been
painted — so "did that work?" needs no second call.

Works on Linux, macOS and Windows.

## Where things are documented

| for | read |
|---|---|
| using it | this file |
| the agent, at runtime | the `gpui_guide` tool — the server documents itself, see [Getting an agent up to speed](#getting-an-agent-up-to-speed) |
| why it is built this way | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — the measurements, the frame contract, the decisions and what would break if one were undone |
| what is coming | [PLAN.md](PLAN.md) |
| putting it in an app | the *MCP Inspector* page in [gpui-component](https://github.com/stefan-siebert/gpui-component)'s docs |

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
   gpui_component::mcp       ← the in-app side: a listener thread hands each
                               request to the GPUI main thread, which answers
                               from gpui's inspector data — after waiting for
                               the frame that shows what an input changed
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
| `GPUI_MCP_RECORD` | additionally: write every successful tool call to this script file — see [Recording and replay](#recording-and-replay) |
| `GPUI_MCP_UPDATE_GOLDENS` | `1` (or `true`) accepts what the window looks like now instead of failing against the stored golden — a decision for a whole run, after looking at what changed. Any other value, `false` included, leaves comparison on |

Discovery scans the OS temp directory for `gpui-mcp-*.sock`, probes each, and
deletes the ones nothing listens on (left behind by a crashed app).

## The tools

| tool | what it does |
|---|---|
| `gpui_guide` | this server's own documentation — the three-step start, worked examples, how ids resolve, the traps. Answered by the server, so it works before the app runs |
| `get_windows` | open windows with id, title, bounds, content size, active flag — the window ids the other tools take. `bounds` is the outer frame; `content_size` is what layout sees and what `set_viewport` sets |
| `ui_snapshot` | the window as one short line per meaningful element — `role "name" #test-id @ref` — with the layout scaffolding dropped. Start here: on a real UI it is a fraction of the tree's size |
| `get_app_state` | window overview plus whatever the app's state provider returns (`app` key) |
| `a11y_audit` | controls nothing can name, ids that name several elements, targets under 24px — the problems that hurt a screen-reader user and a script equally |
| `a11y_tree` | the accessibility tree GPUI hands a screen reader: real roles, the label a control announces, an input value, the actions a node offers. Only annotated elements appear, so it complements `ui_snapshot` rather than replacing it |
| `inspect_ui_tree` | the element hierarchy: id, type (from the source file), bounds, `source_location`, children, text. Filters: `max_depth`, `window_id`, `root_element_id`, `element_type_filter`, `text_filter`, `format: compact` |
| `get_element` | one element with its full subtree |
| `get_focus_info` | the focus handle and the active key-context chain — the first thing to check when a key binding does not fire |
| `list_actions` | the app's gpui actions; `include_bindings` adds key bindings and docs, `only_available` keeps those whose context predicate matches the current focus |
| `execute_action` | dispatch a named action through the focus chain, as a keystroke would |
| `send_key` | one keystroke in gpui's notation (`enter`, `pagedown`, `f5`; modifiers as flags) |
| `type_text` | a string, one keystroke per character |
| `click_element` | left/right/middle click at an element's centre or at window pixel coordinates |
| `wait_for` | wait inside the app, one check per painted frame, until an element, a text, a key context or an app-state value is there — or, with `absent`, gone |
| `batch` | several tools in one call, in order, with one state snapshot at the end — the way to spend one turn instead of four |
| `take_screenshot` | the window (or one element, cropped) rendered to PNG, downscaled to 1400px wide unless `max_width` says otherwise |
| `get_logs` | the in-app log buffer (≤500 lines) |
| `replay_script` | replay a recorded session — to reach a state (`seek`), or as a test |
| `set_viewport` | resize a window to an exact content size, and answer after the frame that shows it. Recorded scripts carry a viewport and replay applies it first |
| `reset_app` | put the app back into a known starting state via the hook it registered. Fails loudly when there is none |
| `expect_screenshot` | compare the window against a stored golden image. Answered by the server, so the goldens live beside the script |

Every input tool (`send_key`, `type_text`, `click_element`, `execute_action`,
and `reset_app`, which is input by another name) waits for the frame that shows
what it changed, then appends the app state and focus info from *that* frame,
so the answer describes the app after the input rather than the app it
replaced. `set_viewport` waits for that frame too, and answers with the size
actually reached. `settled: false` in an answer means no frame
was painted while it waited — a minimised or occluded window — and everything
in that answer describes an older frame.

What this cannot cover is work the app starts on its own: an async load, a
debounce, an animation. That is what `wait_for` is for, and it waits inside the
app rather than costing the agent a call per look.

**Element ids** come in four forms, all accepted wherever an id is taken: a
`@ref` from the last `ui_snapshot`, the full id
(`WindowId(1)/view-1.panel[0]`), the global id (`view-1.panel`), or a suffix
(`panel`) — the first match wins. An id copied out of `format: "compact"`
output works too, shortened crate paths and instance suffix and all, as does
the `#panel` the snapshot prints. Give the elements you want to target stable
ids in the app (`div().id("results")`): a lowercase, dashed id is what the
snapshot prints as `#results`.

### The snapshot

```
ui_snapshot 4 — WindowId(1v1), 9 of 96 painted elements
- button #github @e1 ✓
- button "Edit" #menu @e2 ✓
- textbox #input-4294967299 @e9 value="" ✓
```

An element earns a line by having a role, an id somebody chose, or text;
everything else is dropped and its children take its place. The role comes
from the file that rendered the element — for gpui-component's own widgets the
file name *is* the role, so `button/button.rs` renders a `button` and an app
gets that vocabulary without annotating anything. A role that describes a
region (`banner`, `list`, `dialog`, …) is only used when the element actually
contains something, because one file paints both a title bar and its close
button.

**A `✓` means the line came from the accessibility tree**, not from that
guess. Where an element has a node, its declared role wins over the derived
one, its label supplies a name when nothing is painted (which is how `#menu`
above got `"Edit"`), and its state is appended: `checked`, `selected`,
`expanded`, `value="…"`. The marker is there because a role a widget declared
and a role inferred from a file name are not the same claim, and an agent
deciding what to trust should be able to see which it got. On the
gpui-component gallery 10 of 91 lines carry it — most of a normal UI is not
annotated, and the derived layer is what still sees the rest.

Because of that, `ui_snapshot` and `a11y_audit` switch the window into
building its accessibility tree and wait one frame the first time they run
against it. Otherwise what they reported would depend on whether something
else had switched it on first.

Each `@ref` is shorthand for "the thing on that line of the snapshot I just
showed you", and the next snapshot replaces the whole set — a stale ref fails
with a message saying to take a new one rather than resolving to whatever now
sits on that line.

## Getting an agent up to speed

The socket round trip costs about a millisecond; the model turn wrapped around
it costs seconds. So the expensive mistake is an agent that learns this
server's shape by trial and error. It is told instead, on four surfaces, all
generated from one table in `src/docs.rs`:

- **`initialize` instructions** — a short orientation the client keeps in
  context for the whole session: the three-step start, the four element-id
  forms, what an input answer already tells it, and where the rest is.
- **the `gpui_guide` tool** — the long form, one topic per call:
  `overview`, `tools`, `recipes`, `ids`, `focus`, `screenshots`,
  `troubleshooting`. The server answers it itself, without touching the
  socket, so reading the guide works before the app is started — which is when
  it is most likely to be read.
- **resources** — the same topics as `gpui://guide/<topic>`, for clients that
  attach resources rather than call tools.
- **the `onboard` prompt** — overview, tools and recipes in one message.
  Claude Code surfaces it as `/gpui-inspector:onboard`.

A test asserts that every method in `methods::ALL` appears in the `tools`
topic, so a new tool cannot ship undocumented.

## The accessibility audit

`a11y_audit` reads the same derived layer the snapshot prints and reports the
problems it can actually see:

| check | severity | what it means |
|---|---|---|
| `unnamed-control` | serious | an interactive element that paints no text. A screen reader has nothing to announce, and nothing can target it by name |
| `duplicate-id` | serious on a control, else warning | one id names several elements. A suffix match takes the first, so a click or a recorded script may act on the wrong one |
| `target-too-small` | warning | an interactive element with a side under `min_target_size`, 24px by default (WCAG 2.2) |
| `zero-size-control` | serious | an interactive element painted with no area at all |
| `unstable-id` | warning | an id ending in a number the app generates fresh on every start, like `#input-4294967299`. It reads like a name and is not one: anything written down against it matches nothing after a restart |

A first run against a real desktop UI — the gpui-component gallery, an
unmodified upstream demo used here as a test subject — reported nine unnamed
controls (every icon-only button in the title bar, plus the search field),
`#menu` naming four buttons and `#item` naming sixty-two list rows. That is
what most UIs look like before anyone has had reason to name things, not a mark
against that one.

The `#item` finding is why this is not only an accessibility feature: a recorded
script targeting `#item` clicks the first of sixty-two, quietly, and only in
the run where the order changed. **The same fix serves both readers** — give
the element its own id and a label, and it becomes both announceable and
targetable.

Findings are ordered worst first and carry the element, its id, and the source
location gpui recorded. Note what that location is: for a gpui-component widget
it is the widget's own file, so it says *what* the element is rather than where
your app put it. The id and the element path are what locate it in your code.

Contrast is not checked, and cannot be: colours never reach this side.

The audit reads the accessibility tree wherever it reaches, which changes two
things. `unnamed-control` now says *which* fix applies — an element with a
node needs an `.aria_label(...)`, one without needs a `.role(...)` first, and
telling the second to add a label is advice that cannot work. And the answer
carries `announced` beside `checked` (10 of 91 on the gallery): how much of
the window a screen reader can see at all. That is a count rather than a
finding per element on purpose — 81 findings saying "no node" would bury the
handful that name a real defect, and a number cannot be tuned out.

As a step in a recorded script, a failing audit fails the replay — which is how
this stays checked instead of having been checked once:

```json
{ "method": "a11y_audit", "params": { "fail_on": "serious" } }
```

## The accessibility tree

The audit above reads a *derived* layer: roles guessed from the file that
rendered an element, names taken from the text it painted. GPUI also builds a
real accessibility tree — the one AccessKit hands a screen reader — and
`a11y_tree` returns it.

It knows things the derived layer cannot:

```
Button  label "Edit"                         # paints an icon, announces a word
TextInput  value ""  actions Focus, SetValue # what the field currently holds
MenuBar                                      # a role nobody had to guess
```

Those four buttons the audit reports as `#menu` naming four elements are
`GPUI Component`, `Edit`, `Window` and `Help` in this tree. The derived layer
had nothing to tell them apart, because none of them paints text.

The catch is coverage. GPUI builds a node only for elements somebody
annotated, and against the gpui-component gallery that is **11 nodes over 96
painted elements** — the sidebar's sixty-two rows have none. So the tree is
not a smaller snapshot, it is a different, sparser view, and the answer says
`nodes` against `painted` so that is visible rather than assumed.

Most of the time you do not need this tool: `ui_snapshot` already folds the
tree into its lines, marking them `✓` and appending state. Reach for
`a11y_tree` when you want the full node — description, keyboard shortcut,
position-in-set, the exact AccessKit role — or when you want to see the
structure a screen reader walks rather than the one the elements form. The two
line up exactly: gpui derives a node's id from the same `GlobalElementId` the
element path comes from, so the join is by identity, not by geometry or name.

GPUI builds the tree only while assistive technology is attached, which is
right for a shipping app and useless for checking one. Reading it therefore
switches the window into building it and waits a frame — which is why the
first `a11y_tree`, `ui_snapshot` or `a11y_audit` against a window costs one
frame more than the ones after it. This needs
`Window::set_a11y_force_active`, which lives in the gpui fork this project
builds against.

## Recording and replay

Start the server with `GPUI_MCP_RECORD` set and every successful tool call is
written to that file as a script:

```sh
claude mcp add --transport stdio gpui-inspector \
  --env GPUI_MCP_APP=my-app --env GPUI_MCP_RECORD=tests/open-file.json \
  -- /abs/path/to/gpui-mcp-server
```

```json
{
  "name": "open-file",
  "app": "my-app",
  "steps": [
    { "method": "click_element", "params": { "element_id": "open-file" } },
    { "method": "type_text", "params": { "text": "src/main.rs" } },
    { "method": "send_key", "params": { "key": "enter" } },
    { "method": "wait_for", "params": { "text": "main.rs", "timeout_ms": 5000 } }
  ]
}
```

Steps are the same `{method, params}` shape `batch` takes, so a script can be
written by hand as easily as recorded. Failed calls are not recorded — a script
of things that did not work replays nothing — and the file is rewritten after
each step, so an interrupted session still leaves valid JSON.

**A `@ref` is rewritten into the id the snapshot printed beside it**, because a
ref means "line seven of what I am looking at" and that is meaningless in a
file. Where there was no id, or an id like `#item` that appeared on several
lines, the ref is kept and the step carries a `note` saying it will only replay
if the preceding snapshot produces the same lines. The fix is in the app: give
that element its own id.

The recorder also writes a `note` when the id going into the file will not
hold — an id the last snapshot printed on several lines, or one ending in a
generated number (`#input-4294967299`). The audit reports both of these too,
but it reports them about the app, later, if anyone runs it. The note reports
them about *this step*, while the person recording it can still pick a
different element or go and name that one.

Replaying the file does two different jobs:

```sh
gpui-mcp-server replay tests/open-file.json          # as a test, exits non-zero on failure
gpui-mcp-server replay tests/open-file.json --seek   # just get back to that state
```

The agent can do the same through the `replay_script` tool. There is no
assertion step and there does not need to be one: a `wait_for` that comes back
unsatisfied **is** a failed assertion, and it already reports which condition
did not hold.

```
  0  applied  viewport 1280x800
  1  passed   click_element
  2  failed   wait_for — waited 310 ms and the condition never held: {"text":{"found":false,"query":"Saved"}}

open-file: 1 passed, 1 failed, 0 skipped, of 4
```

Line 0 is the script's viewport header, applied before the first step. When it
cannot be — no window, or a window that refused the size — the run stops
there, every step reads `skipped`, and line 0 says why.
Which makes the CLI form the interesting one: the file an agent produced by
exploring runs in CI afterwards, with no agent and no model cost.

### Making a replay mean the same thing twice

A test that passes for the wrong reason is worse than no test, and a replay has
three ways to drift.

**The window size.** It decides layout: a sidebar collapses, a toolbar folds
into a menu, and the element a step wanted is somewhere else or nowhere. So a
recorded script carries the size it was made at, and replay applies it before
the first step:

```json
{ "name": "open-file", "viewport": { "width": 1280, "height": 800 }, "steps": [] }
```

The recorder asks the app for it once, so this is filled in whether or not the
session ever looked at a window. It is the *content* size — what layout sees —
not the outer frame, which on macOS includes the title bar. A window that
cannot be resized, or that comes back a different size than it was asked for
(`honoured: false` — a platform minimum, a maximised window), aborts the replay
rather than producing a run of failures that all describe the wrong problem.

**The starting state.** Nothing here can make an app left on the third tab with
two files open behave like one that just started — only the app can. Register
what a known starting state is, the same way an app registers its state
provider, and `reset_app` becomes a step a script can take:

```rust
gpui_component::mcp::mcp_set_reset_hook(|_arguments, cx| {
    // put the app back where a script expects to find it
    Ok(())
});
```

Without a hook, `reset_app` fails and says so. That is deliberate: a replay
which believes it started from a known state and did not is a green run hiding
a bug.

**What it looks like.** `expect_screenshot` compares the window against a
stored image:

```json
{ "method": "expect_screenshot", "params": { "path": "golden/sidebar.png" } }
```

In a script the path is relative to the script file, so the goldens travel
with it and resolve the same from whichever directory the replay is started.
The recorder writes it that way: an agent names the golden relative to
wherever the MCP client started the server, and that directory is written down
nowhere. (Called directly as a tool, the path is relative to the server's
working directory.)

The first run writes the golden and says there was nothing to compare against —
look at it before trusting the next run. The step passes, and the replay
report and the CLI line carry that message rather than a bare `passed`. Later
runs compare, and a failure writes this run beside the golden as
`<name>.actual.png` so both can be opened. Two images match when they are the
same size and at most `pixel_tolerance` of pixels (default 0.1%) differ by
more than `channel_tolerance` per channel (default 8). That is not a
perceptual metric and is not claimed to be — it is enough to absorb the level
or two that text rendering moves between runs, and not enough to miss a
layout change.

A size mismatch is reported on its own, with nothing counted as differing, and
names its likely cause: the window was not pinned, or — when both sides differ
by the same factor — the display has a different scale factor than the one
the golden was taken on. A golden is in device pixels, so a 1280x800 window is
a 1920x1200 image at 150%.

`GPUI_MCP_UPDATE_GOLDENS=1` rewrites goldens instead of failing. It is an
environment variable rather than a step parameter on purpose — a script that
could update its own golden would never fail. Only `1` (or `true`) switches it
on: a CI file that exports a YAML `false` as the string `"false"` must not.

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
- *"gpui-mcp protocol mismatch"* — the app and this server were built from one
  crate at different times and have drifted. The message names which half to
  rebuild. Protocol v2 (frame-synchronous input answers, `wait_for`, `batch`)
  needs both halves rebuilt: `cargo build --release` here, and a normal build
  of the app.
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
cargo test                 # protocol serde defaults, socket names, the guide surface
```

CI (`.github/workflows/ci.yml`) runs the same three on Linux and Windows.

This repo is deliberately small: the binary in `src/main.rs`, the wire types in
`src/protocol.rs`, and what the server tells an agent about itself in
`src/docs.rs`. The in-app logic — main-thread dispatch, waiting for frames, how
gpui's flat inspector list becomes a tree and then a snapshot, rendering
screenshots — lives in `gpui_component::mcp`.

Before changing behaviour here, read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
Several things that look like obvious simplifications are load-bearing: why
settling waits for *two* frame callbacks, why refs are replaced whole, why a
region role has to contain something, and when `PROTOCOL_VERSION` is bumped.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
