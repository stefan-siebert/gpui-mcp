//! Onboarding documentation the server serves about itself.
//!
//! An agent that has to discover this server's pitfalls by trial and error
//! spends a model turn per discovery, and turns are the expensive unit here —
//! the socket round trip is under two milliseconds, the turn around it is
//! seconds. So the server tells the agent up front.
//!
//! Three surfaces, all fed from the [`TOPICS`] table below:
//!
//! - [`INSTRUCTIONS`] rides along in the `initialize` result. It is always in
//!   context, so it stays short and carries only what prevents mistakes.
//! - The `gpui_guide` tool serves a whole topic on demand. It is answered
//!   here, without touching the socket, so it works with no app running.
//! - The same topics are listed as MCP resources and one MCP prompt, for
//!   clients that surface those.

/// Name of the tool that serves [`TOPICS`]. Not an IPC method: the app never
/// sees it.
pub const TOOL_NAME: &str = "gpui_guide";

/// URI scheme and prefix for the resource form of a topic.
pub const RESOURCE_PREFIX: &str = "gpui://guide/";

/// Name of the MCP prompt that hands over the starting topics in one go.
pub const PROMPT_NAME: &str = "onboard";

/// The topic served when `gpui_guide` is called without arguments.
pub const DEFAULT_TOPIC: &str = "overview";

/// Short orientation returned from `initialize`, and therefore always in the
/// agent's context. Everything here earns its place by preventing a wasted
/// turn; the long form lives in [`TOPICS`].
pub const INSTRUCTIONS: &str = "\
gpui-mcp drives a running GPUI app: read the element tree, click, type, press keys, dispatch \
named actions, take screenshots.

Start: (1) `ui_snapshot` — one short line per meaningful element, each ending in a `@ref` you can \
act on. This is the cheap way to see a window; `inspect_ui_tree` is for layout questions and is \
huge unfiltered. (2) `get_windows` when you need window ids; every tool defaults to the active \
window. (3) drive it, preferring `execute_action` over `send_key` over `click_element` — actions \
are named, stable and independent of layout.

Spend calls, not turns. `batch` runs several tools in one call and `wait_for` waits inside the app \
until a condition holds, so click, type, enter, wait is ONE call rather than four. Never poll by \
calling a read tool in a loop.

Element ids: a `@ref` from the last snapshot, or the full id, the global id, or a suffix of it \
(first match wins). All four work wherever an id is taken.

Input tools answer only once the frame showing their effect has been painted, so the state they \
return is current. Work the app starts on its own — an async load, a debounce — is not covered by \
that, and is what `wait_for` is for.

Keystroke not arriving? `get_focus_info` first, then `list_actions {\"only_available\":true}`.

A session can be recorded (`GPUI_MCP_RECORD`) and replayed with `replay_script` — the fastest way \
back to a state you were working in.

Full examples: call `gpui_guide` (topics: overview, tools, recipes, ids, focus, screenshots, \
recording, troubleshooting).";

/// One documentation topic.
pub struct Topic {
    /// Value of the `topic` argument, and the last segment of the resource URI.
    pub name: &'static str,
    /// One line, shown in the topic list and as the resource description.
    pub summary: &'static str,
    /// The document itself, markdown.
    pub body: &'static str,
}

/// Every topic, in the order they are worth reading.
pub const TOPICS: &[Topic] = &[
    Topic {
        name: "overview",
        summary: "What this server is, the three-step start, and the traps that cost turns",
        body: OVERVIEW,
    },
    Topic {
        name: "tools",
        summary: "Every tool with a minimal call and what it returns",
        body: TOOLS,
    },
    Topic {
        name: "recipes",
        summary: "Complete call sequences for the tasks that come up",
        body: RECIPES,
    },
    Topic {
        name: "ids",
        summary: "How element ids and element types work, and how to find the one you want",
        body: IDS,
    },
    Topic {
        name: "focus",
        summary: "Focus, key contexts and actions — why a keystroke does nothing",
        body: FOCUS,
    },
    Topic {
        name: "screenshots",
        summary: "Seeing the UI without drowning the context in pixels",
        body: SCREENSHOTS,
    },
    Topic {
        name: "recording",
        summary: "Recording a session and replaying it — to get back to a state, or as a test",
        body: RECORDING,
    },
    Topic {
        name: "troubleshooting",
        summary: "Errors this server produces and what each one means",
        body: TROUBLESHOOTING,
    },
];

