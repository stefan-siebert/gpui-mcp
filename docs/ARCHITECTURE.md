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

## Recording and replay

A session is already a sequence of steps; writing it down costs nothing. The
file then does two jobs that would otherwise need two mechanisms: it puts the
app back where the work was, and it is a regression test that runs without an
agent.

Three decisions make it small.

**A script's steps are `batch` steps.** There was no reason to invent a second
shape for "a tool and its arguments", and reusing the first one means a script
can be written by hand as easily as recorded, and pasted into a `batch`.

**`wait_for` is the assertion.** A wait that comes back unsatisfied is a failed
expectation, and it already reports which of its conditions did not hold. So
replay needs no assertion step, no matcher vocabulary, and no second way to say
what should be true. Adding one would have been the obvious design and the
wrong one.

**Recording lives in the server.** No app change, no wire change, and it works
for any app the server can reach. The cost is that the server only sees what
passes through it — which is everything an agent does, and nothing a person
does by hand at the keyboard. Recording real user input would have to happen in
the app; it is not needed for the two jobs above.

### Refs have to be rewritten, and sometimes cannot be

`@e7` means "line seven of the snapshot I am looking at". In a file that is a
number, not an intention. So the recorder reads the snapshot text it just
produced, and where a line carried an id it writes the id down instead of the
ref.

Where it could not — no id on that line, or an id like `#item` that appears on
forty of them — it keeps the ref and attaches a note. Rewriting an ambiguous id
would be worse than useless: `#item` suffix-matches the *first* item, so the
script would replay cleanly and click the wrong thing. A script that admits it
is fragile beats one that lies.

That failure mode is also the strongest argument for giving elements explicit
ids in the app, which is the same thing the snapshot has been asking for.

### What replay does not do yet

The plan lists three things a test suite eventually needs and this does not
have: a pinned window size (layout, and therefore any golden image, depends on
it), golden-screenshot comparison with a perceptual tolerance, and a defined
starting state via an app-side reset hook. All three are additive.

## The accessibility audit

`a11y_audit` reads the derived layer the snapshot prints, which fixes both what
it can find and what it cannot. It cannot check contrast: colours never reach
this side. It cannot check what a control announces when its state changes:
nothing here knows the state. Claiming otherwise would be worse than the gap.

What it can see is the overlap between an accessibility problem and a targeting
problem, and that overlap turns out to be most of what matters here:

- **A control that paints no text** has no accessible name *and* nothing an
  agent can match on. The icon-only button is the same bug for both readers.
- **An id that names several elements** breaks the promise that an id
  identifies something. A suffix match takes the first, so a recorded script
  targeting `#item` clicks the first of sixty-two — quietly, and only in the
  run where the order changed. This one was found the hard way: the recorder
  hit it before the audit existed.
