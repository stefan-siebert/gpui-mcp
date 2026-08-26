# Plan: make driving a GPUI app cheap for an agent

Working document. The goal is not a faster socket — it is fewer agent turns,
smaller payloads, and artefacts that double as tests.

## State of play

Every stage is shipped and verified against a running app. Stage 2 did not land
the way this plan expected: the annotation layer 2b was going to build arrived
in upstream gpui instead, so what shipped is a reader for it rather than a
design of our own.

| stage | what | state |
|---|---|---|
| 0 | the server documents itself to the agent | done |
| 1 | frame-synchronous answers, `wait_for`, `batch` | done, protocol v2 |
| 2a | `ui_snapshot`, roles derived from the source file, refs | done |
| 2b | annotations: real roles, labels, values, state | done via upstream AccessKit + `a11y_tree` |
| 2c | folding the a11y tree into the snapshot and the audit | done |
| 3a | recording a session, replaying it, the `replay` CLI | done |
| 3b | pinned window size, golden screenshots, an app-side reset hook | done |
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

### What 2c settled, and what the evidence said

Whether the a11y tree replaces the derived layer or overlays it. Run against
the gallery, the answer was legible, and one part of it was wrong in this
plan's first reading:

- **11 nodes over 96 painted elements.** The sidebar's sixty-two rows have no
  node at all. The tree cannot be the spine; it is the sparser view. So: an
  overlay.
- **It knows what the derived layer cannot.** The four buttons `duplicate-id`
  reports as `#menu` are `GPUI Component`, `Edit`, `Window` and `Help` in the
  tree — none of them paints text, so 2a had nothing to tell them apart. The
  search field carries `role: TextInput`, `value: ""`,
  `on_action: [Focus, SetValue]`.
- **The join was *not* free, and the fix was small.** This plan first recorded
  that nodes carry `element_id` and `source_location` and that no geometry
  matching would be needed. Both facts are true and neither is enough: the
  four `#menu` buttons share *both*, so matching on them picks one of four at
  random — the exact class of bug `duplicate-id` exists to report. What makes
  it exact is that gpui derives a node's id from the element's
  `GlobalElementId`, so a second fork line puts that id on
  `InspectorElementInfo` and the join is by identity. Verified by set
  intersection against the gallery: all ten nodes match, none ambiguous.
- **`NamedInteger("input", 4294967299)`** is `unstable-id` seen from the other
  side: gpui itself prints the entity number as a separate field.

**What shipped.** Where an element has a node, its declared role wins over the
derived one, its label supplies a name when nothing is painted, and its state
is appended — `checked`, `unchecked`, `selected`, `expanded`, `value="…"`. The
line ends in `✓`, because a role a widget declared and a role inferred from a
file name are not the same claim and an agent should be able to see which it
got. An AccessKit role with no entry in the mapping table leaves the derived
role alone rather than introducing a second vocabulary: `filter`,
`interactive_only` and the audit all match on those strings.

`ui_snapshot` and `a11y_audit` now switch the window into building the tree
and wait a frame the first time, which makes them asynchronous. That cost buys
the alternative away: otherwise what the audit reported would depend on
whether something else had called `a11y_tree` first.

**The question about a missing node, answered by measuring.** A per-element
finding would have fired 81 times on the gallery. Worse, it would have fired
*nowhere useful*: every one of the nine interactive elements already has a
node, and an app's own clickable `div` has no derived role either, so the
check could not see it. So there is no such finding. The audit reports
`announced` beside `checked` instead, and `unnamed-control` splits into two
messages — an element with a node needs a label, one without needs a role
first, and telling the second to add a label is advice that cannot work.

**Still not shipped:** contrast (colours never reach this side, and never
will). `disabled` is not among the fields gpui writes to a node, so despite
what stage 2b's original sketch promised, it is not available from here.

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

### 2c — fold the tree into the snapshot and the audit — **done**

`ui_snapshot` keeps the spine and takes role, name and state from the node
where one exists, falling back to the derived guess where none does. A line
backed by a node ends in `✓`. `a11y_audit` reads the same overlay: it reports
`announced` beside `checked`, and `unnamed-control` names the fix that
actually applies.

The join is `InspectorElementInfo::accesskit_node_id`, which gpui derives from
the element's `GlobalElementId` — not the node's own leaf element id and
source location, which the gallery's four title-bar buttons share. See *What
2c settled* above for the measurements, including why a "no accessibility
node" finding was measured and then not built.

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

### 3b — making a replay mean the same thing twice — **done**

A test that passes for the wrong reason is worse than no test. A replay had
three ways to drift, and each now has a fix.

- **The window size.** `set_viewport` resizes to an exact content size and
  answers after the frame that shows it; a recorded script carries a
  `viewport` header and replay applies it before the first step. The recorder
  asks the app for the size once, so the header is there whether or not the
  session ever looked at a window. A window that cannot be resized — or that
  the platform left at another size, `honoured: false` — aborts the replay
  instead of producing a run of failures that all describe the wrong problem.
  `Window::resize` was already public in the fork — no patch needed. The
  size recorded and measured is the content size (`get_windows` now reports
  `content_size` beside `bounds`), because on macOS the outer bounds include
  the title bar.