/// Look up a topic by name. `None` for an unknown name — the caller turns
/// that into an error that lists what does exist.
pub fn topic(name: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.name == name)
}

/// Comma-separated topic names, for error messages and tool descriptions.
pub fn topic_names() -> String {
    TOPICS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
}

/// The message a bad `topic` argument gets: what was asked for, and the menu.
pub fn unknown_topic_message(asked: &str) -> String {
    let mut out = format!("Unknown guide topic '{asked}'. Available topics:\n");
    for t in TOPICS {
        out.push_str(&format!("  {:<16} {}\n", t.name, t.summary));
    }
    out
}

const OVERVIEW: &str = r#"# gpui-mcp in five minutes

You are talking to a running GPUI application through an MCP server. The
server holds no state: it re-discovers the app's socket on every call, so the
app can restart under you without breaking anything.

## The three-step start

1. `get_windows` — ids, titles, bounds, which one is active. Every other tool
   takes an optional `window_id` and falls back to the active window, so you
   usually only need this once.
2. `inspect_ui_tree {"max_depth": 3, "format": "compact"}` — the shape of the
   UI. Then drill in with `root_element_id` or `text_filter` instead of
   fetching the whole tree again.
3. Drive it. In order of preference:
   - `execute_action` — named, stable, layout-independent. Find names with
     `list_actions`. This is the closest thing to "what a user meant".
   - `send_key` / `type_text` — real keystrokes through the focus chain.
   - `click_element` — needs an element id or pixel coordinates, and breaks
     when the layout moves. Use it when there is no action and no shortcut.

## Spend calls, not turns

A tool call costs you a model turn: seconds of thinking and a slice of your
context. The socket underneath costs about a millisecond. So the thing worth
minimising is the number of calls, never the work inside one.

- **`batch`** runs several tools in one call. "Click the search box, type a
  path, press enter, wait for the result" is one call, not four.
- **`wait_for`** waits *inside* the app, checking once per painted frame until
  a condition holds. Never poll by calling `inspect_ui_tree` or
  `get_app_state` in a loop — that is one turn per look.
- Input tools already return `app_state` and `focus_info` from after the
  change, so a separate read to see what happened is usually wasted.

## Three things that will otherwise cost you turns

**Everything you read comes from the last painted frame.** Input tools take
care of this for you: `click_element`, `send_key`, `type_text` and
`execute_action` answer only once the frame showing their effect has been
painted, and the `app_state` and `focus_info` they carry describe that frame.
What they cannot cover is work the app starts on its own — an async load, a
debounce, an animation. For that, `wait_for` is the answer, not a second look.

**`inspect_ui_tree` without filters can be enormous** — tens of thousands of
tokens on a real UI. Use `ui_snapshot` for "what is on screen"; reach for the
tree when the question is about layout, and then pass at least `max_depth` or
`format: "compact"`, and prefer `text_filter` / `root_element_id` once you know
what you are looking for.

**`element_type` is a filename, not a widget class.** It is derived from the
element's `source_location`, so an element rendered from `button.rs` has
`element_type: "button"`. That makes `element_type_filter` a search over *the
app's source layout*, which is often exactly what you want — and never a
semantic role.

## What you cannot do

No mouse movement, hover, drag or scroll wheel — only clicks. No resizing or
moving windows. No text selection except through the app's own keys.
"#;

const TOOLS: &str = r#"# Every tool, with a minimal call

Inputs marked optional may be omitted entirely. Anything taking `window_id`
defaults to the active window.

## Inspection

**`get_windows`** — `{}`
Returns `[{id, title, bounds:{x,y,width,height}, is_active}]`. The `id` looks
like `"WindowId(1)"` and is what every other tool's `window_id` wants.

**`ui_snapshot`** — `{}` or `{"interactive_only": true}`
The window as a short list: one line per element that means something, layout
scaffolding dropped and its children lifted in its place. Lines read
`role "name" #test-id @ref`, indented by nesting. The role comes from the file
that rendered the element, so gpui-component's own widgets name themselves;
your app's widgets appear by their id and their text. Options: `filter`
(role/name/test-id substring, ancestors kept), `interactive_only`,
`root_element_id`, `max_elements` (default 200), `include_bounds`.

