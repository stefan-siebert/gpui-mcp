//! Wire types shared by the `gpui-mcp-server` binary and the in-app side
//! (`gpui_component::mcp`).
//!
//! Transport: one newline-delimited JSON [`IpcRequest`] per connection over a
//! Unix-domain socket, answered by one [`IpcResponse`] line.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A request from the MCP server to the GPUI app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    /// One of the constants in [`methods`].
    pub method: String,
    pub params: serde_json::Value,
}

/// The app's answer to an [`IpcRequest`], echoing its `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub result: Result<serde_json::Value, String>,
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
    fn ipc_response_roundtrip() {
        let ok = IpcResponse {
            id: "1".into(),
            result: Ok(serde_json::json!({"success": true})),
        };
        let err = IpcResponse {
            id: "2".into(),
            result: Err("boom".into()),
        };
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
