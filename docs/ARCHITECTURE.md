# Architecture

Why this thing is shaped the way it is. The README says what it does; this says
what was measured, what was decided, and what would break if a decision were
quietly undone.

## The measurement everything follows from

The obvious assumption about a tool like this is that the transport is the slow
part. It is not.

| step | cost | how it was measured |
|---|---|---|
| socket discovery | ~0.5 ms per call | 20 calls in one server process, with vs. without `GPUI_MCP_PID` set, against a temp dir holding 488 entries |
| the stdio loop plus one connect | ~1.5 ms per call | the same 20 calls, divided |
| launching the server, `initialize`, one `get_windows`, exit | ~40 ms | one process per call against a live app; process startup dominates, and an agent's server stays running |
| one *agent* tool call | seconds | a model turn |

A model turn costs roughly a thousand times what the socket costs. Payload is
the other half of the bill, and on a real window it is large:

| answer | size |
|---|---|
| `inspect_ui_tree`, unfiltered | 151 KB |
| `inspect_ui_tree`, `format: "compact"`, `max_depth: 3` | 63 KB |
| `ui_snapshot` | 3.4 KB |
| `ui_snapshot { interactive_only: true }` | 332 B |

(gpui-component's story app, 96 painted elements, 1600×1200.)

So every decision here trades work inside the app — milliseconds, sometimes a
whole redraw — against **turns** and **tokens**. Optimising the socket would be
optimising the part that already costs nothing.

## The frame contract

gpui's inspector data is read from `rendered_frame`: the last frame that was
actually painted. `window.render_to_image()` renders that same frame's scene.
Everything an agent can read therefore describes the past.

The original code dispatched a click and assembled the answer in the same
main-thread tick, before gpui had laid out and painted again. The `app_state`
and `focus_info` it helpfully attached were systematically the state the click
had just replaced. That produced the loop this whole project exists to kill:
*click → inspect → looks unchanged → inspect again*, one model turn per
attempt.

Input methods now wait for the frame that shows their effect. The mechanism is
worth stating precisely, because it is not obvious and a plausible-looking
simplification breaks it:

> `on_next_frame` callbacks are drained at the **start** of a frame request,
> *before* the draw that request performs. A single callback therefore still
> sees the previous `rendered_frame`. Two of them bracket exactly one completed
> draw — and that is the draw carrying the input.

Hence `settle()` awaits two callbacks. Only the first asks for a redraw
(`window.refresh()`): it has to guarantee a draw happens at all, even for an
input that dirtied nothing. Making a *wait* force a redraw every frame would
re-render the whole window for as long as the wait lasted, which is why
`next_frame` takes `force_draw` instead of always doing it.

If no frame arrives within `FRAME_TIMEOUT` (200 ms), the answer says
`settled: false` rather than hanging. A minimised or occluded window on some
platforms simply stops being drawn, and reporting that is more useful than
waiting it out.

**Verification.** In one `batch`: two screenshots with nothing between them are
byte-identical; a screenshot, a click, and a second screenshot produce two
different images. The control is what makes the second result mean anything.

## Waiting belongs in the app

`wait_for` checks its conditions once per painted frame, inside the app. The
alternative — the agent calling a read tool in a loop — costs a model turn per
look. Measured: a 400 ms wait performed **31 checks**; the same coverage from
outside would have been 31 turns.

Running out of time is not an error. `satisfied: false` is a fact, and often
the fact being asked for (`absent: true` waits for something to *stop* being
there). The answer carries a per-condition breakdown, so a timeout says which
part was missing instead of just disappointing.

Requests are answered sequentially, in arrival order. A `wait_for` suspends
without blocking the main thread, but letting a later call overtake it would
reorder inputs an agent meant as a sequence.

## Batching

`batch` is not faster than sending its steps one at a time — the socket was
never the slow part. It saves turns: five steps sent separately cost five round
trips *through the agent*.

Steps stop at the first failure by default. Continuing is the dangerous
behaviour: a batch usually describes one intention, and carrying on past a
failed click means typing into whatever happened to have focus instead. A batch
that reports `ok: true` did the whole thing.

Post-dispatch state is attached once, at the end, rather than after every step —
repeating `app_state` and `focus_info` per step is most of what a batch answer
would otherwise weigh.

## The snapshot

`ui_snapshot` prints one line per element that means something:

```
- button "Save" #save-button @e6
```

An element earns its line by having a **role**, an **id somebody wrote**, or
**text**. Everything else is layout scaffolding: it is dropped and its children
take its place. The size difference against the tree comes mostly from the
format — one line versus a JSON object carrying bounds, a content mask, a
source location and a content size.

The server hands the snapshot back as **text**, not as pretty-printed JSON.
Escaping the indentation into `\n  ` sequences would roughly double it and
destroy the structure that makes it readable.

### Roles come from the file that rendered the element

`source_location` is already in the data, and gpui-component keeps one widget
per file, so `button/button.rs` renders a `button`. Every app on that crate
gets a semantic vocabulary with nothing to annotate — which is why the payoff
of the accessibility layer arrived before the accessibility layer did.

Two rules keep the derivation honest:

- **A region role must contain something.** `title_bar.rs` paints the title bar
  *and* its close button; the file name alone would label that button `banner`.
  Roles like `banner`, `list`, `dialog` are only used for an element with
  children. A leaf falls back to `node` plus its id and text — vague, but not
  wrong.
- **A `test_id` is a name somebody wrote.** Lowercase, dashes or underscores.
  Never `view-4294967734`, never `1-0-0`, never a CamelCase type name: those
  change when the app does, and printing them would invite an agent to target
  them.

What a file name cannot say is `checked`, `selected`, `disabled`, or what an
input currently holds — and an app's own widgets have no role at all. That
needs explicit annotation, which is the open half of the work (see PLAN.md).

### Refs

Each snapshot line ends in `@e7`, usable anywhere an element id is taken. A ref
is shorthand for "the thing on that line of the snapshot I just showed you", so
the table is **replaced whole** by the next snapshot. A ref from an older one
fails with a message saying to take a new snapshot; it never resolves to
whatever now sits on that line. Keeping old refs alive would let an agent act
on something it can no longer see.

## Element ids

Four forms resolve, everywhere an id is taken: a `@ref`, the full id, the
global id, or a suffix of it (first match wins).

An id copied out of `format: "compact"` output resolves too. That was a bug
worth remembering: compact output strips crate paths from each segment and
keeps the `[instance]` suffix, so an id the server had *just printed* came back
as "element not found". The format we recommend was a trap. All resolution
paths now share one `id_matches`, rather than the three near-identical copies
that made the inconsistency possible in the first place.

## Screenshots

Images cost an agent tokens by their **pixel dimensions**, not by their file
size. That single fact settles two questions:

- Downscaling is worth it — default `max_width: 1400`, `0` to disable — and a
  scaled answer reports `scale`, because coordinates read off such an image are
  not window coordinates.
- Offering JPEG is not. It changes bytes, not tokens, and would add an encoder
  dependency for nothing. The plan originally listed it; the measurement
  removed it.

The app writes the image to a temp file and the server reads, inlines and
deletes it. The file name carries a sequence number as well as the pid, because
a `batch` may take several screenshots and naming them alike meant the last one
overwrote a file the server had not read yet — the first image came back
showing the last frame, and the last came back with no image at all.

## Documentation is part of the product

An agent that has to discover this server's shape by trial and error spends a
turn per discovery. So the server documents itself, on four surfaces generated
from one topic table in `src/docs.rs`:

- `instructions` in the `initialize` result — short, and in context all
  session.
- the `gpui_guide` tool — the long form, one topic per call. Answered by the
  server itself without touching the socket, so it works with **no app
  running**, which is exactly when an agent is most likely to read it.
- MCP resources (`gpui://guide/<topic>`), for clients that attach resources.
- the `onboard` prompt, which Claude Code surfaces as a slash command.

A test asserts that every method in `methods::ALL` appears in the guide's
`tools` topic, so a new tool cannot ship undocumented.

## Protocol versioning

The two halves are built from one crate by different mechanisms: the library is
linked into the app and rebuilt whenever the app is; the server binary is
launched from `target/release/` by an agent and is rebuilt by nobody. They
drift.

`PROTOCOL_VERSION` is bumped **only for a change that is not backward
compatible**, where "compatible" means the failure would be loud. A new
`#[serde(default)]` field is compatible. A new method is compatible too: an
older app answers `Unknown method: …`, which is diagnosable.

v2 was bumped for frame-synchronous answers. The types stayed compatible, but a
new server against an old app would have *promised* frame-synchronous answers
the app does not give — silent, and exactly the drift the handshake exists to
catch. `ui_snapshot` arrived without a bump, by the same rule.

## Things deliberately not done

- **Caching the socket path.** Re-resolving costs ~0.5 ms and buys transparent
  survival of an app restart, which happens constantly during development.
- **Concurrent request handling.** See *Waiting belongs in the app*.
- **A persistent connection.** Same reason as the path: the saving is invisible
  next to a model turn, and a fresh connection cannot go stale.
- **Optimising `inspector_elements()`.** It matches every painted text against
  every hitbox, which is quadratic — but it lives in the gpui fork, not here.
  `build_element_tree` was the quadratic loop on this side, and that one is
  fixed.