- **The starting state.** `mcp_set_reset_hook` in gpui-component, mirroring
  `mcp_set_app_state_provider`, and `reset_app` to call it. Without a hook the
  method fails and says what to register. That is deliberate: a replay which
  believes it started from a known state and did not is a green run hiding a
  bug, and a silent no-op would produce exactly that.
- **What it looks like.** `expect_screenshot`, answered by the server so the
  goldens live beside the script — literally: in a script the golden path is
  relative to the script file, written that way by the recorder and resolved
  that way by replay. First run writes the golden and says there was nothing
  to compare against, and the CLI prints that rather than a bare `passed`;
  later runs compare and a failure writes this run beside it as
  `<name>.actual.png`. `GPUI_MCP_UPDATE_GOLDENS=1` accepts a change — an
  environment variable rather than a step parameter, because a script that
  could update its own golden would never fail; only `1`/`true` count, so a
  CI `false` does not switch it on.

**On "perceptual tolerance", which this plan asked for and did not get.** Two
images match when they are the same size and at most `pixel_tolerance` of
pixels (default 0.1%) differ by more than `channel_tolerance` per channel
(default 8). That is not a perceptual metric in the CIE sense and is not
described as one anywhere. It is enough to absorb the level or two that text
rendering moves between runs on the same machine — a comparison that called
those a failure would fail every time and teach everyone to ignore it — and
not enough to miss a layout change. A size mismatch is reported on its own,
with nothing counted as differing, and names its cause: an unpinned window,
or a display with a different scale factor — a golden is in device pixels.

This adds one dependency to the server: `image`, PNG only, no encoders. The
earlier decision not to take `image` for JPEG screenshots still holds and is a
different question — that bought nothing, since an image costs tokens by its
dimensions. Comparing pixels cannot be done without decoding them.

Verified end to end: pin 1280x800, write a golden, match it at 0 differing
pixels, resize to 1000x700 and watch the size mismatch name its own cause;
then record at 1000x700, resize to 1400x900, replay, and watch the header put
the window back so the golden matches again. Without the header the same
replay fails, which is what makes the header load-bearing rather than
decorative. The CLI exits 1 on the failing run and 0 on the passing one.

Verified again after the review round (2026-08-26), against the story app on
Windows: a session recorded from one directory writes the golden path relative
to the script (`..\recording-cwd\golden\main.png`), and a replay started from
a different directory finds it and compares at 0 differing pixels. A header of
40x30 is clamped by the platform to 480x320 and stops the run as line 0,
`failed`, with every step skipped and the counts still adding up.
`GPUI_MCP_UPDATE_GOLDENS=false` leaves comparison on. A hand-written script
resolves its golden beside itself and reports the created golden as `passed`
with the message, and a `replay_script` step is refused with a reason. And one
finding about the app rather than the tooling: shrinking the story window to
1000x700 and letting the header grow it back to 1280x800 leaves the sidebar
some forty pixels wider than before — the resizable panel keeps the width the
shrink gave it — so the golden fails at 9.4% differing. That is a real drift
of the starting state, which is what `reset_app` exists for, and the golden
comparison catching it is the point.

Still not built, and still worth having: recording input a *person* performs by
hand — the server only sees what passes through it, so that needs the app side.

### The original sketch, for the parts still not built

Record **semantically**, never as coordinates: at record time each real user
event (hand-driven too, not just MCP-driven) is resolved against the registry
and stored as role + name + `test_id`; anything with a keybinding is preferred
as an `action:` step.

A CLI mode that starts the app itself — `gpui-mcp-server run tests/*.json
--app elane` — and writes JUnit/TAP, so a CI job is one command rather than a
script that launches the app and then replays against it.

## Stage 4 — `a11y_audit` — **done, minus what cannot be seen**

Shipped: `unnamed-control`, `duplicate-id`, `target-too-small`,
`zero-size-control` and `unstable-id`, ordered worst first, each naming the
element, its id and gpui's source location. A failing audit fails a replay
step, so accessibility is part of a regression run rather than a thing checked
once.

State arrived with 2c: the snapshot the audit reads now carries `checked`,
`selected`, `expanded` and an input's value wherever an element has an
accessibility node, and the audit reports how many do. Contrast is still not
checked and never will be — colours never reach this side. Keyboard
reachability and focus traps are still open: gpui maps a node to a
`FocusHandle`, but nothing here reads that mapping yet.

First run against the gpui-component gallery (upstream's demo, borrowed as a
test subject): nine unnamed controls, `#menu` naming four buttons, `#item`
naming sixty-two list rows. Evidence that the checks fire on a real UI — not a
list of things anyone here owns.

## Order

Stage 0 first (self-contained, ships immediately), then stage 1 with frame
synchronisation at the front — little code, but it touches every single
interaction. Stage 2 is the larger piece and pays for itself three times over:
addressing, small payloads, assertions. Stage 3 builds on it.
