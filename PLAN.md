# Plan: make driving a GPUI app cheap for an agent

Working document. The goal is not a faster socket — it is fewer agent turns,
smaller payloads, and artefacts that double as tests.

## State of play

Stages 0, 1, 3a and 4 are shipped and verified against a running app. Stage 2
is done on both halves, but not the way this plan expected: the annotation
layer 2b was going to build arrived in upstream gpui instead, so what shipped
is a reader for it rather than a design of our own.

| stage | what | state |
|---|---|---|
| 0 | the server documents itself to the agent | done |
| 1 | frame-synchronous answers, `wait_for`, `batch` | done, protocol v2 |
| 2a | `ui_snapshot`, roles derived from the source file, refs | done |
| 2b | annotations: real roles, labels, values, state | done via upstream AccessKit + `a11y_tree` |
| 2c | folding the a11y tree into the snapshot and the audit | open, and now decidable |
| 3a | recording a session, replaying it, the `replay` CLI | done |
| 3b | pinned window size, golden screenshots, an app-side reset hook | open |
| 4 | `a11y_audit`, and a failing audit failing a replay | done, minus what the derived layer cannot see |

### What happened to 2b

This plan said a generic `.a11y(...)` wrapper cannot work, that the annotation
has to ride on the element registering the hitbox, that this means fields on
`Interactivity` in the zed fork, and that the cost was a heavier rebase — a
call to make before 2b started.

That call no longer exists. Upstream zed built the whole thing, and it reached
the fork through the 2026-08-04 merge without a line of our own:

- `accesskit` in `crates/gpui/Cargo.toml` and a guide in `_accessibility.rs`,
- `Element::a11y_role()`, and about twenty `aria_*` builders on
  `Interactivity` — label, description, selected, expanded, toggled, value,
  placeholder, orientation, level, position-in-set, row/column index and count,
- action listeners and a focus-handle-to-node mapping,
- `Window::debug_a11y_tree_json()`, which dumps the tree with each node's
  `element_id` and `source_location` attached.

gpui-component already fills it in: Button, ToggleButton, Checkbox, Radio,
Switch, Tab, List, MenuItem and PopupMenu annotate themselves. That is very
nearly the widget list this plan named as 2b's work.

So everything 2b listed as blocked — `checked`/`selected`/`disabled`, an
input's value, roles for app-owned widgets, per-element focus — was already
there, behind one gate: GPUI builds the tree only while assistive technology is
attached (`window.rs`, `if self.a11y.is_active()`), and the flag starts false
and is set only by AccessKit's own callbacks.

**What shipped instead of 2b:**

- `Window::set_a11y_force_active(bool)` in the fork — a `force_enabled` flag
  ORed into `sync_active_flag`, so `Application::new_inaccessible` still wins
  and nothing changes for an app that never asks. Recorded in `FORK_CHANGES.md`.
  Effective from the next frame, because the frame being painted latched its
  answer before the first node was pushed.
- `a11y_tree`, answered asynchronously: it switches the window into building
  the tree, waits a frame, and returns it with `nodes` against `painted`.

### The decision 2c is waiting on — with the evidence

Whether the a11y tree replaces the derived layer or overlays it. Run against
the gallery, the answer is legible:

- **11 nodes over 96 painted elements.** The sidebar's sixty-two rows have no
  node at all. The tree cannot be the spine; it is the sparser view.
- **It knows what the derived layer cannot.** The four buttons `duplicate-id`
  reports as `#menu` are `GPUI Component`, `Edit`, `Window` and `Help` in the
  tree — none of them paints text, so 2a had nothing to tell them apart. The
  search field carries `role: TextInput`, `value: ""`,
  `on_action: [Focus, SetValue]`.
- **The join is free.** Nodes carry `element_id` (`Name("menu")`,
  `NamedInteger("input", 4294967299)`) and `source_location`. No geometry
  matching is needed — the worry this plan recorded was unfounded.
