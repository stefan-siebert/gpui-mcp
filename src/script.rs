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

/// A recorded session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Script {
    /// Free-form, for whoever reads the file. Defaults to the file stem.
    #[serde(default)]
    pub name: String,
    /// The `GPUI_MCP_APP` this was recorded against, when one was set. Replay
    /// does not enforce it — it is a note for the reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_with: Option<String>,
    pub steps: Vec<Step>,
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
pub const NOT_RECORDED: &[&str] = &["gpui_guide", "replay_script"];

pub fn is_read_only(method: &str) -> bool {
    READ_ONLY.contains(&method)
}

impl Script {
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Cannot read script {}: {}", path.display(), e))?;
        let script: Script = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{} is not a gpui-mcp script: {}", path.display(), e))?;
        Ok(script)
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(self)?))?;
        Ok(())
    }
}

/// Appends every tool call to a script file as it happens.
///
/// The file is rewritten after each step rather than appended to, so an
/// interrupted session still leaves valid JSON behind — a script is small, and
/// a half-written one would be worse than a slow one.
pub struct Recorder {
    path: PathBuf,
    script: Script,
    /// `e7` -> `save-button`, learned from the last snapshot this server sent.
    /// Only ids that appeared once in that snapshot: see [`Recorder::learn_refs`].
    refs: HashMap<String, String>,
    /// `e3` -> `item`, for refs whose id appeared more than once.
    ambiguous: HashMap<String, String>,
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
            path,
            script: Script {
                name,
                app,
                recorded_with: Some(format!(
                    "{} {}",
                    env!("CARGO_PKG_NAME"),
                    env!("CARGO_PKG_VERSION")
                )),
                steps: Vec::new(),
            },
            refs: HashMap::new(),
            ambiguous: HashMap::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
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

        let (params, note) = self.resolve_refs(params.clone());
        self.script.steps.push(Step {
            method: method.to_string(),
            params,
            note,
        });

        self.script.write(&self.path)
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

        let mut seen: HashMap<&str, usize> = HashMap::new();
        let mut lines = Vec::new();
        for line in snapshot.lines() {
            let Some((reference, test_id)) = parse_snapshot_line(line) else {
                continue;
            };
            *seen.entry(test_id).or_insert(0) += 1;
            lines.push((reference, test_id));
        }

        for (reference, test_id) in lines {
            let table = match seen.get(test_id) {
                Some(1) => &mut self.refs,
                _ => &mut self.ambiguous,
            };
            table.insert(reference.to_string(), test_id.to_string());
        }
    }

    /// Replace `@e7` with the id the snapshot printed beside it.
    ///
    /// A ref means "line 7 of the snapshot I am looking at", which is true for
    /// exactly as long as that snapshot is the current one. Writing it into a
    /// file unchanged would record a number, not an intention.
    fn resolve_refs(&self, mut params: serde_json::Value) -> (serde_json::Value, Option<String>) {
        let mut note = None;

        for key in ["element_id", "root_element_id"] {
            let Some(value) = params.get(key).and_then(|value| value.as_str()) else {
                continue;
            };
            let Some(reference) = value.strip_prefix('@') else {
                continue;
            };

            match self.refs.get(reference) {
                Some(test_id) => {
                    params[key] = serde_json::json!(test_id);
                }
                None => {
                    note = Some(match self.ambiguous.get(reference) {
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
                }
            }
        }

        (params, note)
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
#[derive(Debug, Clone, Default)]
pub struct ReplayOptions {
    /// Skip the steps that only look at the app. Getting somewhere is the
    /// point; re-reading the tree on the way is pure cost.
    pub seek: bool,
    /// Stop at the first failing step. On by default for the same reason a
    /// batch does: the steps after a failure are acting on a state nobody
    /// intended.
    pub stop_on_error: bool,
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

/// What the whole replay did.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ok: bool,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub of: usize,
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

        let outcome = match call(&step.method, step.params.clone()) {
            Err(error) => Err(error.to_string()),
            Ok(value) => match unmet_expectation(&step.method, &value) {
                Some(reason) => Err(reason),
                None => Ok(()),
            },
        };

        match outcome {
            Ok(()) => {
                passed += 1;
                steps.push(StepOutcome {
                    index,
                    method: step.method.clone(),
                    status: "passed",
                    detail: None,
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
        steps,
    }
}

/// A step that succeeded as a call but did not say what the script expects.
fn unmet_expectation(method: &str, value: &serde_json::Value) -> Option<String> {
    match method {
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
            name: "test".into(),
            app: None,
            recorded_with: None,
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
}