Start here. On a real UI this is a small fraction of the tree's size, and the
`@ref` at the end of each line works as `element_id` in `click_element`,
`wait_for`, `get_element` and `take_screenshot`.

**`inspect_ui_tree`** — `{"max_depth": 3, "format": "compact"}`
The element hierarchy. Options: `max_depth` (0 = unlimited), `window_id`,
`root_element_id` (start at a subtree), `element_type_filter` (substring of the
source filename), `text_filter` (substring of painted text, keeps matches and
their ancestors), `format: "full" | "compact"` (compact drops bounds,
content_mask, source_location, content_size).

**`get_element`** — `{"element_id": "results"}`
One element with its full subtree, always in full format.

**`get_focus_info`** — `{}`
`{has_focus, focus_handle, window_id, window_title, key_contexts}`. The first
call to make when a key or action does not do what you expect. Note that
`focus_handle` is gpui's handle, not an element id — there is no way to ask
which *element* has focus, only which contexts are active.

**`take_screenshot`** — `{}` or `{"element_id": "results"}`
Renders the window with GPUI itself (no screen capture, so it works on an
occluded window and needs no OS permission). With `element_id` the image is
cropped to that element's bounds. Images are downscaled to 1400px wide by
default; pass `max_width` for something else, or `max_width: 0` to keep every
pixel. When the image was scaled the answer says so — coordinates read off it
are no longer window coordinates.

## Waiting and batching

**`wait_for`** — `{"text": "Saved", "timeout_ms": 5000}`
Waits inside the app, checking once per painted frame, until every condition
given holds at once. Conditions: `element_id` (an element is painted), `text`
(painted anywhere in the window), `key_context` (a context is on the focus
chain), `app_state_path` + optional `app_state_equals` (a JSON pointer into
`get_app_state`). `absent: true` inverts the lot — that is how you wait for a
dialog to close. `timeout_ms` defaults to 3000 and is capped at 30000.
Running out of time is not an error: the answer says `satisfied: false` and
`checks` names the part that was missing.

**`batch`** — `{"steps": [{"method": "click_element", "params": {"element_id": "search"}}, {"method": "type_text", "params": {"text": "main.rs"}}]}`
Runs its steps in order inside the app and returns each step's result, plus one
`app_state`/`focus_info` at the end. At most 32 steps. Stops at the first
failure unless `stop_on_error: false`. `window_id` on the batch applies to
every step that does not name its own. A batch cannot contain a batch. A
screenshot taken inside a batch comes back as an image attached to the answer,
same as a standalone one.

## Driving

**`list_actions`** — `{"filter": "toggle", "include_bindings": true}`
The app's GPUI actions. `include_bindings` adds keybinding, context predicate
and doc comment; `only_available: true` keeps only actions whose context
predicate matches the current focus chain, i.e. "what would work right now".

**`execute_action`** — `{"action": "elane::ToggleTerminal"}`
Dispatched through the focus chain exactly as the keybinding would be.
Optional `args` for the few actions that take them.

**`send_key`** — `{"key": "enter"}` or `{"key": "s", "modifiers": {"ctrl": true}}`
GPUI key notation: lowercase names — `a`-`z`, `0`-`9`, `enter`, `escape`,
`tab`, `space`, `backspace`, `delete`, `up`, `down`, `left`, `right`, `home`,
`end`, `pageup`, `pagedown`, `f1`-`f12`.

**`type_text`** — `{"text": "src/main.rs"}`
One keystroke per character into the focused element; space, newline and tab
are translated. Characters GPUI cannot parse as a keystroke are skipped, and
the answer's `dispatched` count tells you how many arrived.

**`click_element`** — `{"element_id": "save-button"}` or `{"x": 120, "y": 340}`
Clicks the centre of the element, or the given window coordinates. Optional
`button: "Left" | "Right" | "Middle"`.

All four driving tools append `app_state` and `focus_info` to their answer —
see the staleness note in the `overview` topic.

## State

**`get_app_state`** — `{}`
`{window_count, active_window, windows}`, plus an `app` key holding whatever
the application's own state provider returns. If the app registered one, this
is the cheapest ground truth available — much cheaper than a tree or an image.

**`get_logs`** — `{}`
The in-app log buffer, up to 500 entries. The MCP module logs every dispatch
it performs, so this shows what actually happened on the app side.

