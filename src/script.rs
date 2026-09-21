//! Recording what an agent did, and doing it again.
//!
//! A session driving a GPUI app is already a sequence of steps; writing it
//! down costs nothing and buys two things. The cheap one: replaying it puts
//! the app back into the state where work happens, in one call instead of the
//! dozen turns it took to find. The valuable one: the same file is a
//! regression test, and it runs in CI without an agent and without model cost.
//!
//! Recording happens in the server, so no app change and no wire change is
//! involved. A script's steps are the same `{method, params}` shape a `batch`
//! takes, because there was no reason to invent a second one.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Name of the tool that replays a recorded script. Server-local, like the
/// guide: it drives the app through the other tools rather than being one of
/// them.
pub const REPLAY_TOOL: &str = "replay_script";

/// A recorded session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Script {
    /// Where the file was read from, when it was. Not part of the file: it is
    /// what a relative golden path is relative to, so the goldens travel with
    /// the script instead of depending on the directory a replay is started
    /// from.
    #[serde(skip)]
    pub source: Option<PathBuf>,
    /// Free-form, for whoever reads the file. Defaults to the file stem.
    #[serde(default)]
    pub name: String,
    /// The `GPUI_MCP_APP` this was recorded against, when one was set. Replay
    /// does not enforce it — it is a note for the reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_with: Option<String>,
    /// The window size this was recorded at, in logical pixels.
    ///
    /// Replay applies it before the first step. A window's size decides its
    /// layout, so a script recorded at one size and replayed at another is not
    /// replaying the same UI — a sidebar collapses, a toolbar folds into a
    /// menu, and the element a step wanted is somewhere else or nowhere. This
    /// is the cheapest determinism available, and the one a golden screenshot
    /// depends on completely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
    pub steps: Vec<Step>,
}

/// A window's content size, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

/// One step: a tool name and its arguments, exactly as they were sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    /// Written by the recorder when it could not make a step reproducible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Tools that describe the app rather than change it. A `seek` replay skips
/// them: re-reading the tree on the way to a state is pure cost.
pub const READ_ONLY: &[&str] = &[
    "a11y_audit",
    "a11y_tree",
    crate::golden::TOOL_NAME,
    "get_windows",
    "get_app_state",
    "get_logs",
    "get_element",
    "get_focus_info",
    "inspect_ui_tree",
    "list_actions",
    "take_screenshot",
];

/// Tools that are the server's own and mean nothing to a replay.
pub const NOT_RECORDED: &[&str] = &[crate::docs::TOOL_NAME, REPLAY_TOOL];

pub fn is_read_only(method: &str) -> bool {
    READ_ONLY.contains(&method)
}

/// Create the directory a file is about to be written into. A script and the
/// goldens beside it are written the same way.
pub fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

impl Script {
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Cannot read script {}: {}", path.display(), e))?;
        let mut script: Script = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{} is not a gpui-mcp script: {}", path.display(), e))?;
        script.source = Some(path.to_path_buf());
        Ok(script)
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        ensure_parent_dir(path)?;
        std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(self)?))?;
        Ok(())
    }

    /// The directory relative golden paths are resolved against: the script's
    /// own, when it is known.
    fn base_dir(&self) -> Option<&Path> {
        self.source
            .as_deref()
            .and_then(Path::parent)
            .filter(|dir| !dir.as_os_str().is_empty())
    }

    /// A step's params as the tool should see them. The one rewrite: a golden
    /// path is relative to the script file, and the tool knows nothing about
    /// scripts, so it is made relative to the working directory here.
    fn params_for(&self, step: &Step) -> serde_json::Value {
        let mut params = step.params.clone();
        if step.method != crate::golden::TOOL_NAME {
            return params;
        }
        let Some(dir) = self.base_dir() else {
            return params;
        };
        if let Some(path) = params.get("path").and_then(|path| path.as_str()) {
            // `has_root`, not `is_relative`: on Windows `/abs/x.png` is not
            // absolute, and is still not a path to hang off the script.
            if !Path::new(path).has_root() {
                // Folded, so `../scripts/../golden/x.png` reads as the
                // `../golden/x.png` it is when it comes back in a message.
                params["path"] = serde_json::json!(normalise(&dir.join(path)).to_string_lossy());
            }
        }
        params
    }
}

/// `target` written relative to `base`, both absolute — the way a golden path
/// is written into a script so it still resolves when the script moves.
///
/// `None` when no relative path exists (a different drive on Windows), in
/// which case the absolute path is the honest thing to write.
pub fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let (base, target) = (normalise(base), normalise(target));
    let base: Vec<Component> = base.components().collect();
    let target: Vec<Component> = target.components().collect();

    // Prefix and root have to agree, or there is no path from one to the other.
    let is_anchor = |c: &Component| matches!(c, Component::Prefix(_) | Component::RootDir);
    let base_anchor: Vec<&Component> = base.iter().filter(|c| is_anchor(c)).collect();
    let target_anchor: Vec<&Component> = target.iter().filter(|c| is_anchor(c)).collect();
    if base_anchor != target_anchor {
        return None;
    }

    let shared = base
        .iter()
        .zip(target.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut relative = PathBuf::new();
    for _ in shared..base.len() {
        relative.push("..");
    }
    for component in &target[shared..] {
        relative.push(component);
    }
    Some(relative)
}

