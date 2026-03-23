use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// IPC Protocol zwischen MCP Server und GPUI App
/// Wird über Unix Domain Socket übertragen

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub result: Result<serde_json::Value, String>,
}

/// UI Element Informationen
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiElement {
    pub id: String,
    pub element_type: String,
    pub bounds: Bounds,
    pub visible: bool,
    pub children: Vec<UiElement>,
    pub properties: HashMap<String, serde_json::Value>,
    /// Source-Location im Code, z.B. "src/button.rs:42:5"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    /// Serialisierte StyleRefinement als JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style_json: Option<String>,
    /// Content-Size des Elements (width, height)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_size: Option<(f32, f32)>,
    /// Text content painted within this element's bounds
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

/// UI Tree - komplette Hierarchie
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiTree {
    pub root: UiElement,
    pub window_count: usize,
    pub timestamp: u64,
}

/// Window Informationen
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub bounds: Bounds,
    pub is_active: bool,
    pub display_id: Option<usize>,
}

/// Screenshot mit optionalen Highlights
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screenshot {
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
    pub highlighted_elements: Vec<String>,
}

/// Click Event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickEvent {
    pub element_id: Option<String>,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default = "default_left_button")]
    pub button: MouseButton,
}

fn default_left_button() -> MouseButton {
    MouseButton::Left
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Keyboard Event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: String,
    #[serde(default)]
    pub modifiers: Modifiers,
    /// Optional window ID to target (falls back to active window, then first window)
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// MCP Tool Methods
pub mod methods {
    /// UI Inspektion
    pub const INSPECT_UI_TREE: &str = "inspect_ui_tree";
    pub const GET_ELEMENT: &str = "get_element";
    pub const GET_WINDOWS: &str = "get_windows";
    pub const TAKE_SCREENSHOT: &str = "take_screenshot";
    
    /// Automatisierung
    pub const CLICK_ELEMENT: &str = "click_element";
    pub const SEND_KEY: &str = "send_key";
    pub const EXECUTE_ACTION: &str = "execute_action";
    
    /// Text input
    pub const TYPE_TEXT: &str = "type_text";

    /// State & Debug
    pub const GET_APP_STATE: &str = "get_app_state";
    pub const GET_LOGS: &str = "get_logs";
    pub const LIST_ACTIONS: &str = "list_actions";
    pub const GET_FOCUS_INFO: &str = "get_focus_info";
}

/// Params für verschiedene Methods
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetElementParams {
    pub element_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeScreenshotParams {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectUiTreeParams {
    /// Maximum depth of the tree to return (0 = unlimited)
    #[serde(default)]
    pub max_depth: usize,
    /// Only return elements from this window
    #[serde(default)]
    pub window_id: Option<String>,
    /// Only return elements matching this type substring
    #[serde(default)]
    pub element_type_filter: Option<String>,
    /// Start the tree at this element ID instead of the root.
    #[serde(default)]
    pub root_element_id: Option<String>,
    /// Output format: "full" (default) or "compact"
    #[serde(default)]
    pub format: Option<String>,
    /// Only return elements whose text_content contains this substring (case-insensitive).
    #[serde(default)]
    pub text_filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListActionsParams {
    /// Filter actions by name substring
    #[serde(default)]
    pub filter: Option<String>,
    /// If true, include keybinding and context info for each action
    #[serde(default)]
    pub include_bindings: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeTextParams {
    /// The text string to type into the focused element
    pub text: String,
    /// Optional window ID to target (falls back to active window)
    #[serde(default)]
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetFocusInfoParams {
    #[serde(default)]
    pub window_id: Option<String>,
}