**`gpui_guide`** — `{"topic": "recipes"}`
This documentation. Answered by the server itself, so it works even when no
app is running.

**`replay_script`** — `{"path": "tests/open-file.json", "seek": true}`
Replay a recorded session. `seek: true` skips the read-only steps and just
puts the app back where the work was; the default runs everything as a test,
where a `wait_for` that comes back unsatisfied is a failed assertion. Recording
is switched on by starting the server with `GPUI_MCP_RECORD=<path>`. See the
`recording` topic.
"#;

const RECIPES: &str = r#"# Recipes

## Do a whole interaction in one call

This is the recipe that matters. Anything you would send as three or four
calls belongs in one `batch`:

```json
{"name": "batch", "arguments": {"steps": [
  {"method": "click_element", "params": {"element_id": "search-input"}},
  {"method": "type_text",     "params": {"text": "src/main.rs"}},
  {"method": "send_key",      "params": {"key": "enter"}},
  {"method": "wait_for",      "params": {"text": "main.rs", "timeout_ms": 5000}},
  {"method": "take_screenshot", "params": {"element_id": "editor-pane"}}
]}}
```

One turn instead of five, and the screenshot comes back attached to the same
answer. Steps stop at the first failure, so a batch that reports `ok: true` did
the whole thing.

## Verify a UI change you just made

```json
{"name": "get_windows", "arguments": {}}
{"name": "take_screenshot", "arguments": {}}
```
Then narrow to the part you changed and compare it against what you intended:
```json
{"name": "inspect_ui_tree", "arguments": {"root_element_id": "sidebar", "format": "compact"}}
{"name": "take_screenshot", "arguments": {"element_id": "sidebar"}}
```

## See what is on screen

```json
{"name": "ui_snapshot", "arguments": {}}
```
```
ui_snapshot 4 — WindowId(1v1), 47 of 318 painted elements
- banner #title-bar @e1
- menu #gallery-sidebar @e2
  - listitem "Accordion" #item @e3
  - listitem "Alert" #item @e4
- group #gallery-container @e5
  - button "Save" #save-button @e6
  - textbox #search @e7
```
Narrow it when the window is busy: `{"interactive_only": true}` for what can be
clicked, `{"filter": "save"}` for one thing, `{"root_element_id": "@e5"}` for
one region.

## Press a button whose label you know

```json
{"name": "ui_snapshot", "arguments": {"filter": "save"}}
{"name": "click_element", "arguments": {"element_id": "@e6"}}
```
Refs come from the most recent snapshot and the next one replaces them; acting
on a stale ref fails with a message saying so rather than hitting the wrong
element. If the app has an action for the same thing, prefer `execute_action` —
it survives layout changes, a click does not.

## Type into a field

```json
{"name": "batch", "arguments": {"steps": [
  {"method": "click_element", "params": {"element_id": "search-input"}},
  {"method": "get_focus_info", "params": {}},
  {"method": "type_text", "params": {"text": "src/main.rs"}},
  {"method": "send_key", "params": {"key": "enter"}}
]}}
```
The `get_focus_info` step earns its place: typing into the wrong element looks
exactly like nothing happening, and this is what tells you which it was. Inside
a batch it costs nothing extra.

## Run a command the app knows

```json
{"name": "list_actions", "arguments": {"filter": "terminal", "include_bindings": true}}
{"name": "execute_action", "arguments": {"action": "elane::ToggleTerminal"}}
```

## Find out why a keystroke does nothing

```json
{"name": "get_focus_info", "arguments": {}}
{"name": "list_actions", "arguments": {"only_available": true}}
{"name": "send_key", "arguments": {"key": "down"}}
{"name": "get_logs", "arguments": {}}
```
`send_key` reports `dispatched: false` when nothing in the focus chain claimed
the keystroke. `only_available` tells you what the current focus *would*
accept, which usually names the missing binding immediately.

## Reach a deep part of the UI without fetching the whole tree

```json
{"name": "inspect_ui_tree", "arguments": {"max_depth": 2, "format": "compact"}}
{"name": "inspect_ui_tree", "arguments": {"root_element_id": "editor-pane", "max_depth": 3}}
{"name": "get_element", "arguments": {"element_id": "editor-pane"}}
```

## Check a value the app owns

