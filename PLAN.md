# Plan: make driving a GPUI app cheap for an agent

Working document. The goal is not a faster socket — it is fewer agent turns,
smaller payloads, and artefacts that double as tests.

## Why: where the time actually goes

Measured against this repo's release binary and a temp dir with 488 entries:
socket discovery costs ~0.5 ms per call, a full process round trip ~1.5 ms.
Even with the 10 ms main-thread poll and a fresh connection per call, the
transport is under 20 ms. One agent tool call costs *seconds* of model time.
So the transport is not the problem. These four are:

1. **Frame staleness.** `window.inspector_elements()` reads
   `self.rendered_frame` — the last *painted* frame. `attach_post_state()`
   collects app state and focus in the same main-thread tick as
   `dispatch_click`/`dispatch_keystroke`, i.e. before GPUI lays out and paints
   again. The "post state" an input tool returns is therefore systematically
   the state *before* the input took effect. This is what produces
   "click → inspect → looks unchanged → inspect again", and every retry is a
   full model turn.
2. **One tool call = one model turn.** "Open dialog, type a path, press enter,
   check" is 4–6 turns, each carrying the whole context.
3. **Payload size.** An unfiltered element tree and a full-window base64 PNG
   crowd out the context, which makes every later turn slower and dearer.
4. **Two quadratic loops.** `build_element_tree` finds each parent by scanning
   every entry (`gpui-component/crates/ui/src/mcp.rs`, the `for j in
   0..insertion_order.len()` loop) — O(n²) with string compares and clones,
   despite the comment claiming otherwise. And `inspector_elements()` matches
   every painted text against every hitbox, O(texts × hitboxes).

## Stage 0 — Tell the agent what it needs to know (this repo only) — **done**

The cheapest win available: an agent that knows the pitfalls in advance does
not spend turns rediscovering them. No app changes, no wire changes.

- `instructions` in the `initialize` result — the short version, always in
  context: the three-step start, the element-id forms, the staleness warning,
  where to get more.
- A **`gpui_guide` tool** with a `topic` argument carrying the full examples.
  Answered by the server itself, so it works with no app running.
- The same documents as **MCP resources** (`gpui://guide/<topic>`) for clients
  that attach resources, and one **MCP prompt** (`onboard`) which surfaces in
  Claude Code as a slash command.
- An example line in every tool description.
- A test that every method in `methods::ALL` appears in the guide, so the
  documentation cannot silently drift from the tool list.

## Stage 1 — Kill the round trips — **done**

Shipped as protocol v2 (`wait_for`, `batch`, frame-synchronous input answers).
The version was bumped even though the types stayed compatible: the behaviour
did not, and a new server against an old app would otherwise promise
frame-synchronous answers the app does not give.

- **Frame synchronisation.** Every input tool answers after
  `window.on_next_frame` (possibly two frames), so `app_state`, `focus_info`
  and the tree describe the state *after* the input. Biggest single win.
- **`wait_for`** — waits in-app, once per frame, until a condition holds
  (an element is painted, a text is present, a key context is active, an
  app-state JSON pointer holds a value), with a timeout and an `absent`
  inversion. Replaces agent-side polling. The answer reports how many frames
  and milliseconds it took, and which condition was missing when it did not.
  Matching on *which element* has focus is not possible: gpui reports a
  `FocusHandle`, not an element id. That arrives with stage 2.
- **`batch`** — a list of steps in one IPC request, each frame-synchronous,
  aborting on the first failure, returning a compact per-step result plus one
  final snapshot. Turns a five-turn interaction into one.
- **Screenshots**: `max_width`, defaulting to 1400. A 4K window shot costs a
  multiple in image tokens for no added information. Shipped without the JPEG
  option this plan first listed: an image costs tokens by its pixel
  dimensions, not its file size, so the encoding buys nothing and the
  dependency was not worth adding.
- Fix the two quadratic loops; replace the 10 ms poll with a real wakeup via
  the foreground executor (also saves 100 main-loop wakeups/s in an idle app).

`build_element_tree` and the poll are done. The second quadratic loop is in
gpui itself (`inspector_elements` matches every painted text against every
hitbox) and is left for a separate change to the fork.

## Stage 2 — Names: an accessibility layer as the addressing vocabulary

Stable addressing and accessibility are the same feature, which is why they
get built together. Two tiers:

- **Durable: `test_id` + role/name.** An annotation in gpui-component
  (`.a11y(Role::Button, "Open file")`, `.test_id("open-file")`) stored in a
  per-frame side table keyed by `InspectorElementId`. gpui-component owns
  Button, Input, Checkbox, List and Tab, so those fill role, name, value and
  state (`checked`, `selected`, `disabled`, `expanded`) automatically — only
  bespoke UI needs annotating by hand. Duplicate `test_id`s within a window
  are reported as an error, which is what makes them actually unique.
- **Ephemeral: `ref`.** Each snapshot hands out short handles (`e17`) plus a
  generation number. Acting on a ref from an older generation fails with
  "stale ref, take a new snapshot" instead of quietly hitting the wrong
  element.

`ui_snapshot` then returns one semantic line per element, layout-only divs
omitted — typically 10–50× smaller than `inspect_ui_tree`, readable,
addressable and assertable. The raw tree stays for layout debugging.

## Stage 3 — Record and replay

Record **semantically**, never as coordinates: at record time each real user
event (hand-driven too, not just MCP-driven) is resolved against the registry
and stored as role + name + `test_id`; anything with a keybinding is preferred
as an `action:` step.

```yaml
name: open-file-and-search
viewport: 1280x800          # required — layout and golden screenshots depend on it
steps:
  - action: elane::OpenFile
  - wait_for: { role: dialog, name: "Open file" }
  - type: "src/main.rs"
  - key: enter
  - assert: { role: tab, name: "main.rs", state: selected }
  - audit: { fail_on: serious }
```

Two replay modes from one file: **seek** (no assertions, as fast as frames
allow) to reach the state where work happens, and **test** (assertions plus
optional golden-screenshot comparison with a perceptual tolerance).

A CLI mode of this binary — `gpui-mcp-server run tests/*.yaml --app elane` —
starts the app, replays and writes JUnit/TAP, so the same artefacts run in CI
without an agent and without model cost.

Cheap first version: the server can already write every forwarded tool call
into this script format, which makes any exploratory session a draft test with
no app changes at all.

Determinism rules to build in: never `sleep`, always `wait_for`; the window
size is fixed by the script header; a defined start state via an `app_reset`
hook analogous to the app-state provider.

## Stage 4 — `a11y_audit`

Runs over the semantic layer from stage 2 and reports: interactive elements
without an accessible name, controls unreachable by keyboard, focus order and
focus traps, contrast ratio from computed style against the background, target
size below 24 px, duplicate `test_id`s. As an `audit:` step inside a replay
script, accessibility becomes part of the regression suite instead of a
one-off.

## Order

Stage 0 first (self-contained, ships immediately), then stage 1 with frame
synchronisation at the front — little code, but it touches every single
interaction. Stage 2 is the larger piece and pays for itself three times over:
addressing, small payloads, assertions. Stage 3 builds on it.
