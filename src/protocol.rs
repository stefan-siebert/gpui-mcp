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
    pub x: f32,
    pub y: f32,
    pub button: MouseButton,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Keyboard Event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: String,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
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
    
    /// State
    pub const GET_APP_STATE: &str = "get_app_state";
    pub const GET_LOGS: &str = "get_logs";
}

/// Params für verschiedene Methods
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetElementParams {
    pub element_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeScreenshotParams {
    pub highlight_elements: Vec<String>,
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteActionParams {
    pub action: String,
    pub args: serde_json::Value,
}
