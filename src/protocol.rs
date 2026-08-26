//! Wire types shared by the `gpui-mcp-server` binary and the in-app side
//! (`gpui_component::mcp`).
//!
//! Transport: one newline-delimited JSON [`IpcRequest`] per connection over a
//! Unix-domain socket, answered by one [`IpcResponse`] line.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Version of the wire format in this module.
///
/// This crate has two consumers built at different times by different
/// mechanisms: the `[lib]` is linked into the GPUI app (through
/// `gpui_component::mcp`) and rebuilt whenever the app is, while the
/// `[[bin]] gpui-mcp-server` is launched from `target/release/` by an MCP
/// client and is rebuilt by nobody. Nothing forces the two to move together,
/// so a change here can leave a fresh app talking to a months-old server.
///
/// **Bump this only when a change to the types below is not backward
/// compatible** — a new `#[serde(default)]` field is not, a renamed or
/// retyped field is. Bumping it for a compatible change costs everyone a
/// rebuild for nothing.
///
/// - **v2** — input methods answer only once the frame that shows their effect
///   has been painted, and `wait_for` / `batch` exist. The types stayed
///   compatible here, but the behaviour did not: a new server paired with an
///   old app would promise frame-synchronous answers the app does not give and
///   advertise two methods it does not know. That is worth one rebuild rather
///   than a mystery.
pub const PROTOCOL_VERSION: u32 = 2;

/// What a peer built before this handshake existed appears as: it sends no
/// version at all, and `#[serde(default)]` reads that back as zero.
pub const VERSION_UNKNOWN: u32 = 0;

/// Explain a version disagreement to whoever is reading the error, or return
/// `None` when the peer agrees with us.
///
/// `peer_label` names the half that needs rebuilding and `rebuild` is the
/// command that does it — both sides of the socket call this, and each knows
/// only the other's remedy.
pub fn version_complaint(peer: u32, peer_label: &str, rebuild: &str) -> Option<String> {
    if peer == PROTOCOL_VERSION {
        return None;
    }
    let theirs = if peer == VERSION_UNKNOWN {
        "predates the version handshake".to_string()
    } else {
        format!("speaks protocol v{peer}")
    };
    Some(format!(
        "gpui-mcp protocol mismatch: the {peer_label} {theirs}, this side speaks \
         v{PROTOCOL_VERSION}. They are built from one crate but by different \
         mechanisms, so they can drift apart. Rebuild the {peer_label}: {rebuild}"
    ))
}

/// A request from the MCP server to the GPUI app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    /// One of the constants in [`methods`].
    pub method: String,
    pub params: serde_json::Value,
    /// Sender's [`PROTOCOL_VERSION`]. Defaulted so a peer that predates the
    /// handshake deserializes as [`VERSION_UNKNOWN`] rather than failing to
    /// parse — the whole point is to produce a *diagnosis*, not a parse error.
    #[serde(default)]
    pub protocol_version: u32,
}

/// The app's answer to an [`IpcRequest`], echoing its `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub result: Result<serde_json::Value, String>,
    /// Sender's [`PROTOCOL_VERSION`]; see [`IpcRequest::protocol_version`].
    #[serde(default)]
    pub protocol_version: u32,
}