- **`NamedInteger("input", 4294967299)`** is `unstable-id` seen from the other
  side: gpui itself prints the entity number as a separate field.

So 2c is an overlay, not a replacement: `ui_snapshot` keeps the spine and takes
role, name, value and state from the node where one exists; `a11y_audit` gains
the checks that were listed as unshippable — state, and a control that is
focusable with no node (which gpui already logs about). Contrast stays out:
colours never reach this side.

Worth deciding with it: whether a missing node becomes an audit finding. Only
11 of 96 elements have one, so the check would fire on most of a normal UI —
the same shape of finding as `unnamed-control`, and the same risk of being
tuned out.

### Loose ends — closed

Both were the same problem: an id that will not mean tomorrow what it means
today.

- **`unstable-id`**, a fifth audit check. `#input-4294967299` carries an entity
  number that is fresh on every app start, and it is the one bad id the derived
  layer cannot tell apart from a good one — lowercase, dashed, every bit as
  deliberate-looking as `#save-button`. The line is drawn at six trailing
  digits, so `item-3`, `row-42` and `since-2024` survive. Reported once per id
  rather than once per element: sixty rows sharing a generated id are one
  problem with one fix. A warning, not serious — nothing is broken for anyone
  using the app right now; what breaks is everything written down against it.
- **The recorder says so at record time.** It already warned when a `@ref`
  pointed at a repeated id. It now also warns when a step *names* an id itself
  that the last snapshot printed on several lines — the gap, since such a step
  never went through the ref machinery — and when the id going into the file
  ends in a generated number. The audit reports both about the app, later, if
  anyone runs it. The note reports them about *this step*, while whoever is
  recording it can still click something else or go and name it.

The heuristic lives in `protocol.rs` (`id_looks_generated`, `GENERATED_ID_DIGITS`)
so both halves agree on what "generated" means instead of guessing it the same
way twice.

### Verifying a change by hand

There is no app in this repo, so use gpui-component's gallery:

```sh
cd ../gpui-component && cargo run -p gpui-component-story --features mcp
GPUI_MCP_APP=story ./target/release/gpui-mcp-server replay <script.json>
```

Both halves must be rebuilt together after a protocol change; the version
handshake will say so if they are not.

There is a third checkout: `../gpui-fork`, the zed fork, which gpui-component
patches in by path. `a11y_tree` depends on a change there
(`Window::set_a11y_force_active`), so a build of the gallery is also the test
of that patch. The fork's own workspace does not build on Windows — an
unrelated dependency exceeds the path limit — so type-check gpui through
gpui-component rather than with `cargo check -p gpui` in the fork. Anything
added there belongs in the fork's `FORK_CHANGES.md`, which is what makes the
next upstream rebase survivable.

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
get built together. Two tiers.

### 2a — derived, no annotation — **done**

The role was already in the data: an element's `source_location` names the
file that rendered it, and for gpui-component's own widgets the file name *is*
the role. `button/button.rs` renders a button. So every app on this crate gets
a semantic vocabulary for nothing, and stage 2's payoff arrived without a
single annotation.

`ui_snapshot` prints one line per element that means something —
`role "name" #test-id @ref` — and drops the layout scaffolding, lifting its
children into its place. Measured on the story app: **3.4 KB against 151 KB**
for the full tree and 63 KB for `format: "compact"` at depth 3;
`interactive_only` answers "what can I click here" in 332 bytes. The server
hands it back as text rather than JSON, since escaping the indentation would
double it for nothing.

Refs (`@e7`) are handed out per snapshot and the next snapshot replaces the
whole set, so a stale one fails loudly instead of resolving to whatever now
sits on that line. They work as `element_id` everywhere, including for a
snapshot taken earlier in the same `batch`.