```json
{"name": "get_app_state", "arguments": {}}
```
The `app` key holds the application's own snapshot, if it registered a state
provider. Prefer it over reading the tree: it is small, semantic, and not tied
to the painted frame.

## Wait for something the app is doing on its own

An input's answer already describes the frame after the input. What it cannot
know is a load, a debounce or an animation the app started for itself:

```json
{"name": "wait_for", "arguments": {"app_state_path": "/app/rows", "app_state_equals": 12}}
{"name": "wait_for", "arguments": {"text": "Done", "timeout_ms": 10000}}
```

Waiting happens inside the app, one check per painted frame. Calling a read
tool in a loop to achieve the same thing costs a model turn per look.

## Wait for a dialog to close

`absent` inverts the condition:

```json
{"name": "wait_for", "arguments": {"element_id": "save-dialog", "absent": true}}
```

## Confirm an input actually landed

```json
{"name": "execute_action", "arguments": {"action": "elane::ToggleTerminal"}}
```
That is the whole recipe: the answer carries `app_state` and `focus_info` from
after the frame that shows the change, and `settled: true` confirms the frame
was painted. `settled: false` means the window is not being drawn — minimised
or occluded on a platform that stops painting — and everything in the answer
describes an older frame.
"#;

const IDS: &str = r#"# Element ids and element types

## The four forms

Every tool that takes an element id accepts any of:

| form | example | when to use |
|---|---|---|
| ref | `@e7` | from the last `ui_snapshot` — the one to reach for |
| full | `WindowId(1)/view-1.panel[0]` | exactly what `inspect_ui_tree` returned — never ambiguous |
| global | `view-1.panel` | stable across window ids |
| suffix | `panel` | shortest; **the first match wins**, so check there is only one |

A `@ref` is shorthand for "the thing on that line of the snapshot I just
showed you". Each snapshot replaces the previous set, so a ref from an older
one fails with a message telling you to take a new snapshot — it never
silently resolves to whatever now sits on that line. An id copied out of
`format: "compact"` output works too, instance suffix and all.

The id path comes from GPUI's element ids: only elements the app gave an id
(`div().id("results")`) appear as a named segment. Anonymous layout elements
still appear in the tree, but with generated segments, which makes them poor
targets — they move whenever the layout does.

## Finding the element you want

- By its visible text: `inspect_ui_tree {"text_filter": "Save"}` keeps matching
  elements *and their ancestors*, so you see where the text sits. Take the
  innermost match.
- By the source file that renders it:
  `inspect_ui_tree {"element_type_filter": "button"}`. `element_type` is
  derived from the element's `source_location` filename, so this searches the
  app's source layout — `button.rs` renders `element_type: "button"`.
- By position in the hierarchy: `root_element_id` plus a small `max_depth`.

`source_location` on each element points at the line of app code that rendered
it. When you are about to change the UI, that is the file to open.

## When an id is not found

The error lists near-miss candidates from the current tree. That list is the
fastest way to fix a typo or spot that the element is in another window — or
that it does not exist yet because the frame you are looking at predates it.

## Making ids better

Ids are the app's to give. Elements you intend to target from here should get
an explicit, unique id in the app's source: `div().id("results")`. Unique
means unique within the window — the suffix form takes the first match, and
two elements ending in `.row` make that a coin flip.
"#;

const FOCUS: &str = r#"# Focus, key contexts and actions

GPUI dispatches a keystroke along the focus chain: from the focused element
up through its ancestors. Each level carries key contexts, and a binding fires
only where its context predicate matches. So "the key did nothing" is nearly
always "the binding was not in scope where the focus was".

## The diagnosis, in order

1. `get_focus_info` — `focus_id` says which element has focus, `key_contexts`
   lists the contexts active along the chain. If `has_focus` is false, nothing
   will accept keystrokes: click the element you meant to type into first.
2. `list_actions {"only_available": true}` — the actions whose predicate
   matches the current chain. If the action you wanted is absent, its binding
   is not in scope; if it is present, the binding is fine and the problem is
   elsewhere.
3. `send_key` and read `dispatched`. `false` means nothing in the chain
   claimed the keystroke.
4. `get_logs` — the app-side record of what was dispatched.

## Actions beat keystrokes

