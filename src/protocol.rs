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
pub const PROTOCOL_VERSION: u32 = 1;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub bounds: Bounds,
    pub is_active: bool,
    pub display_id: Option<usize>,
}

/// Result of [`methods::TAKE_SCREENSHOT`].
///
/// The app writes the PNG to a temporary file and returns its path; the MCP
/// server reads, base64-encodes and deletes the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotResult {
    pub path: String,
    pub width: u32,
    pub height: u32,
    /// Always `"png"`.
    pub format: String,
    /// Set when the screenshot was cropped to an element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
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
    pub const INSPECT_UI_TREE: &str = "inspect_ui_tree";
    pub const GET_ELEMENT: &str = "get_element";
    pub const GET_WINDOWS: &str = "get_windows";
    pub const TAKE_SCREENSHOT: &str = "take_screenshot";
    pub const GET_FOCUS_INFO: &str = "get_focus_info";

    // Automation
    pub const CLICK_ELEMENT: &str = "click_element";
    pub const SEND_KEY: &str = "send_key";
    pub const TYPE_TEXT: &str = "type_text";
    pub const EXECUTE_ACTION: &str = "execute_action";
    pub const LIST_ACTIONS: &str = "list_actions";

    // State & debugging
    pub const GET_APP_STATE: &str = "get_app_state";
    pub const GET_LOGS: &str = "get_logs";

    /// Every method, in the order the MCP server advertises its tools.
    pub const ALL: &[&str] = &[
        GET_WINDOWS,
        INSPECT_UI_TREE,
        GET_ELEMENT,
        GET_FOCUS_INFO,
        LIST_ACTIONS,
        EXECUTE_ACTION,
        SEND_KEY,
        CLICK_ELEMENT,
        TYPE_TEXT,
        TAKE_SCREENSHOT,
        GET_APP_STATE,
        GET_LOGS,
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
}

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
    fn ui_element_text_content_optional() {
        let json = r#"{"id":"a","element_type":"div","bounds":{"x":0,"y":0,"width":1,"height":1},
                      "visible":true,"children":[],"properties":{}}"#;
        let el: UiElement = serde_json::from_str(json).unwrap();
        assert!(el.text_content.is_empty());
    }
}