impl IpcRequest {
    /// Build a request stamped with this build's [`PROTOCOL_VERSION`].
    /// Constructing the struct literally is still possible but leaves the
    /// stamp at [`VERSION_UNKNOWN`], which reads as "peer is ancient" on the
    /// far side — prefer this.
    pub fn new(id: String, method: String, params: serde_json::Value) -> Self {
        Self {
            id,
            method,
            params,
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

impl IpcResponse {
    /// Build a response stamped with this build's [`PROTOCOL_VERSION`].
    pub fn new(id: String, result: Result<serde_json::Value, String>) -> Self {
        Self {
            id,
            result,
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

/// One node of the element tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiElement {
    pub id: String,
    pub element_type: String,
    pub bounds: Bounds,
    pub visible: bool,
    pub children: Vec<UiElement>,
    pub properties: HashMap<String, serde_json::Value>,
    /// Source location in the app's code, e.g. `src/button.rs:42:5`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    /// The element's `StyleRefinement`, serialised as JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style_json: Option<String>,
    /// Content size of the element (width, height).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_size: Option<(f32, f32)>,
    /// Text painted within this element's bounds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_content: Vec<String>,
    /// The AccessKit node id this element's accessibility node would carry.
    ///
    /// gpui derives it from the same `GlobalElementId` the element path comes
    /// from, so it is the exact join between this tree and the one
    /// [`methods::A11Y_TREE`] returns — a node records only the *leaf* of its
    /// element id and its source location, and four title-bar buttons can
    /// share both. Zero when the app side is older than this field.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub accesskit_node_id: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The complete element hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiTree {
    pub root: UiElement,
    pub window_count: usize,
    /// Unix timestamp (seconds) of when the tree was captured.
    pub timestamp: u64,
}

/// A width and a height, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    /// The outer window frame. On macOS this includes the title bar, so it is
    /// not the size layout sees — that is [`WindowInfo::content_size`].
    pub bounds: Bounds,
    pub is_active: bool,
    pub display_id: Option<usize>,
    /// The drawable area: what layout sees, what a screenshot renders, and
    /// what [`methods::SET_VIEWPORT`] sets. Absent from an older app, in which
    /// case `bounds` is the best guess available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_size: Option<Size>,
}

/// Result of [`methods::TAKE_SCREENSHOT`].
///
/// The app writes the image to a temporary file and returns its path; the MCP
/// server reads, base64-encodes and deletes the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotResult {
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// Always `"png"`.
    pub format: String,
    /// Set when the image was downscaled, so the caller can tell that pixel
    /// coordinates in it are not window coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Set when the screenshot was cropped to an element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
    /// The display's device scale factor the window was rendered at: a
    /// 1280x800 window is a 1920x1200 image at 1.5. `width` and `height` are
    /// device pixels, and this is what relates them to the logical size a
    /// viewport is given in. Absent from an older app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_factor: Option<f32>,
}

/// Params for [`methods::CLICK_ELEMENT`]: either `element_id` or `x`/`y`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickEvent {
    pub element_id: Option<String>,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default)]
    pub button: MouseButton,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Params for [`methods::SEND_KEY`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Key name in gpui notation (`a`, `enter`, `pagedown`, `f5`, ...).
    pub key: String,
    #[serde(default)]
    pub modifiers: Modifiers,
    /// Target window (falls back to the active window, then the first window).
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub meta: bool,
}

/// IPC method names. The MCP tool names are identical.
pub mod methods {
    // Inspection
    pub const UI_SNAPSHOT: &str = "ui_snapshot";
    pub const A11Y_AUDIT: &str = "a11y_audit";
    pub const A11Y_TREE: &str = "a11y_tree";
    pub const INSPECT_UI_TREE: &str = "inspect_ui_tree";
    pub const GET_ELEMENT: &str = "get_element";
    pub const GET_WINDOWS: &str = "get_windows";
    pub const TAKE_SCREENSHOT: &str = "take_screenshot";
    pub const GET_FOCUS_INFO: &str = "get_focus_info";

    // Automation
    pub const WAIT_FOR: &str = "wait_for";
    pub const BATCH: &str = "batch";
    pub const CLICK_ELEMENT: &str = "click_element";
    pub const SEND_KEY: &str = "send_key";
    pub const TYPE_TEXT: &str = "type_text";
    pub const EXECUTE_ACTION: &str = "execute_action";
    pub const LIST_ACTIONS: &str = "list_actions";

    // State & debugging
    pub const GET_APP_STATE: &str = "get_app_state";
    pub const GET_LOGS: &str = "get_logs";

    // Determinism: a script that replays the same way tomorrow
    pub const SET_VIEWPORT: &str = "set_viewport";
    pub const RESET_APP: &str = "reset_app";