`execute_action` dispatches through the focus chain like a keybinding, but by
name. It does not depend on a binding existing, on the right context being
active, or on the key notation being right. When you are driving the app to
reach a state, prefer it. Keep `send_key` for when the keystroke itself is
what you are testing.

## The common fix in the app

When an input keeps focus and swallows navigation keys, bind the same action
in the input's context as well — GPUI gives the later binding precedence.
"#;

const SCREENSHOTS: &str = r#"# Screenshots

`take_screenshot` renders the window through GPUI's own renderer rather than
capturing the screen. Consequences worth knowing: it works when the window is
occluded or behind others, it needs no accessibility permission, and it shows
the app's last painted frame — not necessarily the state after an input you
just sent.

## Keep them small

An image costs tokens by its pixel dimensions, not by its file size, so a
full-window shot at a high DPI spends a large part of your context on detail
you rarely need. Two levers:

```json
{"name": "take_screenshot", "arguments": {"element_id": "sidebar"}}
{"name": "take_screenshot", "arguments": {"max_width": 600}}
```

Cropping to an element is the better one — it answers a specific question.
Beyond that, every image is downscaled to 1400px wide by default; `max_width`
changes that and `max_width: 0` turns it off when you genuinely need to read
fine detail. A scaled answer says `scale`, and coordinates measured off such an
image are *not* window coordinates: clicking them lands somewhere else.

The crop uses the element's bounds from the last painted frame, converted by
the window's scale factor.

## What a screenshot is good for

Layout, spacing, colour, overlap, clipping, and "does this look like what I
intended" — questions the element tree cannot answer.

## What it is bad for

Values, state and structure. `get_app_state` and a filtered `inspect_ui_tree`
answer those precisely, in a fraction of the tokens, and without you having to
read pixels.
"#;

const RECORDING: &str = r#"# Recording and replaying a session

A session driving an app is a sequence of steps. Written to a file it becomes
two useful things at once: a way to put the app back where the work is, and a
regression test that runs without an agent.

## Recording

The server records when it is started with `GPUI_MCP_RECORD` set to a path:

```
GPUI_MCP_RECORD=tests/open-file.json
```

Every successful tool call is appended. Failed calls are not: a script of
things that did not work replays nothing. The file is rewritten after each
step, so an interrupted session still leaves valid JSON behind.

You do not do anything special while recording. Drive the app as usual.

## What a script looks like

```json
{
  "name": "open-file",
  "app": "my-app",
  "steps": [
    { "method": "ui_snapshot", "params": { "interactive_only": true } },
    { "method": "click_element", "params": { "element_id": "open-file" } },
    { "method": "type_text", "params": { "text": "src/main.rs" } },
    { "method": "send_key", "params": { "key": "enter" } },
    { "method": "wait_for", "params": { "text": "main.rs", "timeout_ms": 5000 } }
  ]
}
```

Steps are the same `{method, params}` shape a `batch` takes. You can write one
by hand, and you should edit a recorded one: drop the steps that were you
looking around, and add `wait_for` steps where the app does something
asynchronous.

## Replaying

```json
{"name": "replay_script", "arguments": {"path": "tests/open-file.json", "seek": true}}
```

- `seek: true` skips the steps that only read — the tree, the screenshots —
  and runs the rest. Use it at the start of a session to reach the state you
  were working in, instead of clicking your way back there.
- `seek: false` (the default) runs everything as a test.

There is no separate assertion step, because there does not need to be one: a
`wait_for` that comes back unsatisfied **is** a failed assertion, and its
answer already says which condition did not hold. So the same file reaches a
state and tests reaching it.

## Refs are rewritten, when they can be

A `@ref` means "line seven of the snapshot I am looking at", which is
meaningless in a file. When the snapshot printed an id beside it, the recorder
writes the id down instead:

```
- button "Save" #save-button @e6      →  "element_id": "save-button"
```

When it did not — no id, or an id like `#item` that appears on several lines —
the ref is kept as written and the step carries a `note` saying so. Such a step
only replays correctly if the snapshot before it produces the same lines. The
fix is in the app: give that element its own id.

## In CI, without an agent

```sh
gpui-mcp-server replay tests/open-file.json
```

Prints a line per step and exits non-zero if any failed. `--seek` and
`--keep-going` work as above. That is the point of recording: the file an
agent produced by exploring costs nothing to run again.
"#;

const TROUBLESHOOTING: &str = r#"# Errors and what they mean