Two rules keep the derivation honest: a region role (`banner`, `list`,
`dialog`, …) is only used when the element contains something, because one
file paints both a title bar and its close button; and an id segment counts as
a `test_id` only when somebody clearly wrote it — lowercase, dashes or
underscores — never `view-4294967734`, `1-0-0` or a type name.

### 2b — annotated, for what cannot be derived — **done, by upstream**

A file name cannot say `checked`, `selected` or `disabled`, cannot say what an
input holds, and gives an app's own widgets no role at all. This plan proposed
building that annotation ourselves. Upstream zed built it first — see *What
happened to 2b* above for what arrived and what it cost.

What the tree gives, read off the gallery: real roles (`Button`, `MenuBar`,
`TextInput`) rather than roles inferred from a file name; the label a control
announces when it paints only an icon; an input's `value`; the actions a node
offers (`Click`, `Focus`, `SetValue`); and, per node, the `element_id` and
`source_location` that tie it back to a snapshot line.

What it does not give is coverage: a node exists only where somebody
annotated the element, which on the gallery is 11 of 96 painted elements. The
derived layer of 2a is therefore not superseded — it is what still sees the
other 85. Folding the two together is 2c.

### 2c — fold the tree into the snapshot and the audit — open

`ui_snapshot` keeps the spine and takes role, name, value and state from a
node where one exists, falling back to the derived guess where none does.
`a11y_audit` gains state-aware checks and can report a focusable control with
no node. The join key is `element_id` plus `source_location`; no geometry
matching is needed.

Two things to settle when it is built: whether a missing node is a finding at
all — it would fire on most of a normal UI — and whether a snapshot line
should say where its facts came from, since a derived role and an announced
one are not the same claim.

## Stage 3 — Record and replay — **3a done**

Shipped, in the server only — no app change and no wire change:

- `GPUI_MCP_RECORD=<path>` writes every successful tool call to a script.
  Steps are the same `{method, params}` shape a `batch` takes.
- A `@ref` is rewritten into the id the snapshot printed beside it. Where there
  was none, or where the id appears on more than one line, the ref is kept and
  the step carries a note: rewriting `#item` when forty lines say `#item` would
  produce a script that replays cleanly and clicks the wrong thing.
- `replay_script { path, seek, stop_on_error }` for the agent, and
  `gpui-mcp-server replay <file> [--seek] [--keep-going]` for CI, which exits
  non-zero on failure.
- No assertion step: a `wait_for` that comes back unsatisfied is a failed
  assertion and already says which condition did not hold.

Open (3b): a pinned window size, golden-screenshot comparison with a
perceptual tolerance, and a defined starting state via an app-side reset hook.
Recording input a *person* performs by hand would need the app side too; the
server only sees what passes through it.

The original sketch below is kept for the parts not built yet.

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

## Stage 4 — `a11y_audit` — **done, minus what cannot be seen**

Shipped: `unnamed-control`, `duplicate-id`, `target-too-small`,
`zero-size-control` and `unstable-id`, ordered worst first, each naming the
element, its id and gpui's source location. A failing audit fails a replay
step, so accessibility is part of a regression run rather than a thing checked
once.

Not shipped, because the derived layer cannot see it: contrast (no colours on
this side), keyboard reachability and focus traps, and anything about state.
Contrast stays out for good. State and focus do not: the a11y tree carries
both — `selected`, `toggled`, `value`, and a node-to-focus-handle mapping — so
they arrive with 2c rather than needing a fork change of their own.

First run against the gpui-component gallery (upstream's demo, borrowed as a
test subject): nine unnamed controls, `#menu` naming four buttons, `#item`
naming sixty-two list rows. Evidence that the checks fire on a real UI — not a
list of things anyone here owns.

## Order

Stage 0 first (self-contained, ships immediately), then stage 1 with frame
synchronisation at the front — little code, but it touches every single
interaction. Stage 2 is the larger piece and pays for itself three times over:
addressing, small payloads, assertions. Stage 3 builds on it.