    /// Every method, in the order the MCP server advertises its tools.
    pub const ALL: &[&str] = &[
        GET_WINDOWS,
        UI_SNAPSHOT,
        A11Y_AUDIT,
        A11Y_TREE,
        INSPECT_UI_TREE,
        GET_ELEMENT,
        GET_FOCUS_INFO,
        LIST_ACTIONS,
        EXECUTE_ACTION,
        SEND_KEY,
        CLICK_ELEMENT,
        TYPE_TEXT,
        WAIT_FOR,
        BATCH,
        TAKE_SCREENSHOT,
        GET_APP_STATE,
        GET_LOGS,
        SET_VIEWPORT,
        RESET_APP,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetElementParams {
    pub element_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TakeScreenshotParams {
    /// Element ids to highlight in the image (not implemented by the in-app
    /// side yet; kept for wire compatibility).
    #[serde(default)]
    pub highlight_elements: Vec<String>,
    #[serde(default)]
    pub window_id: Option<String>,
    /// If set, crop the screenshot to this element's bounds.
    #[serde(default)]
    pub element_id: Option<String>,
    /// Downscale until the image is at most this wide, in device pixels.
    /// Omitted: [`DEFAULT_SCREENSHOT_MAX_WIDTH`]. Zero: no downscaling.
    ///
    /// A full-window image at a high DPI costs an agent a large share of its
    /// context for detail it almost never needs, so shrinking is the default
    /// and keeping every pixel is the thing you ask for.
    #[serde(default)]
    pub max_width: Option<u32>,
}

/// Widest image [`methods::TAKE_SCREENSHOT`] returns unless asked otherwise.
///
/// Downscaling is the only screenshot lever that matters: an image costs an
/// agent tokens by its pixel dimensions, not by its file size, so a smaller
/// picture is a cheaper picture and the encoding is beside the point.
pub const DEFAULT_SCREENSHOT_MAX_WIDTH: u32 = 1400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteActionParams {
    pub action: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InspectUiTreeParams {
    /// Maximum depth of the tree to return (0 = unlimited).
    #[serde(default)]
    pub max_depth: usize,
    /// Only return elements from this window.
    #[serde(default)]
    pub window_id: Option<String>,
    /// Only return elements whose type contains this substring.
    #[serde(default)]
    pub element_type_filter: Option<String>,
    /// Start the tree at this element instead of the root.
    #[serde(default)]
    pub root_element_id: Option<String>,
    /// Output format: `"full"` (default) or `"compact"`.
    #[serde(default)]
    pub format: Option<String>,
    /// Only return elements whose `text_content` contains this substring
    /// (case-insensitive).
    #[serde(default)]
    pub text_filter: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListActionsParams {
    /// Filter actions by name substring.
    #[serde(default)]
    pub filter: Option<String>,
    /// Include keybinding and context info for each action.
    #[serde(default)]
    pub include_bindings: bool,
    /// Only return actions whose key-binding predicate matches the current
    /// focus chain in the target window — i.e. actions that would fire if
    /// their keybinding were pressed right now. Implies `include_bindings`.
    #[serde(default)]
    pub only_available: bool,
    /// Window to evaluate `only_available` against (default: active window).
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeTextParams {
    /// The text to type into the focused element.
    pub text: String,
    /// Target window (default: active window).
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetFocusInfoParams {
    #[serde(default)]
    pub window_id: Option<String>,
}

/// Params for [`methods::UI_SNAPSHOT`].
///
/// The snapshot is the cheap way to look at a UI: one line per element that
/// means something, with the layout scaffolding left out. The full tree from
/// [`methods::INSPECT_UI_TREE`] stays for layout debugging, where the bounds
/// and source locations are the point.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiSnapshotParams {
    #[serde(default)]
    pub window_id: Option<String>,
    /// Snapshot this element's subtree instead of the whole window.
    #[serde(default)]
    pub root_element_id: Option<String>,
    /// Keep only elements whose role, name or test id contains this
    /// (case-insensitive), along with their ancestors.
    #[serde(default)]
    pub filter: Option<String>,
    /// Keep only elements you can act on — buttons, inputs, list items and the
    /// like.
    #[serde(default)]
    pub interactive_only: bool,
    /// Stop after this many elements, saying so. Default
    /// [`DEFAULT_SNAPSHOT_ELEMENTS`].
    #[serde(default)]
    pub max_elements: Option<usize>,
    /// Add each element's bounds. Off by default: a snapshot is for structure,
    /// and coordinates are what makes the tree expensive.
    #[serde(default)]
    pub include_bounds: bool,
}

/// How many elements a snapshot returns unless asked for more.
pub const DEFAULT_SNAPSHOT_ELEMENTS: usize = 200;

/// Params for [`methods::A11Y_AUDIT`].
///
/// The audit reads the same derived layer [`methods::UI_SNAPSHOT`] prints, so
/// it can only report what that layer can see. That is less than a browser
/// accessibility checker and more than nothing: an unnamed control, an id that
/// identifies several elements, a target too small to hit. Contrast is not
/// among them — the colours never reach this side.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct A11yAuditParams {
    #[serde(default)]
    pub window_id: Option<String>,
    /// Audit this element's subtree instead of the whole window.
    #[serde(default)]
    pub root_element_id: Option<String>,
    /// Severity at which the audit reports `ok: false`: `"serious"` (default),
    /// `"warning"`, or `"none"` to always pass. A replay step fails when the
    /// audit does, which is how accessibility becomes part of a regression run
    /// rather than a thing somebody remembers to check.
    #[serde(default)]
    pub fail_on: Option<String>,
    /// Smallest acceptable side of an interactive element, in pixels.
    /// Default [`DEFAULT_MIN_TARGET_SIZE`], WCAG 2.2's minimum.
    #[serde(default)]
    pub min_target_size: Option<f32>,
    /// Stop after this many findings. Default [`DEFAULT_MAX_FINDINGS`].
    #[serde(default)]
    pub max_findings: Option<usize>,
}

/// WCAG 2.2 "Target Size (Minimum)", in pixels.
pub const DEFAULT_MIN_TARGET_SIZE: f32 = 24.0;

/// Params for [`methods::A11Y_TREE`].
///
/// The accessibility tree is what a screen reader is handed: real roles, the
/// label a control announces, an input's value, what actions it offers. GPUI
/// builds it only while an assistive technology is attached, so this method
/// turns it on for the window and waits a frame for it to appear.
///
/// It is not a replacement for [`methods::UI_SNAPSHOT`] and does not try to
/// be. Only elements somebody annotated get a node — on the gpui-component
/// gallery that is eleven of ninety-six painted elements — so the snapshot
/// remains the way to see everything, and this is the way to see what the
/// annotated ones actually announce.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct A11yTreeParams {
    /// Window to read (default: active window).
    #[serde(default)]
    pub window_id: Option<String>,
}

/// How many findings an audit returns unless asked for more.
pub const DEFAULT_MAX_FINDINGS: usize = 50;

/// Params for [`methods::SET_VIEWPORT`].
///
/// A window's size decides its layout, so a script recorded at one size and
/// replayed at another is not replaying the same UI: a sidebar collapses, a
/// toolbar overflows into a menu, and the element the script wanted is
/// somewhere else or nowhere. Pinning the size is the cheapest determinism
/// there is, which is why a recorded script carries one.
///
/// This is the *content* size in logical pixels, not the outer window: it is
/// what layout sees, and what a golden screenshot is taken of.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetViewportParams {
    pub width: f32,
    pub height: f32,
    /// Window to resize (default: active window).
    #[serde(default)]
    pub window_id: Option<String>,
}

/// Params for [`methods::RESET_APP`].
///
/// Answered by a hook the app registers, the way [`methods::GET_APP_STATE`] is
/// answered by a provider it registers. Without one the method fails and says
/// so rather than quietly doing nothing — a replay that believes it started
/// from a known state and did not is the kind of green run that hides a bug.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResetAppParams {
    /// Passed to the hook unchanged. An app that has more than one starting
    /// state can name which one it wants; most will ignore it.
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
}
/// Params for [`methods::WAIT_FOR`].
///
/// Every condition that is set must hold in the same painted frame. Setting
/// none is legal and returns as soon as one frame has been painted — the way
/// to ask for nothing but a settled frame.
///
/// This exists so an agent never has to spin: polling from the outside costs a
/// model turn per attempt, while waiting here costs one frame callback.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaitForParams {
    /// Wait until an element with this id is in the painted frame. Accepts the
    /// same full / global / suffix forms as everywhere else.
    #[serde(default)]
    pub element_id: Option<String>,
    /// Wait until this text is painted anywhere in the window
    /// (case-insensitive).
    #[serde(default)]
    pub text: Option<String>,
    /// Wait until this key context is on the focus chain (substring,
    /// case-insensitive) — how you wait for a dialog or a mode to take over
    /// the keyboard. Focus itself is reported as a `FocusHandle`, which is not
    /// an element id, so there is nothing else here to match it against.
    #[serde(default)]
    pub key_context: Option<String>,
    /// JSON pointer into the [`methods::GET_APP_STATE`] answer, e.g.
    /// `/app/rows`. Without [`Self::app_state_equals`] the condition is "this
    /// pointer resolves to something other than null".
    #[serde(default)]
    pub app_state_path: Option<String>,
    /// The value [`Self::app_state_path`] must reach.
    #[serde(default)]
    pub app_state_equals: Option<serde_json::Value>,
    /// Invert the whole predicate: wait until the conditions stop holding.
    /// This is how you wait for a dialog to close or a spinner to go away.
    #[serde(default)]
    pub absent: bool,
    /// Give up after this long, defaulting to [`DEFAULT_WAIT_MS`] and capped
    /// at [`MAX_WAIT_MS`]. Giving up is not an error: the answer says
    /// `satisfied: false`, which is a fact the caller may well have been
    /// asking for.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub window_id: Option<String>,
}

/// How long [`methods::WAIT_FOR`] waits when no timeout is given.
pub const DEFAULT_WAIT_MS: u64 = 3_000;

/// Longest wait [`methods::WAIT_FOR`] accepts. The app answers one request at
/// a time, so an unbounded wait would wedge every later call.
pub const MAX_WAIT_MS: u64 = 30_000;

/// Params for [`methods::BATCH`]: several methods in one request.
///
/// The point is turns, not milliseconds. Each step costs the agent a full
/// model round trip when sent on its own; sent together they cost one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchParams {
    /// Run in order, at most [`MAX_BATCH_STEPS`] of them.
    pub steps: Vec<BatchStep>,
    /// Stop at the first step that fails. Default `true` — a sequence usually
    /// describes one intention, and continuing past a failed click means
    /// typing into whatever happened to have focus instead.
    #[serde(default = "default_true")]
    pub stop_on_error: bool,
    /// `window_id` for steps that do not name one themselves.
    #[serde(default)]
    pub window_id: Option<String>,
}

/// One step of a [`BatchParams`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStep {
    /// Any method in [`methods::ALL`] except [`methods::BATCH`] itself.
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

fn default_true() -> bool {
    true
}

/// Most steps a single [`methods::BATCH`] may carry.
pub const MAX_BATCH_STEPS: usize = 32;

/// Deadline for a whole [`methods::BATCH`], however its steps divide it.
pub const MAX_BATCH_MS: u64 = 45_000;

/// The number of trailing digits at which an id stops looking hand-written.
///
/// gpui builds an element id out of what the app passed plus the numbers it
/// generates itself, and the two end up in the same string: an entity number
/// becomes `input-4294967299`, which is lowercase, dashed and looks every bit
/// as deliberate as `save-button`. The difference only shows tomorrow — the
/// number is fresh on every app start, so anything written down against it
/// matches nothing on the next run.
///
/// Six is where the line goes so that a counter or a year survives: `item-3`,
/// `row-42` and `since-2024` are ids people write; `input-4294967299` is not.
pub const GENERATED_ID_DIGITS: usize = 6;

/// Whether a `test_id` ends in a number the app generated rather than chose.
///
/// Both halves ask this — the audit, to report the id, and the recorder, to
/// warn before writing it into a script — so it lives here rather than being
/// guessed the same way twice.
pub fn id_looks_generated(test_id: &str) -> bool {
    test_id
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .count()
        >= GENERATED_ID_DIGITS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_event_defaults() {
        let ev: ClickEvent = serde_json::from_str(r#"{"element_id":"panel"}"#).unwrap();
        assert_eq!(ev.element_id.as_deref(), Some("panel"));
        assert_eq!(ev.button, MouseButton::Left);
        assert_eq!((ev.x, ev.y), (0.0, 0.0));
        assert!(ev.window_id.is_none());
    }

    #[test]
    fn key_event_defaults() {
        let ev: KeyEvent = serde_json::from_str(r#"{"key":"enter"}"#).unwrap();
        assert_eq!(ev.modifiers, Modifiers::default());
        assert!(ev.window_id.is_none());
    }

    #[test]
    fn a_peer_that_agrees_is_silent() {
        assert_eq!(
            version_complaint(PROTOCOL_VERSION, "server", "(cd .. && cargo build)"),
            None
        );
    }

    #[test]
    fn a_peer_predating_the_handshake_is_named_as_such() {
        let complaint = version_complaint(VERSION_UNKNOWN, "MCP server", "rebuild-me")
            .expect("an unversioned peer must be reported");
        assert!(
            complaint.contains("predates the version handshake"),
            "{complaint}"
        );
        assert!(complaint.contains("MCP server"), "{complaint}");
        assert!(complaint.contains("rebuild-me"), "{complaint}");
    }

    #[test]
    fn a_peer_on_another_version_is_named_with_its_number() {
        let complaint = version_complaint(PROTOCOL_VERSION + 7, "app", "cargo xtask run")
            .expect("a divergent peer must be reported");
        assert!(
            complaint.contains(&format!("v{}", PROTOCOL_VERSION + 7)),
            "{complaint}"
        );
        assert!(complaint.contains("cargo xtask run"), "{complaint}");
    }

    #[test]
    fn an_unstamped_peer_parses_as_version_unknown() {
        // The handshake must survive contact with a build that predates it:
        // the old wire format has no such field, and refusing to parse would
        // turn a diagnosable mismatch back into a mystery.
        let request: IpcRequest =
            serde_json::from_str(r#"{"id":"1","method":"get_windows","params":null}"#).unwrap();
        assert_eq!(request.protocol_version, VERSION_UNKNOWN);
        let response: IpcResponse =
            serde_json::from_str(r#"{"id":"1","result":{"Ok":null}}"#).unwrap();
        assert_eq!(response.protocol_version, VERSION_UNKNOWN);
    }

    #[test]
    fn constructors_stamp_the_current_version() {
        let request = IpcRequest::new(
            "1".into(),
            methods::GET_WINDOWS.into(),
            serde_json::json!({}),
        );
        assert_eq!(request.protocol_version, PROTOCOL_VERSION);
        let response = IpcResponse::new("1".into(), Ok(serde_json::json!({})));
        assert_eq!(response.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn ipc_response_roundtrip() {
        let ok = IpcResponse::new("1".into(), Ok(serde_json::json!({"success": true})));
        let err = IpcResponse::new("2".into(), Err("boom".into()));
        for r in [ok, err] {
            let json = serde_json::to_string(&r).unwrap();
            let back: IpcResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back.id, r.id);
            assert_eq!(back.result.is_ok(), r.result.is_ok());
        }
    }

    #[test]
    fn wait_for_defaults_to_nothing_but_a_settled_frame() {
        let params: WaitForParams = serde_json::from_str("{}").unwrap();
        assert!(params.element_id.is_none());
        assert!(params.text.is_none());
        assert!(!params.absent);
        assert!(params.timeout_ms.is_none());
    }

    #[test]
    fn wait_for_reads_a_full_condition() {
        let params: WaitForParams = serde_json::from_str(
            r#"{"element_id":"results","text":"Done","absent":true,"timeout_ms":500,"key_context":"Dialog",
                "app_state_path":"/app/rows","app_state_equals":12}"#,
        )
        .unwrap();
        assert_eq!(params.element_id.as_deref(), Some("results"));
        assert_eq!(params.text.as_deref(), Some("Done"));
        assert_eq!(params.key_context.as_deref(), Some("Dialog"));
        assert!(params.absent);
        assert_eq!(params.timeout_ms, Some(500));
        assert_eq!(params.app_state_path.as_deref(), Some("/app/rows"));
        assert_eq!(params.app_state_equals, Some(serde_json::json!(12)));
    }

    /// Continuing past a failed step is the dangerous default, so it must be
    /// the one you ask for.
    #[test]
    fn batch_stops_on_error_unless_told_otherwise() {
        let params: BatchParams =
            serde_json::from_str(r#"{"steps":[{"method":"send_key","params":{"key":"enter"}}]}"#)
                .unwrap();
        assert!(params.stop_on_error);
        assert_eq!(params.steps.len(), 1);
        assert_eq!(params.steps[0].method, methods::SEND_KEY);

        let params: BatchParams =
            serde_json::from_str(r#"{"steps":[],"stop_on_error":false}"#).unwrap();
        assert!(!params.stop_on_error);
    }

    #[test]
    fn batch_step_params_are_optional() {
        let step: BatchStep = serde_json::from_str(r#"{"method":"get_windows"}"#).unwrap();
        assert!(step.params.is_null());
    }

    /// An old app receiving the new fields must still parse the request, and a
    /// new app receiving an old request must still get the defaults.
    #[test]
    fn screenshot_params_stay_optional() {
        let params: TakeScreenshotParams = serde_json::from_str("{}").unwrap();
        assert!(params.max_width.is_none());

        let params: TakeScreenshotParams = serde_json::from_str(r#"{"max_width":0}"#).unwrap();
        assert_eq!(params.max_width, Some(0));
    }

    #[test]
    fn screenshot_result_scale_is_optional() {
        let result: ScreenshotResult =
            serde_json::from_str(r#"{"path":"/tmp/a.png","width":10,"height":10,"format":"png"}"#)
                .unwrap();
        assert!(result.scale.is_none());
    }

    #[test]
    fn every_method_is_listed_once() {
        let mut all = methods::ALL.to_vec();
        let before = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(before, all.len(), "a method appears twice in ALL");
        for method in [methods::WAIT_FOR, methods::BATCH] {
            assert!(methods::ALL.contains(&method), "{method} missing from ALL");
        }
    }

    #[test]
    fn ui_element_text_content_optional() {
        let json = r#"{"id":"a","element_type":"div","bounds":{"x":0,"y":0,"width":1,"height":1},
                      "visible":true,"children":[],"properties":{}}"#;
        let el: UiElement = serde_json::from_str(json).unwrap();
        assert!(el.text_content.is_empty());
    }

    /// The whole point: `input-4294967299` is lowercase and dashed, exactly
    /// like an id somebody chose, and only the digit run tells them apart.
    #[test]
    fn a_generated_id_is_told_apart_from_a_written_one() {
        assert!(id_looks_generated("input-4294967299"));
        assert!(id_looks_generated("view4294967734"));

        assert!(!id_looks_generated("save-button"));
        assert!(!id_looks_generated("item-3"));
        assert!(!id_looks_generated("row-42"));
        assert!(!id_looks_generated("since-2024"), "a year is not a handle");
        assert!(!id_looks_generated(""));
    }
}