- **A target under 24 px** (WCAG 2.2's minimum) is hard to hit with a shaky
  hand, a finger, or a synthetic click at an element's centre.

So the audit is not a side quest bolted onto a driving tool. **The same fix
serves both readers**: give the element a label and its own id, and it becomes
announceable and targetable in one move. That is also the fix the snapshot has
been asking for since it started printing `#id`.

A finding carries the source location gpui recorded, which is worth being
precise about: for a gpui-component widget that is the *widget's* file, so it
says what the element is, not where the app put it. The id and the element path
are what locate it in the app's code. An earlier draft of this documentation
called it "the line to change"; running the audit against a real app showed
that to be wrong.

Because a failing audit fails a replay step, accessibility becomes part of a
regression run rather than something that was checked once. That reuses the
same rule as `wait_for`: a step that reports it is not satisfied is a failed
expectation, and no new vocabulary was needed to say so.

## The real accessibility tree, and why it is a second view

`a11y_tree` returns what AccessKit hands a screen reader. It is tempting to
treat it as a better snapshot and retire the derived layer, and the numbers say
otherwise: against the gpui-component gallery it is **11 nodes over 96 painted
elements**. A node exists only where somebody annotated the element, so the
tree is not a cleaner view of the window — it is a view of the annotated part
of it. The derived layer is what still sees the other 85, which is why both
exist and why the answer reports `nodes` against `painted` rather than letting
the tree pass for the UI.

Where a node does exist it is strictly better, and in a way the derived layer
cannot be talked into. The four title-bar buttons the audit reports as `#menu`
paint no text at all; the tree names them `GPUI Component`, `Edit`, `Window`,
`Help`. A role there is declared, not inferred from a filename. An input
carries its `value`. None of that is guessable from a painted frame.

Two decisions are load-bearing here:

**The tree is switched on, not assumed.** GPUI builds it only while assistive
technology is attached — correct for a shipping app, useless for checking one.
`Window::set_a11y_force_active` in the gpui fork ORs a `force_enabled` flag into
the activation check, leaving `Application::new_inaccessible` the final word and
changing nothing for an app that never asks. This is the one place this project
depends on a fork patch for a *feature* rather than for the inspector itself.

**It takes effect from the next frame, so the method is asynchronous.** The
frame being painted latched its answer before the first node was pushed, and
the builder keeps a node stack that must be pushed and popped exactly once per
frame — a mid-frame flip would corrupt it. So `a11y_tree` joins `wait_for` and
`batch` as a method answered after a frame rather than from the one on screen,
and the first call on a window costs one frame more than the ones after it.

The join back to the snapshot is free rather than geometric: each node carries
the `element_id` and `source_location` gpui recorded, so a node lines up with a
snapshot line by name. An earlier reading of this design assumed the only
available join was bounds-against-bounds, which would have been fuzzy enough to
matter. It is worth noting that this per-node provenance is `debug_assertions`
only — as is the whole inspector, so nothing is lost.

## Folding the tree into the snapshot

The snapshot keeps the spine and the tree overlays it. That order is not a
preference — it is what the numbers force. Eleven annotated nodes cannot
describe a window of ninety-six painted elements, and the derived layer is
what still sees the other eighty-five. Where a node does exist it wins on
role, because a role the widget declared beats one inferred from the file that
rendered it; it supplies a name when nothing is painted; and it contributes
state, which the derived layer was never able to reach at all.

**The join had to be exact, and the obvious key is not.** A node records the
leaf of its element id and its source location. The gallery's four title-bar
buttons are all `Name("menu")` from `button.rs:231:19` — matching on that pair
picks one of the four at random, which is precisely the failure `duplicate-id`
exists to report, committed by the tool that reports it. What makes it exact
is that gpui derives a node's AccessKit id by hashing the element's whole
`GlobalElementId`, so the fork puts that id on `InspectorElementInfo` and the
two sides are joined by identity. The value is computed once, inside gpui, and
only read here — nothing on this side re-hashes anything, so nothing depends
on a hasher staying stable across versions. Checked by set intersection
against a real window: every node matches exactly one element.

**A declared fact and an inferred one are marked apart.** A line backed by a
node ends in `✓`. It would have been easy to let the better data win silently,
and wrong: an agent that cannot tell an announced role from a guessed one
cannot calibrate how much to trust a line, and the audit's whole argument is
that annotating is worth doing — which is invisible if the snapshot hides who
has and has not.

**An unmapped role changes nothing.** AccessKit roles are translated into the
vocabulary the snapshot already prints, and a role missing from that table
leaves the derived one in place rather than printing a second spelling.
`filter`, `interactive_only` and the audit all match on those strings; two
spellings of "button" would quietly halve every one of them.

**Reading the tree is switched on by the reader.** `ui_snapshot` and
`a11y_audit` force the window into building a tree and wait one frame the
first time, which makes both asynchronous — a real cost on the two most-used
methods. The alternative was worse: findings that depend on whether something
else happened to call `a11y_tree` first is hidden state, and an audit that
reports differently on two identical runs is worse than one with less data.

**A missing node is counted, not reported.** The design sketch had an
"unreachable control" finding for an interactive element with no node.
Measuring killed it twice over: it would have fired eighty-one times on the
gallery, and it would have found nothing, because every interactive element
there already has a node and an app's own clickable `div` has no derived role
for the check to see either. What shipped instead is `announced` beside
`checked` in the answer — one number, which cannot be tuned out — and a
split in `unnamed-control`, which now tells an element with a node to add a
label and an element without one to add a role first. The second is the
important half: telling something with no node to "add a label" is advice that
cannot work.

## Determinism: the three ways a replay drifts

A recorded script is only a test if it means the same thing tomorrow. Three
things can change underneath it, and each is handled where it can be handled.

**The window size is pinned by the script, not by the machine.** It decides
layout — a sidebar collapses, a toolbar folds into a menu, and the element a
step wanted is somewhere else or nowhere. So a script carries a `viewport`
header and `replay` applies it before the first step, and the recorder asks the
app for the size once so the header exists whether or not the session ever
looked at a window. A resize that fails aborts the run instead of letting every
later step fail for a reason none of them names.

**The starting state is the app's to define.** Nothing on this side can make an
app left on the third tab with two files open behave like one that just
started, so `reset_app` calls a hook the app registers, mirroring the app-state
provider. When no hook is registered the method *fails*. A silent no-op was the
obvious alternative and is the worse one: a replay that believes it started
from a known state and did not is a green run hiding a bug, and the whole point
of the stage is not producing those.

**A golden screenshot is compared in the server.** The goldens belong beside
the script — both are artefacts of a test run, not of the application — which
is also why the comparison is not in the app. This costs one dependency,
`image` with PNG only. An earlier decision in this project declined `image` for
JPEG screenshots; that was a different question with a different answer,
because an image costs tokens by its dimensions and the encoding bought
nothing. Comparing pixels cannot be done without decoding them.

**Two numbers, and neither is called perceptual.** Images match when they are
the same size and at most `pixel_tolerance` of pixels differ by more than
`channel_tolerance` per channel. Text rendering, subpixel positioning and GPU
filtering move edge pixels by a level or two between runs on the same machine;
a comparison that called those a failure would fail every time, and a check
that always fails is a check nobody reads. A fraction rather than a count is
what lets one tolerance mean the same thing at every window size. The
roadmap asked for a perceptual tolerance and this is not one — saying so is
cheaper than pretending otherwise and being believed.

**A failure leaves evidence.** The new image is written beside the golden as
`<name>.actual.png`. A failure that reports only a percentage cannot be acted
on; two files can be opened side by side. And a size mismatch is reported on
its own rather than as "100% of pixels differ", because it has exactly one
cause worth naming: the window was not pinned.

**Updating goldens is an environment variable, not a parameter.** A script that
could ask for its own golden to be rewritten would never fail. Accepting a new
appearance is a decision a person makes for a whole run, after looking at what
changed.