/// Fold `.` and `..` so that two spellings of one directory compare equal.
fn normalise(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Appends every tool call to a script file as it happens.
///
/// The file is rewritten after each step rather than appended to, so an
/// interrupted session still leaves valid JSON behind — a script is small, and
/// a half-written one would be worse than a slow one.
pub struct Recorder {
    path: PathBuf,
    script: Script,
    /// Whether the window size has been asked for yet. Asked once: a session
    /// against an app with no window would otherwise pay a discovery and a
    /// round trip on every step for the rest of its life.
    viewport_probed: bool,
    /// `e7` -> `save-button`, learned from the last snapshot this server sent.
    /// Only ids that appeared once in that snapshot: see [`Recorder::learn_refs`].
    refs: HashMap<String, String>,
    /// `e3` -> `item`, for refs whose id appeared more than once.
    ambiguous: HashMap<String, String>,
    /// `item` -> 62, every id in that snapshot and how many lines carried it.
    /// This is what catches an id a step names outright rather than by ref.
    id_counts: HashMap<String, usize>,
}

impl Recorder {
    pub fn new(path: impl Into<PathBuf>, app: Option<String>) -> Self {
        let path = path.into();
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("recording")
            .to_string();

        Self {
            script: Script {
                source: Some(path.clone()),
                name,
                app,
                recorded_with: Some(format!(
                    "{} {}",
                    env!("CARGO_PKG_NAME"),
                    env!("CARGO_PKG_VERSION")
                )),
                viewport: None,
                steps: Vec::new(),
            },
            path,
            viewport_probed: false,
            refs: HashMap::new(),
            ambiguous: HashMap::new(),
            id_counts: HashMap::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the window size still has to be asked for. True exactly once.
    pub fn needs_viewport(&self) -> bool {
        !self.viewport_probed
    }

    /// Record the window size the session is happening at, or that it could
    /// not be read. Returns whether a size was written.
    ///
    /// Written once, from the first window seen, and never revised: a script
    /// records the size it was *made* at. A later resize is a step in the
    /// script, and rewriting the header to match it would quietly make the
    /// header agree with whatever happened last.
    pub fn note_viewport(&mut self, size: Option<(f32, f32)>) -> bool {
        self.viewport_probed = true;
        match size {
            Some((width, height))
                if self.script.viewport.is_none() && width >= 1.0 && height >= 1.0 =>
            {
                self.script.viewport = Some(Viewport { width, height });
                true
            }
            _ => false,
        }
    }

    pub fn steps(&self) -> usize {
        self.script.steps.len()
    }

    /// Record one tool call. `snapshot` is the rendered snapshot text when the
    /// call was a `ui_snapshot`, which is where refs come from.
    pub fn record(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        snapshot: Option<&str>,
    ) -> anyhow::Result<()> {
        if NOT_RECORDED.contains(&method) {
            return Ok(());
        }

        if let Some(snapshot) = snapshot {
            self.learn_refs(snapshot);
        }

        let (mut params, note) = self.resolve_refs(params.clone());
        if method == crate::golden::TOOL_NAME {
            self.anchor_golden_path(&mut params);
        }
        self.script.steps.push(Step {
            method: method.to_string(),
            params,
            note,
        });

        self.script.write(&self.path)
    }

    /// Write a golden path relative to the script rather than to wherever this
    /// server happened to be started.
    ///
    /// The agent named the golden relative to the server's working directory,
    /// which is whatever the MCP client chose and is not written down anywhere.
    /// Relative to the script, the same path means the same file from any
    /// directory a replay is started in — and when no relative path exists
    /// (another drive), the absolute one is written, which is at least honest.
    fn anchor_golden_path(&self, params: &mut serde_json::Value) {
        let Some(path) = params.get("path").and_then(|path| path.as_str()) else {
            return;
        };
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };
        let target = cwd.join(path);
        let base = cwd.join(self.path.parent().unwrap_or(Path::new("")));

        // Two spellings of one directory — a symlink, an 8.3 short name on
        // Windows — would otherwise share no prefix and produce a relative
        // path that climbs to the root and back down. Both exist by now: the
        // golden was just written, and the script file with it.
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        let written = relative_path(&canonical(&base), &canonical(&target)).unwrap_or(target);
        params["path"] = serde_json::json!(written.to_string_lossy());
    }

    /// Read `#test-id` and `@ref` off each snapshot line, so a ref recorded
    /// later can be written down as something that will still mean the same
    /// thing tomorrow.
    ///
    /// An id that appears on more than one line is not that something. A
    /// sidebar of forty entries all called `#item` would have every recorded
    /// click rewritten to the first of them — a script that replays cleanly
    /// and does the wrong thing, which is worse than one that admits it
    /// cannot. Those refs are kept aside and reported instead.
    fn learn_refs(&mut self, snapshot: &str) {
        self.refs.clear();
        self.ambiguous.clear();
        self.id_counts.clear();

        let mut lines = Vec::new();
        for line in snapshot.lines() {
            let Some((reference, test_id)) = parse_snapshot_line(line) else {
                continue;
            };
            *self.id_counts.entry(test_id.to_string()).or_insert(0) += 1;
            lines.push((reference.to_string(), test_id.to_string()));
        }

        for (reference, test_id) in lines {
            let table = match self.id_counts.get(&test_id) {
                Some(1) => &mut self.refs,
                _ => &mut self.ambiguous,
            };
            table.insert(reference, test_id);
        }
    }

    /// Replace `@e7` with the id the snapshot printed beside it, and say so
    /// when the id that ends up in the file will not hold.
    ///
    /// A ref means "line 7 of the snapshot I am looking at", which is true for
    /// exactly as long as that snapshot is the current one. Writing it into a
    /// file unchanged would record a number, not an intention.
    fn resolve_refs(&self, mut params: serde_json::Value) -> (serde_json::Value, Option<String>) {
        let mut notes: Vec<String> = Vec::new();

        for key in ["element_id", "root_element_id"] {
            let Some(value) = params.get(key).and_then(|value| value.as_str()) else {
                continue;
            };
            let value = value.to_string();

            // Either the step named a ref, which becomes an id here, or it
            // named an id outright. Both end up as something written into the
            // file, and both are worth the same doubts.
            let written = match value.strip_prefix('@') {
                None => Some(value.clone()),
                Some(reference) => match self.refs.get(reference) {
                    Some(test_id) => {
                        params[key] = serde_json::json!(test_id);
                        Some(test_id.clone())
                    }
                    None => {
                        notes.push(match self.ambiguous.get(reference) {
                            Some(test_id) => format!(
                                "'{value}' pointed at #{test_id}, which appears on more than one \
                                 line of that snapshot and so does not identify it. The ref was \
                                 left as written and will only replay if the snapshot before it \
                                 produces the same lines. Give that element its own id in the app."
                            ),
                            None => format!(
                                "'{value}' is a snapshot ref with no id beside it, so it was left \
                                 as written and will only replay if the snapshot before it \
                                 produces the same lines. Give that element an id in the app."
                            ),
                        });
                        None
                    }
                },
            };

            if let Some(note) = written.as_deref().and_then(|id| self.doubts_about(id)) {
                notes.push(note);
            }
        }

        (params, (!notes.is_empty()).then(|| notes.join(" ")))
    }

    /// What is wrong with the id this step is about to be written down with.
    ///
    /// The audit reports both of these too, but it reports them about the app,
    /// later, if anyone runs it. Here they are reported about *this step*, at
    /// the moment the script is being written — which is the moment somebody
    /// can still pick a different element to click, or go and name it.
    fn doubts_about(&self, id: &str) -> Option<String> {
        // A step may name an id in any of the forms the app resolves: `#save`,
        // a bare `save`, or a whole dotted path ending in it. The snapshot
        // counts last segments, so compare last segments.
        let segment = id.trim_start_matches('#').rsplit('.').next()?;

        if let Some(count) = self.id_counts.get(segment).filter(|count| **count > 1) {
            return Some(format!(
                "#{segment} names {count} elements in the snapshot before this step. A suffix \
                 match takes the first, so this step may replay against a different one than it \
                 was recorded against. Give that element its own id in the app."
            ));
        }

        gpui_mcp_protocol::protocol::id_looks_generated(segment).then(|| {
            format!(
                "#{segment} ends in a number the app generates fresh on every start, so it reads \
                 like a name and is not one: this step will find nothing after a restart. Give \
                 that element an id of its own in the app."
            )
        })
    }
}

/// Pull `(ref, test_id)` out of one snapshot line, e.g.
/// `  - button "Save" #save-button @e6` -> `("e6", "save-button")`.
///
/// A line without an id yields nothing: there is no stable name to record.
fn parse_snapshot_line(line: &str) -> Option<(&str, &str)> {
    let (before_ref, after_ref) = line.rsplit_once(" @")?;
    let reference = after_ref.split_whitespace().next()?;
    if !reference.starts_with('e') || reference[1..].parse::<usize>().is_err() {
        return None;
    }

    // Look for the id only in front of the ref, and only accept something
    // shaped like one — a name can contain a `#` of its own, and reading
    // `"Item #3"` as an id would record a selector that matches nothing.
    let test_id = before_ref.rsplit_once(" #")?.1.split_whitespace().next()?;
    let looks_like_an_id = !test_id.is_empty()
        && test_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');

    looks_like_an_id.then_some((reference, test_id))
}

/// How to replay a script.
#[derive(Debug, Clone)]
pub struct ReplayOptions {
    /// Skip the steps that only look at the app. Getting somewhere is the
    /// point; re-reading the tree on the way is pure cost.
    pub seek: bool,
    /// Stop at the first failing step. On by default for the same reason a
    /// batch does: the steps after a failure are acting on a state nobody
    /// intended.
    pub stop_on_error: bool,
}

impl Default for ReplayOptions {
    /// A test run: every step, stopping at the first failure — what the tool
    /// and the command line both do unless told otherwise.
    fn default() -> Self {
        Self {
            seek: false,
            stop_on_error: true,
        }
    }
}

/// What one step did.
#[derive(Debug, Clone, Serialize)]
pub struct StepOutcome {
    pub index: usize,
    pub method: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What applying [`Script::viewport`] did. It happens before step one and is
/// reported before it, as the CLI's line 0.
#[derive(Debug, Clone, Serialize)]
pub struct ViewportOutcome {
    /// `applied` or `failed`.
    pub status: &'static str,
    pub requested: Viewport,
    /// The size the window actually reached, when the app said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reached: Option<Viewport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What the whole replay did.
///
/// The counts are about the steps and always add up to `of`. The viewport
/// header is not a step: its outcome is `viewport`, and `ok` covers both — a
/// header that failed is `ok: false` with `failed: 0` and every step skipped.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ok: bool,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub of: usize,
    /// What applying [`Script::viewport`] did, when the script carried one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<ViewportOutcome>,
    pub steps: Vec<StepOutcome>,
}

/// Replay a script, calling `call` for each step.
///
/// There is no separate assertion step, because there does not need to be one:
/// a `wait_for` that comes back unsatisfied *is* a failed assertion, and it
/// already reports which of its conditions did not hold. So the same file
/// works as a way to reach a state and as a test of getting there.
pub fn replay(
    script: &Script,
    options: &ReplayOptions,
    mut call: impl FnMut(&str, serde_json::Value) -> anyhow::Result<serde_json::Value>,
) -> Report {
    let mut steps = Vec::with_capacity(script.steps.len());
    let (mut passed, mut failed, mut skipped) = (0, 0, 0);

    // Before anything else, because everything after it depends on the
    // layout: a script recorded at one size and replayed at another is not
    // replaying the same UI. A window that cannot be resized — refused as
    // well as unreachable — aborts the run rather than producing failures
    // that all say the wrong thing.
    let mut viewport = None;
    if let Some(size) = script.viewport {
        let outcome = apply_viewport(size, &mut call);
        let failed = outcome.status == "failed";
        viewport = Some(outcome);
        if failed {
            return Report {
                ok: false,
                passed: 0,
                failed: 0,
                skipped: script.steps.len(),
                of: script.steps.len(),
                viewport,
                steps: Vec::new(),
            };
        }
    }

    for (index, step) in script.steps.iter().enumerate() {
        if options.seek && is_read_only(&step.method) {
            skipped += 1;
            steps.push(StepOutcome {
                index,
                method: step.method.clone(),
                status: "skipped",
                detail: None,
            });
            continue;
        }

        let outcome = match call(&step.method, script.params_for(step)) {
            Err(error) => Err(error.to_string()),
            Ok(value) => match unmet_expectation(&step.method, &value) {
                Some(reason) => Err(reason),
                None => Ok(passing_note(&step.method, &value)),
            },
        };

        match outcome {
            Ok(detail) => {
                passed += 1;
                steps.push(StepOutcome {
                    index,
                    method: step.method.clone(),
                    status: "passed",
                    detail,
                });
            }
            Err(detail) => {
                failed += 1;
                steps.push(StepOutcome {
                    index,
                    method: step.method.clone(),
                    status: "failed",
                    detail: Some(detail),
                });
                if options.stop_on_error {
                    break;
                }
            }
        }
    }

    Report {
        ok: failed == 0,
        passed,
        failed,
        skipped,
        of: script.steps.len(),
        viewport,
        steps,
    }
}

/// Pin the window to the script's size, and say what happened.
fn apply_viewport(
    size: Viewport,
    call: &mut impl FnMut(&str, serde_json::Value) -> anyhow::Result<serde_json::Value>,
) -> ViewportOutcome {
    match call(
        "set_viewport",
        serde_json::json!({ "width": size.width, "height": size.height }),
    ) {
        Err(error) => ViewportOutcome {
            status: "failed",
            requested: size,
            reached: None,
            detail: Some(error.to_string()),
        },
        Ok(value) => {
            let refused = refused_resize(&value);
            ViewportOutcome {
                status: if refused.is_some() {
                    "failed"
                } else {
                    "applied"
                },
                requested: size,
                reached: serde_json::from_value(value["viewport"].clone()).ok(),
                detail: refused,
            }
        }
    }
}

/// A `set_viewport` answer that says the window is not the size it was asked
/// to be. The call succeeded; the resize did not, and a script that carries
/// on is replaying a different layout.
fn refused_resize(value: &serde_json::Value) -> Option<String> {
    if value.get("honoured") != Some(&serde_json::json!(false)) {
        return None;
    }
    // Sizes arrive as JSON numbers, and `1280.0x800.0` is not how anyone says
    // a window size.
    let size = |key: &str| {
        let side = |side: &str| {
            value[key][side]
                .as_f64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into())
        };
        format!("{}x{}", side("width"), side("height"))
    };
    Some(format!(
        "asked for {} and the window reached {}: the platform refused or clamped the resize — \
         a minimum size, a maximised window, or a tiling window manager. The layout is not the \
         one the script was recorded against.",
        size("requested"),
        size("viewport")
    ))
}

/// Something a passing step should still say. A golden that was written
/// rather than compared passed without proving anything, and a report that
/// printed a bare `passed` for it would hide exactly the run that needs a
/// look.
fn passing_note(method: &str, value: &serde_json::Value) -> Option<String> {
    let not_compared = value.get("created") == Some(&serde_json::json!(true))
        || value.get("compared") == Some(&serde_json::json!(false));
    if method != crate::golden::TOOL_NAME || !not_compared {
        return None;
    }
    Some(
        value
            .get("detail")
            .and_then(|detail| detail.as_str())
            .unwrap_or("the golden was written, not compared")
            .to_string(),
    )
}

/// A step that succeeded as a call but did not say what the script expects.
fn unmet_expectation(method: &str, value: &serde_json::Value) -> Option<String> {
    match method {
        // A recorded resize that the platform refused leaves every later step
        // on a layout the script did not mean, the same as a refused header.
        "set_viewport" => refused_resize(value),
        "wait_for" if value.get("satisfied") == Some(&serde_json::json!(false)) => Some(format!(
            "waited {} ms and the condition never held: {}",
            value.get("waited_ms").unwrap_or(&serde_json::json!(0)),
            value.get("checks").unwrap_or(&serde_json::json!({}))
        )),
        "batch" if value.get("ok") == Some(&serde_json::json!(false)) => Some(format!(
            "{} of {} steps ran before one failed",
            value.get("ran").unwrap_or(&serde_json::json!(0)),
            value.get("of").unwrap_or(&serde_json::json!(0))
        )),
        // A golden screenshot is the same kind of assertion, about how the
        // window looks rather than what it contains.
        "expect_screenshot" if value.get("matched") == Some(&serde_json::json!(false)) => Some(
            value
                .get("detail")
                .and_then(|detail| detail.as_str())
                .unwrap_or("the window does not match its golden image")
                .to_string(),
        ),
        // An audit step in a script is an assertion about the UI, the same way
        // a wait is an assertion about its state.
        "a11y_audit" if value.get("ok") == Some(&serde_json::json!(false)) => Some(format!(
            "{} serious and {} warnings, at fail_on={}",
            value.get("serious").unwrap_or(&serde_json::json!(0)),
            value.get("warnings").unwrap_or(&serde_json::json!(0)),
            value
                .get("fail_on")
                .unwrap_or(&serde_json::json!("serious"))
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gpui-mcp-test-{}-{}.json",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn a_snapshot_line_yields_its_ref_and_id() {
        assert_eq!(
            parse_snapshot_line("  - button \"Save\" #save-button @e6"),
            Some(("e6", "save-button"))
        );
        assert_eq!(
            parse_snapshot_line("- textbox #search @e1"),
            Some(("e1", "search"))
        );
    }

    /// A line without an id has no stable name to record, and a `#` inside a
    /// name is not one either — recording `3"` would produce a selector that
    /// matches nothing.
    #[test]
    fn a_line_without_a_usable_id_yields_nothing() {
        assert_eq!(parse_snapshot_line("- listitem \"Accordion\" @e3"), None);
        assert_eq!(parse_snapshot_line("- button \"Item #3\" @e4"), None);
        assert_eq!(parse_snapshot_line("ui_snapshot 1 — WindowId(1)"), None);
        assert_eq!(parse_snapshot_line(""), None);
    }

    /// The whole point of recording: `@e6` means "line six of what I am
    /// looking at", which is worthless in a file. It has to become a name.
    #[test]
    fn a_recorded_ref_becomes_the_id_beside_it() {
        let path = temp_path("refs");
        let mut recorder = Recorder::new(&path, Some("story".into()));

        recorder
            .record(
                "ui_snapshot",
                &json!({}),
                Some("- button \"Save\" #save-button @e6\n- textbox #search @e7\n"),
            )
            .unwrap();
        recorder
            .record("click_element", &json!({ "element_id": "@e6" }), None)
            .unwrap();
        recorder
            .record("type_text", &json!({ "text": "hello" }), None)
            .unwrap();

        let written = Script::read(&path).unwrap();
        assert_eq!(written.steps.len(), 3);
        assert_eq!(written.steps[1].params["element_id"], "save-button");
        assert!(written.steps[1].note.is_none());
        assert_eq!(written.steps[2].params["text"], "hello");
        assert_eq!(written.app.as_deref(), Some("story"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_ref_with_no_id_beside_it_is_recorded_with_a_warning() {
        let path = temp_path("noid");
        let mut recorder = Recorder::new(&path, None);

        recorder
            .record(
                "ui_snapshot",
                &json!({}),
                Some("- listitem \"Accordion\" @e3\n"),
            )
            .unwrap();
        recorder
            .record("click_element", &json!({ "element_id": "@e3" }), None)
            .unwrap();

        let written = Script::read(&path).unwrap();
        assert_eq!(written.steps[1].params["element_id"], "@e3");
        let note = written.steps[1].note.as_deref().expect("a warning");
        assert!(note.contains("@e3"), "{note}");
        assert!(note.contains("id in the app"), "{note}");

        std::fs::remove_file(&path).ok();
    }

    /// Rewriting `@e3` to `#item` when forty lines say `#item` would produce a
    /// script that replays cleanly and clicks the wrong thing.
    #[test]
    fn a_ref_to_a_repeated_id_is_not_rewritten() {
        let path = temp_path("ambiguous");
        let mut recorder = Recorder::new(&path, None);

        recorder
            .record(
                "ui_snapshot",
                &json!({}),
                Some(
                    "- listitem \"One\" #item @e1\n\
                     - listitem \"Two\" #item @e2\n\
                     - button \"Save\" #save @e3\n",
                ),
            )
            .unwrap();
        recorder
            .record("click_element", &json!({ "element_id": "@e2" }), None)
            .unwrap();
        recorder
            .record("click_element", &json!({ "element_id": "@e3" }), None)
            .unwrap();

        let written = Script::read(&path).unwrap();
        assert_eq!(written.steps[1].params["element_id"], "@e2");
        let note = written.steps[1].note.as_deref().expect("a warning");
        assert!(note.contains("#item"), "{note}");
        assert!(note.contains("more than one line"), "{note}");

        // The unique one is still rewritten.
        assert_eq!(written.steps[2].params["element_id"], "save");
        assert!(written.steps[2].note.is_none());

        std::fs::remove_file(&path).ok();
    }

    fn script_of(steps: &[(&str, serde_json::Value)]) -> Script {
        Script {
            source: None,
            name: "test".into(),
            app: None,
            recorded_with: None,
            viewport: None,
            steps: steps
                .iter()
                .map(|(method, params)| Step {
                    method: (*method).to_string(),
                    params: params.clone(),
                    note: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_replay_reports_every_step() {
        let script = script_of(&[
            ("click_element", json!({ "element_id": "save" })),
            ("wait_for", json!({ "text": "Saved" })),
        ]);

        let report = replay(&script, &ReplayOptions::default(), |method, _params| {
            Ok(match method {
                "wait_for" => json!({ "satisfied": true }),
                _ => json!({ "success": true }),
            })
        });

        assert!(report.ok);
        assert_eq!((report.passed, report.failed, report.skipped), (2, 0, 0));
    }

    /// A `wait_for` that never came true is a failed assertion. That is the
    /// whole test story: no separate step type, and the failure already says
    /// which condition did not hold.
    #[test]
    fn an_unsatisfied_wait_fails_the_replay() {
        let script = script_of(&[
            ("wait_for", json!({ "text": "Saved" })),
            ("send_key", json!({ "key": "enter" })),
        ]);

        let report = replay(
            &script,
            &ReplayOptions {
                seek: false,
                stop_on_error: true,
            },
            |_, _| {
                Ok(json!({
                    "satisfied": false,
                    "waited_ms": 3000,
                    "checks": { "text": { "found": false } }
                }))
            },
        );

        assert!(!report.ok);
        assert_eq!(report.failed, 1);
        assert_eq!(report.steps.len(), 1, "stopped at the failure");
        let detail = report.steps[0].detail.as_deref().unwrap();
        assert!(detail.contains("3000"), "{detail}");
        assert!(detail.contains("found"), "{detail}");
    }

    #[test]
    fn keeping_going_runs_the_rest() {
        let script = script_of(&[
            ("wait_for", json!({ "text": "nope" })),
            ("send_key", json!({ "key": "enter" })),
        ]);

        let report = replay(
            &script,
            &ReplayOptions {
                seek: false,
                stop_on_error: false,
            },
            |method, _| {
                Ok(match method {
                    "wait_for" => json!({ "satisfied": false }),
                    _ => json!({ "success": true }),
                })
            },
        );

        assert_eq!((report.passed, report.failed), (1, 1));
        assert_eq!(report.steps.len(), 2);
    }

    #[test]
    fn the_guide_and_replay_are_not_recorded() {
        let path = temp_path("skip");
        let mut recorder = Recorder::new(&path, None);

        recorder.record("gpui_guide", &json!({}), None).unwrap();
        recorder
            .record("replay_script", &json!({ "path": "x.json" }), None)
            .unwrap();
        recorder
            .record("send_key", &json!({ "key": "enter" }), None)
            .unwrap();

        assert_eq!(recorder.steps(), 1);
        std::fs::remove_file(&path).ok();
    }

    /// Seeking is about arriving, not about checking the way there.
    #[test]
    fn seeking_skips_the_steps_that_only_look() {
        let script = script_of(&[
            ("ui_snapshot", json!({})),
            ("inspect_ui_tree", json!({})),
            ("take_screenshot", json!({})),
            ("click_element", json!({ "element_id": "save" })),
        ]);

        let mut called = Vec::new();
        let report = replay(
            &script,
            &ReplayOptions {
                seek: true,
                stop_on_error: true,
            },
            |method, _| {
                called.push(method.to_string());
                Ok(json!({ "success": true }))
            },
        );

        assert_eq!(report.skipped, 2, "the tree and the screenshot");
        assert_eq!(called, ["ui_snapshot", "click_element"]);
    }

    /// A snapshot is not skipped: it hands out the refs a later step may need.
    #[test]
    fn a_snapshot_is_not_a_read_only_step() {
        assert!(!is_read_only("ui_snapshot"));
        assert!(is_read_only("inspect_ui_tree"));
        assert!(is_read_only("take_screenshot"));
        assert!(is_read_only("a11y_audit"), "seeking does not audit");
        assert!(!is_read_only("click_element"));
    }

    /// An audit step is an assertion about the UI, the same way a wait is one
    /// about its state — so accessibility stays checked rather than having
    /// been checked once.
    #[test]
    fn a_failing_audit_fails_the_replay() {
        let script = script_of(&[("a11y_audit", json!({ "fail_on": "serious" }))]);

        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Ok(json!({ "ok": false, "serious": 3, "warnings": 7, "fail_on": "serious" }))
        });

        assert!(!report.ok);
        let detail = report.steps[0].detail.as_deref().unwrap();
        assert!(detail.contains('3'), "{detail}");
        assert!(detail.contains('7'), "{detail}");
    }

    #[test]
    fn an_audit_that_passes_passes() {
        let script = script_of(&[("a11y_audit", json!({}))]);
        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Ok(json!({ "ok": true, "serious": 0, "warnings": 4 }))
        });
        assert!(report.ok);
    }

    #[test]
    fn a_failed_call_fails_the_step() {
        let script = script_of(&[("click_element", json!({ "element_id": "nope" }))]);
        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Err(anyhow::anyhow!("Element not found: nope"))
        });

        assert!(!report.ok);
        assert!(report.steps[0].detail.as_deref().unwrap().contains("nope"));
    }

    /// The gap the ref machinery left open: a step that names `#item` itself
    /// never went through a ref, so nothing looked at how many `#item`s there
    /// are. Record time is when that is still cheap to fix.
    #[test]
    fn an_id_a_step_names_itself_is_checked_too() {
        let path = temp_path("literal-ambiguous");
        let mut recorder = Recorder::new(&path, None);

        recorder
            .record(
                "ui_snapshot",
                &json!({}),
                Some(
                    "- listitem \"One\" #item @e1\n\
                     - listitem \"Two\" #item @e2\n\
                     - button \"Save\" #save @e3\n",
                ),
            )
            .unwrap();
        recorder
            .record("click_element", &json!({ "element_id": "#item" }), None)
            .unwrap();
        recorder
            .record("wait_for", &json!({ "element_id": "save" }), None)
            .unwrap();

        let written = Script::read(&path).unwrap();
        let note = written.steps[1].note.as_deref().expect("a warning");
        assert!(note.contains("#item names 2"), "{note}");
        assert!(written.steps[2].note.is_none(), "the unique one is fine");

        std::fs::remove_file(&path).ok();
    }

    /// `#input-4294967299` passes every test for a hand-written id and is not
    /// one. Nothing else in the recorder can tell the difference, so the note
    /// is the only thing standing between it and a script that breaks on the
    /// next app start.
    #[test]
    fn an_id_carrying_an_entity_number_is_recorded_with_a_warning() {
        let path = temp_path("generated-id");
        let mut recorder = Recorder::new(&path, None);

        recorder
            .record(
                "ui_snapshot",
                &json!({}),
                Some("- textbox #input-4294967299 @e1\n"),
            )
            .unwrap();
        recorder
            .record("click_element", &json!({ "element_id": "@e1" }), None)
            .unwrap();

        let written = Script::read(&path).unwrap();
        // Still rewritten: the id at least says which element was meant, and
        // the ref says only which line it was on.
        assert_eq!(written.steps[1].params["element_id"], "input-4294967299");
        let note = written.steps[1].note.as_deref().expect("a warning");
        assert!(note.contains("generates fresh"), "{note}");

        std::fs::remove_file(&path).ok();
    }

    /// The size a window is at decides its layout, so it has to be set before
    /// the first step rather than somewhere among them.
    #[test]
    fn a_viewport_is_applied_before_the_first_step() {
        let mut script = script_of(&[("click_element", json!({ "element_id": "save" }))]);
        script.viewport = Some(Viewport {
            width: 1280.0,
            height: 800.0,
        });

        let mut called = Vec::new();
        let report = replay(&script, &ReplayOptions::default(), |method, params| {
            called.push((method.to_string(), params));
            Ok(json!({ "success": true, "honoured": true }))
        });

        assert!(report.ok);
        assert_eq!(called[0].0, "set_viewport");
        assert_eq!(called[0].1["width"], 1280.0);
        assert_eq!(called[1].0, "click_element");
        assert!(report.viewport.is_some(), "the report says what it did");
    }

    /// Every later step would be acting on the wrong layout, so its failures
    /// would all describe the wrong problem.
    #[test]
    fn a_window_that_cannot_be_resized_stops_the_replay() {
        let mut script = script_of(&[("click_element", json!({ "element_id": "save" }))]);
        script.viewport = Some(Viewport {
            width: 1280.0,
            height: 800.0,
        });

        let mut called = Vec::new();
        let report = replay(&script, &ReplayOptions::default(), |method, _| {
            called.push(method.to_string());
            Err(anyhow::anyhow!("Window not found"))
        });

        assert!(!report.ok);
        assert_eq!(called, ["set_viewport"], "nothing else ran");
        assert_eq!(report.skipped, 1);
    }

    /// A script with no viewport is the older shape, and still replays.
    #[test]
    fn a_script_without_a_viewport_just_runs() {
        let script = script_of(&[("send_key", json!({ "key": "enter" }))]);

        let mut called = Vec::new();
        let report = replay(&script, &ReplayOptions::default(), |method, _| {
            called.push(method.to_string());
            Ok(json!({ "success": true }))
        });

        assert!(report.ok);
        assert_eq!(called, ["send_key"]);
        assert!(report.viewport.is_none());
    }

    /// A golden that no longer matches is an assertion that failed, and the
    /// replay has to say so with the detail the comparison produced.
    #[test]
    fn a_mismatched_golden_fails_the_replay() {
        let script = script_of(&[(
            "expect_screenshot",
            json!({ "path": "tests/golden/sidebar.png" }),
        )]);

        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Ok(json!({
                "matched": false,
                "detail": "42 of 1000 pixels differ; wrote sidebar.actual.png",
            }))
        });

        assert!(!report.ok);
        let detail = report.steps[0].detail.as_deref().unwrap();
        assert!(detail.contains("actual.png"), "{detail}");
    }

    /// The first run of a golden has nothing to compare against and passes —
    /// but a bare "passed" would hide exactly the run that needs a look, so
    /// the step carries what the comparison said.
    #[test]
    fn a_freshly_written_golden_passes_and_says_so() {
        let script = script_of(&[("expect_screenshot", json!({ "path": "g.png" }))]);
        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Ok(json!({ "matched": true, "created": true, "detail": "Wrote g.png" }))
        });
        assert!(report.ok);
        assert_eq!(report.steps[0].status, "passed");
        assert_eq!(report.steps[0].detail.as_deref(), Some("Wrote g.png"));
    }

    #[test]
    fn a_compared_golden_that_matches_says_nothing() {
        let script = script_of(&[("expect_screenshot", json!({ "path": "g.png" }))]);
        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Ok(json!({ "matched": true, "created": false, "compared": true }))
        });
        assert!(report.ok);
        assert!(report.steps[0].detail.is_none());
    }

    /// Seeking is about arriving. A golden is an assertion about the way
    /// there, which is exactly what seeking skips.
    #[test]
    fn seeking_does_not_compare_goldens() {
        assert!(is_read_only("expect_screenshot"));
    }

    /// The call succeeded and the resize did not. Carrying on would replay
    /// every later step against a layout the script never meant, so this is
    /// the same abort as a window that could not be reached at all.
    #[test]
    fn a_refused_resize_stops_the_replay() {
        let mut script = script_of(&[("click_element", json!({ "element_id": "save" }))]);
        script.viewport = Some(Viewport {
            width: 1280.0,
            height: 800.0,
        });

        let mut called = Vec::new();
        let report = replay(&script, &ReplayOptions::default(), |method, _| {
            called.push(method.to_string());
            Ok(json!({
                "honoured": false,
                "requested": { "width": 1280.0, "height": 800.0 },
                "viewport": { "width": 1920.0, "height": 1080.0 },
            }))
        });

        assert!(!report.ok);
        assert_eq!(called, ["set_viewport"], "nothing else ran");
        let viewport = report.viewport.as_ref().unwrap();
        assert_eq!(viewport.status, "failed");
        assert_eq!(
            viewport.reached,
            Some(Viewport {
                width: 1920.0,
                height: 1080.0
            })
        );
        let detail = viewport.detail.as_deref().unwrap();
        assert!(detail.contains("1280x800"), "{detail}");
        assert!(detail.contains("1920x1080"), "{detail}");
        assert!(detail.contains("refused"), "{detail}");
    }

    /// The header is not a step, so a header that fails must not be counted
    /// as one: the counts describe the steps and add up to `of`, and the
    /// reason lives in the report where a reader can find it.
    #[test]
    fn a_failed_header_keeps_the_counts_honest() {
        let mut script = script_of(&[
            ("click_element", json!({ "element_id": "save" })),
            ("wait_for", json!({ "text": "Saved" })),
        ]);
        script.viewport = Some(Viewport {
            width: 1280.0,
            height: 800.0,
        });

        let report = replay(&script, &ReplayOptions::default(), |_, _| {
            Err(anyhow::anyhow!("Window not found"))
        });

        assert!(!report.ok);
        assert_eq!(
            report.passed + report.failed + report.skipped,
            report.of,
            "{report:?}"
        );
        assert_eq!(report.failed, 0, "the header is not a step");
        assert_eq!(report.skipped, 2);
        let viewport = report.viewport.as_ref().unwrap();
        assert_eq!(viewport.status, "failed");
        assert!(viewport.reached.is_none());
        assert!(viewport
            .detail
            .as_deref()
            .unwrap()
            .contains("Window not found"));
    }

    /// A resize recorded as a step can be refused just like the header.
    #[test]
    fn a_recorded_resize_the_platform_refused_fails_the_step() {
        let script = script_of(&[
            ("set_viewport", json!({ "width": 600, "height": 400 })),
            ("click_element", json!({ "element_id": "save" })),
        ]);

        let report = replay(&script, &ReplayOptions::default(), |method, _| {
            Ok(match method {
                "set_viewport" => json!({
                    "honoured": false,
                    "requested": { "width": 600.0, "height": 400.0 },
                    "viewport": { "width": 800.0, "height": 600.0 },
                }),
                _ => json!({ "success": true }),
            })
        });

        assert!(!report.ok);
        assert_eq!(report.steps[0].status, "failed");
        assert_eq!(report.steps.len(), 1, "stopped there");
    }

    /// A golden named in a script is relative to the script, so the same
    /// file resolves from whichever directory the replay is started in.
    #[test]
    fn a_golden_path_is_resolved_against_the_script() {
        let mut script = script_of(&[
            ("expect_screenshot", json!({ "path": "golden/sidebar.png" })),
            ("expect_screenshot", json!({ "path": "/abs/elsewhere.png" })),
            (
                "click_element",
                json!({ "element_id": "golden/sidebar.png" }),
            ),
        ]);
        script.source = Some(PathBuf::from("tests/open-file.json"));

        let mut seen = Vec::new();
        let report = replay(&script, &ReplayOptions::default(), |method, params| {
            seen.push((method.to_string(), params));
            Ok(json!({ "matched": true, "compared": true }))
        });

        assert!(report.ok);
        assert_eq!(
            Path::new(seen[0].1["path"].as_str().unwrap()),
            Path::new("tests/golden/sidebar.png")
        );
        assert_eq!(seen[1].1["path"], "/abs/elsewhere.png", "absolute stays");
        assert_eq!(
            seen[2].1["element_id"], "golden/sidebar.png",
            "only a golden's path is a path"
        );
    }

    /// A script that was never read from a file has nothing to resolve
    /// against, and leaves the path alone.
    #[test]
    fn a_script_without_a_source_leaves_golden_paths_alone() {
        let script = script_of(&[("expect_screenshot", json!({ "path": "golden/s.png" }))]);
        let mut seen = None;
        replay(&script, &ReplayOptions::default(), |_, params| {
            seen = Some(params);
            Ok(json!({ "matched": true }))
        });
        assert_eq!(seen.unwrap()["path"], "golden/s.png");
    }

    #[test]
    fn a_relative_path_walks_from_the_base_to_the_target() {
        let (base, target) = if cfg!(windows) {
            ("C:\\repo\\tests", "C:\\repo\\tests\\golden\\s.png")
        } else {
            ("/repo/tests", "/repo/tests/golden/s.png")
        };
        assert_eq!(
            relative_path(Path::new(base), Path::new(target)),
            Some(PathBuf::from("golden").join("s.png"))
        );

        let (base, target) = if cfg!(windows) {
            (
                "C:\\repo\\scripts\\.",
                "C:\\repo\\tests\\..\\tests\\golden\\s.png",
            )
        } else {
            ("/repo/scripts/.", "/repo/tests/../tests/golden/s.png")
        };
        assert_eq!(
            relative_path(Path::new(base), Path::new(target)),
            Some(
                PathBuf::from("..")
                    .join("tests")
                    .join("golden")
                    .join("s.png")
            )
        );
    }

    /// Two drives have no path between them; the absolute path is what is
    /// left, and writing it is better than writing something that is wrong.
    #[cfg(windows)]
    #[test]
    fn a_path_across_drives_has_no_relative_form() {
        assert_eq!(
            relative_path(Path::new("C:\\repo"), Path::new("D:\\golden\\s.png")),
            None
        );
    }

    /// The agent names a golden relative to wherever the MCP client started
    /// the server, and that directory is written down nowhere. The recorder
    /// re-anchors it on the script, which is where the golden is looked for.
    #[test]
    fn a_recorded_golden_path_is_anchored_on_the_script() {
        let path = temp_path("golden-anchor");
        let mut recorder = Recorder::new(&path, None);

        recorder
            .record(
                "expect_screenshot",
                &json!({ "path": "tests/golden/sidebar.png" }),
                None,
            )
            .unwrap();

        let written = Script::read(&path).unwrap();
        let recorded = PathBuf::from(written.steps[0].params["path"].as_str().unwrap());
        let meant = normalise(
            &std::env::current_dir()
                .unwrap()
                .join("tests/golden/sidebar.png"),
        );

        if recorded.is_relative() {
            // The spelling the recorder anchored on: on macOS the temp
            // directory is reached through a symlink (`/var` is
            // `/private/var`), and the relative path climbs from the real one.
            let script_dir = std::fs::canonicalize(path.parent().unwrap()).unwrap();
            assert_eq!(normalise(&script_dir.join(&recorded)), meant);
        } else {
            // The temp directory and the working directory are on different
            // drives, so an absolute path was the only honest choice.
            assert_eq!(normalise(&recorded), meant);
        }

        std::fs::remove_file(&path).ok();
    }

    /// One probe. A window that is not there on the first recorded step is
    /// not worth a discovery and a round trip on every step after it.
    #[test]
    fn the_window_size_is_asked_for_once() {
        let path = temp_path("probe");
        let mut recorder = Recorder::new(&path, None);

        assert!(recorder.needs_viewport());
        assert!(!recorder.note_viewport(None), "nothing to write");
        assert!(!recorder.needs_viewport(), "and no second probe");

        let mut recorder = Recorder::new(&path, None);
        assert!(
            !recorder.note_viewport(Some((0.0, 0.0))),
            "a minimised window has no size"
        );
        assert!(!recorder.needs_viewport());

        let mut recorder = Recorder::new(&path, None);
        assert!(recorder.note_viewport(Some((1280.0, 800.0))));
        recorder
            .record("send_key", &json!({ "key": "enter" }), None)
            .unwrap();
        let written = Script::read(&path).unwrap();
        assert_eq!(
            written.viewport,
            Some(Viewport {
                width: 1280.0,
                height: 800.0
            })
        );

        std::fs::remove_file(&path).ok();
    }
}