**`No running GPUI app found`** — no socket in the temp directory answered.
The app is not running, was built without its `mcp` feature, or never called
`init_mcp`. The app prints `[MCP] IPC Server listening on …` to stderr when it
did. Nothing needs restarting on this side: the socket is re-discovered on
every call, so starting the app is enough.

**`GPUI app not running or not reachable at …`** — the socket file exists but
nothing is listening. Usually a crashed app; the file is deleted on the next
discovery pass.

**`gpui-mcp protocol mismatch: …`** — the app and this server were built from
one crate at different times and their wire formats have diverged. The message
names which half to rebuild. Nothing else will work until it is done.

**`Element not found`, with a candidate list** — the id did not resolve in the
frame currently painted. Check the candidates for a typo, a different window,
or an element that only exists after an interaction you have not made yet.

**`Request timeout (10s)`** — the app's main thread did not answer within ten
seconds. It is blocked, in a modal loop, or busy. Nothing to fix here; look at
the app.

**A tool succeeded but nothing changed** — an input's answer describes the
frame after the input, so this is rarely staleness any more. Check `dispatched`
(for `send_key`) and `focus_info`: a keystroke nothing claimed, or one that
went to the wrong element, both look like this. If the app is doing the work
asynchronously, `wait_for` is what tells you when it finished.

**`settled: false`** — no frame was painted while the tool waited for one, so
everything in that answer describes an older frame. The window is minimised,
occluded, or on a platform that stops drawing invisible windows. Bring it to
the front and repeat the call.

**`wait_for` came back with `satisfied: false`** — not an error and not a
failure of the tool: the condition did not hold within the timeout. `checks`
lists each condition separately, which usually shows straight away whether the
element never appeared, the text differs, or the app-state pointer resolves to
nothing.

**Multiple apps answering** — discovery picks the newest and warns on stderr.
Set `GPUI_MCP_APP` (the name the app passed to `init_mcp`), and `GPUI_MCP_PID`
alongside it to pin one exact instance.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_mcp_protocol::protocol::methods;

    #[test]
    fn every_topic_is_reachable_by_name() {
        for t in TOPICS {
            assert!(topic(t.name).is_some(), "topic {} not resolvable", t.name);
        }
        assert!(topic(DEFAULT_TOPIC).is_some());
    }

    #[test]
    fn no_topic_is_empty() {
        for t in TOPICS {
            assert!(!t.summary.trim().is_empty(), "{} has no summary", t.name);
            assert!(
                t.body.trim().len() > 200,
                "{} looks like a stub ({} bytes)",
                t.name,
                t.body.trim().len()
            );
        }
    }

    #[test]
    fn topic_names_are_unique() {
        let mut names: Vec<&str> = TOPICS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate topic name");
    }

    /// The guide is only useful while it still describes the tools that exist.
    /// A new method that nobody documented fails here rather than silently
    /// leaving the agent to discover it.
    #[test]
    fn the_tools_topic_documents_every_method() {
        let body = topic("tools").expect("tools topic").body;
        for method in methods::ALL {
            assert!(
                body.contains(method),
                "method '{method}' is not documented in the 'tools' topic"
            );
        }
        assert!(
            body.contains(TOOL_NAME),
            "the guide tool itself is not documented"
        );
    }

    /// `INSTRUCTIONS` is in context for the whole session, so it has a budget.
    #[test]
    fn instructions_stay_short() {
        assert!(
            INSTRUCTIONS.len() < 1600,
            "instructions grew to {} bytes — move detail into a topic",
            INSTRUCTIONS.len()
        );
        assert!(INSTRUCTIONS.contains(TOOL_NAME), "no pointer to the guide");
        for name in ["overview", "recipes", "troubleshooting"] {
            assert!(INSTRUCTIONS.contains(name), "{name} not offered");
        }
    }

    #[test]
    fn unknown_topic_lists_the_alternatives() {
        let msg = unknown_topic_message("nope");
        assert!(msg.contains("nope"));
        for t in TOPICS {
            assert!(msg.contains(t.name), "{} missing from the menu", t.name);
        }
    }

    #[test]
    fn topic_names_render_as_a_list() {
        let names = topic_names();
        assert!(names.contains(DEFAULT_TOPIC));
        assert!(names.contains(", "));
    }
}
