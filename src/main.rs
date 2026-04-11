use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

use gpui_mcp_protocol::protocol::*;

/// MCP Server for GPUI inspection and automation.
///
/// Communicates over stdio with Claude (MCP Protocol)
/// Communicates over Unix Domain Socket with the GPUI App.
///
/// Holds no cached socket path: every tool call re-resolves via
/// `resolve_socket_path()` so the server survives GPUI app restarts
/// (which change the PID and therefore the socket filename).
struct GpuiMcpServer;

/// One discovered GPUI app instance reachable via its MCP socket.
#[derive(Debug, Clone)]
struct Instance {
    app_name: String,
    pid: u32,
    path: String,
    mtime: SystemTime,
}

/// Parse a gpui-mcp socket filename into `(app_name, pid)`.
///
/// Expected format: `gpui-mcp-{app_name}-{pid}.sock`. The app name may itself
/// contain `-`, so we split on the *last* `-` to isolate the numeric PID.
/// Returns `None` for any filename that doesn't match or has a non-numeric PID.
fn parse_socket_name(name: &str) -> Option<(String, u32)> {
    let middle = name.strip_prefix("gpui-mcp-")?.strip_suffix(".sock")?;
    let last_dash = middle.rfind('-')?;
    let (app, pid_part) = middle.split_at(last_dash);
    // pid_part starts with '-'; skip it
    let pid: u32 = pid_part.get(1..)?.parse().ok()?;
    if app.is_empty() {
        return None;
    }
    Some((app.to_string(), pid))
}

/// Discover running GPUI MCP instances by scanning `temp_dir` for sockets.
///
/// If `app_filter` is `Some`, only instances with that app name are returned.
/// The list is sorted by mtime, newest first. Stale (non-connectable) sockets
/// are removed as a side effect.
fn discover_instances(app_filter: Option<&str>) -> Vec<Instance> {
    let temp_dir = std::env::temp_dir();
    let mut instances = Vec::new();

    let Ok(entries) = std::fs::read_dir(&temp_dir) else {
        return instances;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        let Some((app_name, pid)) = parse_socket_name(&name_str) else {
            continue;
        };

        if let Some(filter) = app_filter {
            if app_name != filter {
                continue;
            }
        }

        let path = entry.path().to_string_lossy().into_owned();

        // Drop stale sockets that nothing is listening on.
        if UnixStream::connect(&path).is_err() {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }

        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        instances.push(Instance {
            app_name,
            pid,
            path,
            mtime,
        });
    }

    instances.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    instances
}

/// Resolve which socket to connect to.
///
/// Priority:
/// 1. `GPUI_MCP_APP` + `GPUI_MCP_PID` → exact path (no discovery)
/// 2. `GPUI_MCP_APP` alone → discover filtered by app, pick newest
/// 3. neither → discover all apps, pick newest, warn on ambiguity
fn resolve_socket_path() -> Result<String> {
    let app = std::env::var("GPUI_MCP_APP").ok();
    let pid = std::env::var("GPUI_MCP_PID").ok();

    if let (Some(app), Some(pid)) = (app.as_deref(), pid.as_deref()) {
        let path = std::env::temp_dir()
            .join(format!("gpui-mcp-{}-{}.sock", app, pid))
            .to_string_lossy()
            .into_owned();
        return Ok(path);
    }

    if pid.is_some() && app.is_none() {
        return Err(anyhow::anyhow!(
            "GPUI_MCP_PID requires GPUI_MCP_APP to be set as well \
             (socket names now include the app name)."
        ));
    }

    let instances = discover_instances(app.as_deref());

    match instances.len() {
        0 => {
            let scope = match app.as_deref() {
                Some(a) => format!(" for app '{}'", a),
                None => String::new(),
            };
            Err(anyhow::anyhow!(
                "No running GPUI app found{}. Scanned: {}",
                scope,
                std::env::temp_dir().display()
            ))
        }
        1 => Ok(instances.into_iter().next().unwrap().path),
        _ => {
            let chosen = instances[0].clone();
            let scope = app
                .as_deref()
                .map(|a| format!(" of '{}'", a))
                .unwrap_or_default();
            eprintln!(
                "[MCP] Found {} instances{}. Connecting to newest: app='{}', pid={}.",
                instances.len(),
                scope,
                chosen.app_name,
                chosen.pid
            );
            eprintln!("[MCP] Other instances:");
            for inst in instances.iter().skip(1) {
                eprintln!(
                    "[MCP]   - app='{}', pid={}, path={}",
                    inst.app_name, inst.pid, inst.path
                );
            }
            eprintln!(
                "[MCP] Set GPUI_MCP_APP (and optionally GPUI_MCP_PID) to target a specific one."
            );
            Ok(chosen.path)
        }
    }
}

impl GpuiMcpServer {
    fn new() -> Self {
        Self
    }

    /// Resolve the current socket path and open a connection.
    ///
    /// Re-resolves every call so a GPUI app restart (new PID = new socket
    /// filename) is transparently picked up. `discover_instances` inside
    /// `resolve_socket_path` also removes stale sockets as a side effect.
    fn connect(&self) -> Result<UnixStream> {
        let path = resolve_socket_path()?;
        UnixStream::connect(&path)
            .with_context(|| format!("GPUI app not running or not reachable at {}", path))
    }

    fn send_ipc_request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let mut stream = self.connect()?;

        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            method: method.to_string(),
            params,
        };

        let request_json = serde_json::to_string(&request)?;
        stream.write_all(request_json.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut response_buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stream.read_exact(&mut byte)?;
            if byte[0] == b'\n' {
                break;
            }
            response_buf.push(byte[0]);
        }

        let response: IpcResponse = serde_json::from_slice(&response_buf)?;

        match response.result {
            Ok(value) => Ok(value),
            Err(error) => Err(anyhow::anyhow!("GPUI app error: {}", error)),
        }
    }

    fn handle_tool_call(&self, tool_name: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
        match tool_name {
            "inspect_ui_tree" => {
                self.send_ipc_request(methods::INSPECT_UI_TREE, arguments)
            }
            "get_element" => {
                self.send_ipc_request(methods::GET_ELEMENT, arguments)
            }
            "get_windows" => {
                self.send_ipc_request(methods::GET_WINDOWS, json!({}))
            }
            "take_screenshot" => {
                let result = self.send_ipc_request(methods::TAKE_SCREENSHOT, arguments)?;

                // The IPC response contains a file path to the PNG screenshot.
                // Read it, encode as base64, and clean up the temp file.
                let path = result["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Screenshot response missing 'path'"))?;

                let png_data = std::fs::read(path)
                    .with_context(|| format!("Failed to read screenshot file: {}", path))?;

                // Clean up temp file
                let _ = std::fs::remove_file(path);

                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);

                Ok(json!({
                    "width": result["width"],
                    "height": result["height"],
                    "format": "png",
                    "data": b64,
                    "encoding": "base64",
                }))
            }
            "click_element" => {
                self.send_ipc_request(methods::CLICK_ELEMENT, arguments)
            }
            "send_key" => {
                self.send_ipc_request(methods::SEND_KEY, arguments)
            }
            "execute_action" => {
                self.send_ipc_request(methods::EXECUTE_ACTION, arguments)
            }
            "get_app_state" => {
                self.send_ipc_request(methods::GET_APP_STATE, json!({}))
            }
            "get_logs" => {
                self.send_ipc_request(methods::GET_LOGS, json!({}))
            }
            "list_actions" => {
                self.send_ipc_request(methods::LIST_ACTIONS, arguments)
            }
            "get_focus_info" => {
                self.send_ipc_request(methods::GET_FOCUS_INFO, arguments)
            }
            "type_text" => {
                self.send_ipc_request(methods::TYPE_TEXT, arguments)
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        }
    }
}

/// MCP Protocol Messages
#[derive(Debug, Serialize, Deserialize)]
struct McpRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct McpResponse {
    jsonrpc: String,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<McpError>,
}

#[derive(Debug, Serialize)]
struct McpError {
    code: i32,
    message: String,
}

fn send_response(stdout: &mut impl Write, response: &McpResponse) -> Result<()> {
    let response_json = serde_json::to_string(response)?;
    stdout.write_all(response_json.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn tools_list() -> serde_json::Value {
    json!({
        "tools": [
            {
                "name": "get_windows",
                "description": "List all open GPUI windows with their ID, title, bounds, and active status. Use this first to discover window IDs for other tools. Returns: [{id, title, bounds: {x,y,width,height}, is_active}]",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "inspect_ui_tree",
                "description": "Get the UI element hierarchy for debugging layout and structure. Each element has: id, element_type (derived from source file), bounds, source_location, children, properties. Use max_depth to limit tree size (default: unlimited). Use root_element_id to inspect a subtree instead of the whole app. Use format='compact' to strip verbose fields (bounds, content_mask, source_location, content_size). WARNING: Without filters this can return very large responses.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "max_depth": {
                            "type": "integer",
                            "description": "Maximum tree depth to return (0 or omit = unlimited). Use 2-3 for overview."
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Only inspect this window (from get_windows). Omit for all windows."
                        },
                        "element_type_filter": {
                            "type": "string",
                            "description": "Only return elements whose type contains this substring (case-insensitive)"
                        },
                        "root_element_id": {
                            "type": "string",
                            "description": "Start the tree at this element instead of the root. Supports full_id, global_id, or suffix match. Use to drill into a subtree without fetching the whole tree."
                        },
                        "format": {
                            "type": "string",
                            "enum": ["full", "compact"],
                            "description": "Output format. 'compact' strips bounds, content_mask, source_location, content_size — keeps id, element_type, children, properties. Default: 'full'."
                        },
                        "text_filter": {
                            "type": "string",
                            "description": "Only return elements whose text_content contains this substring (case-insensitive). Keeps matching elements and their ancestors."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "get_element",
                "description": "Get a UI element and its full subtree by ID. Supports exact full_id, global_id, or suffix match. Returns the element with all descendants, text content, bounds, source location, and properties.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "element_id": {
                            "type": "string",
                            "description": "Element ID to find. Can be full ID (WindowId(1)/view-1.panel[0]), global_id (view-1.panel), or suffix (panel)"
                        }
                    },
                    "required": ["element_id"]
                }
            },
            {
                "name": "get_focus_info",
                "description": "Get information about the currently focused element and active key contexts. Essential for debugging keyboard/focus issues. Returns: {has_focus, focus_id, window_id, key_contexts: [...]}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_id": {
                            "type": "string",
                            "description": "Window to check focus for (default: active window)"
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "list_actions",
                "description": "List all registered GPUI actions that can be dispatched via execute_action. Actions are the keyboard shortcuts and commands of the app (e.g. 'elane::CursorUp', 'elane::ToggleTerminal'). Use filter to search by name substring. Set include_bindings=true to get keybinding and context info for each action.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filter": {
                            "type": "string",
                            "description": "Filter actions by name substring (case-insensitive). E.g. 'cursor', 'toggle'"
                        },
                        "include_bindings": {
                            "type": "boolean",
                            "description": "If true, return keybinding, context, and documentation for each action instead of just names. Default: false."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "execute_action",
                "description": "Execute a named GPUI action on the focused element of a window. Actions are dispatched through the focus chain just like keyboard shortcuts. Use list_actions to find available action names. Example: {\"action\": \"elane::ToggleTerminal\"}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "description": "Full action name (e.g. 'elane::CursorUp', 'elane::ActivateItem'). Use list_actions to discover names."
                        },
                        "args": {
                            "type": "object",
                            "description": "Optional JSON arguments for the action (most actions take none)"
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Target window (default: active window)"
                        }
                    },
                    "required": ["action"]
                }
            },
            {
                "name": "send_key",
                "description": "Send a keyboard keystroke to the app. The key is dispatched to the focused element. Use GPUI key format: lowercase key name with modifier prefixes. Examples: 'a', 'enter', 'escape', 'tab', 'f1', 'up', 'down'. Modifiers via the modifiers object.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "Key name: 'a'-'z', '0'-'9', 'enter', 'escape', 'tab', 'space', 'backspace', 'delete', 'up', 'down', 'left', 'right', 'home', 'end', 'pageup', 'pagedown', 'f1'-'f12', '+', '-', etc."
                        },
                        "modifiers": {
                            "type": "object",
                            "properties": {
                                "ctrl": { "type": "boolean", "description": "Ctrl modifier" },
                                "alt": { "type": "boolean", "description": "Alt modifier" },
                                "shift": { "type": "boolean", "description": "Shift modifier" },
                                "meta": { "type": "boolean", "description": "Super/Cmd modifier" }
                            }
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Target window (default: active window)"
                        }
                    },
                    "required": ["key"]
                }
            },
            {
                "name": "click_element",
                "description": "Simulate a mouse click. Provide EITHER element_id (clicks center of that element) OR x/y pixel coordinates. Element ID supports full_id, global_id, or suffix match.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "element_id": {
                            "type": "string",
                            "description": "Click the center of this element. Supports full_id, global_id, or suffix match from inspect_ui_tree."
                        },
                        "x": { "type": "number", "description": "X coordinate in pixels (from window left edge). Used when element_id is not provided." },
                        "y": { "type": "number", "description": "Y coordinate in pixels (from window top edge). Used when element_id is not provided." },
                        "button": {
                            "type": "string",
                            "enum": ["Left", "Right", "Middle"],
                            "description": "Mouse button (default: Left)"
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Target window (default: active window)"
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "type_text",
                "description": "Type a text string into the focused element by dispatching individual keystrokes. Much more convenient than send_key for entering text in input fields and dialogs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The text to type"
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Target window (default: active window)"
                        }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "take_screenshot",
                "description": "Take a screenshot of a window or a specific element. Renders the current window content to a PNG image. Optionally crop to a specific element by ID for a focused, higher-detail view.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_id": {
                            "type": "string",
                            "description": "Window to screenshot (default: active window)"
                        },
                        "element_id": {
                            "type": "string",
                            "description": "Crop screenshot to this element's bounds. Supports full_id, global_id, or suffix match from inspect_ui_tree."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "get_app_state",
                "description": "Get a snapshot of the application state: window count, active window, and per-window bounds/titles. Quick overview without the full UI tree.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "get_logs",
                "description": "Get recent MCP-related log entries (up to 500 buffered). Useful for debugging MCP interactions and seeing results of dispatched actions/keys.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        ]
    })
}

fn main() -> Result<()> {
    let server = GpuiMcpServer::new();

    eprintln!("GPUI MCP Server starting...");

    // Informational only — the server does not cache this path. Each
    // tool call re-resolves to handle GPUI app restarts transparently.
    match resolve_socket_path() {
        Ok(path) => {
            let file_name = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            match parse_socket_name(file_name) {
                Some((app, pid)) => eprintln!(
                    "Initial resolve: app='{}', pid={}, path={}",
                    app, pid, path
                ),
                None => eprintln!("Initial resolve: {}", path),
            }
        }
        Err(err) => {
            eprintln!(
                "Initial resolve: no GPUI app running yet ({}). Will retry on each tool call.",
                err
            );
        }
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request: McpRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to parse request: {}", e);
                continue;
            }
        };

        let id = request.id.clone().unwrap_or(json!(null));

        match request.method.as_str() {
            // MCP lifecycle
            "initialize" => {
                send_response(&mut stdout, &McpResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "gpui-mcp-inspector",
                            "version": "0.2.0"
                        }
                    })),
                    error: None,
                })?;
            }

            // Client sends this after initialize — acknowledge silently
            "notifications/initialized" => {
                eprintln!("[MCP] Client initialized");
                // Notifications don't get responses
            }

            // Health check
            "ping" => {
                send_response(&mut stdout, &McpResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({})),
                    error: None,
                })?;
            }

            "tools/list" => {
                send_response(&mut stdout, &McpResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(tools_list()),
                    error: None,
                })?;
            }

            "tools/call" => {
                let params = request.params.unwrap_or(json!({}));
                let tool_name = params["name"].as_str().unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                let result = server.handle_tool_call(tool_name, arguments);

                let response = match result {
                    Ok(content) => {
                        // For screenshots, return as MCP image content
                        let mcp_content = if tool_name == "take_screenshot" {
                            if let (Some(data), Some(mime)) = (content["data"].as_str(), Some("image/png")) {
                                json!({
                                    "content": [
                                        {
                                            "type": "image",
                                            "data": data,
                                            "mimeType": mime,
                                        },
                                        {
                                            "type": "text",
                                            "text": format!("Screenshot: {}x{}", content["width"], content["height"])
                                        }
                                    ]
                                })
                            } else {
                                json!({
                                    "content": [{
                                        "type": "text",
                                        "text": serde_json::to_string_pretty(&content)?
                                    }]
                                })
                            }
                        } else {
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&content)?
                                }]
                            })
                        };

                        McpResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(mcp_content),
                            error: None,
                        }
                    }
                    Err(e) => McpResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(json!({
                            "content": [{
                                "type": "text",
                                "text": format!("Error: {}", e)
                            }],
                            "isError": true
                        })),
                        error: None,
                    },
                };

                send_response(&mut stdout, &response)?;
            }

            // Unknown methods — ignore notifications, error on requests
            method => {
                if method.starts_with("notifications/") {
                    // Notifications don't need responses
                    eprintln!("[MCP] Ignoring unknown notification: {}", method);
                } else {
                    send_response(&mut stdout, &McpResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: None,
                        error: Some(McpError {
                            code: -32601,
                            message: format!("Method not found: {}", method),
                        }),
                    })?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_name() {
        assert_eq!(
            parse_socket_name("gpui-mcp-elane-12345.sock"),
            Some(("elane".to_string(), 12345))
        );
    }

    #[test]
    fn parse_app_name_with_dashes() {
        assert_eq!(
            parse_socket_name("gpui-mcp-my-editor-42.sock"),
            Some(("my-editor".to_string(), 42))
        );
        assert_eq!(
            parse_socket_name("gpui-mcp-a-b-c-7.sock"),
            Some(("a-b-c".to_string(), 7))
        );
    }

    #[test]
    fn parse_rejects_wrong_prefix() {
        assert_eq!(parse_socket_name("other-thing-1.sock"), None);
        assert_eq!(parse_socket_name("gpui-foo-1.sock"), None);
    }

    #[test]
    fn parse_rejects_wrong_suffix() {
        assert_eq!(parse_socket_name("gpui-mcp-elane-1.txt"), None);
        assert_eq!(parse_socket_name("gpui-mcp-elane-1"), None);
    }

    #[test]
    fn parse_rejects_non_numeric_pid() {
        assert_eq!(parse_socket_name("gpui-mcp-elane-abc.sock"), None);
    }

    #[test]
    fn parse_rejects_empty_app_name() {
        // "gpui-mcp--123.sock" → middle = "-123" → last_dash at 0 → app = "" → reject
        assert_eq!(parse_socket_name("gpui-mcp--123.sock"), None);
    }

    #[test]
    fn parse_rejects_missing_pid_separator() {
        // No dash at all between app and pid: "gpui-mcp-elane.sock" → middle = "elane" → no '-'
        assert_eq!(parse_socket_name("gpui-mcp-elane.sock"), None);
    }

    #[test]
    fn parse_accepts_numeric_app_name() {
        assert_eq!(
            parse_socket_name("gpui-mcp-123-456.sock"),
            Some(("123".to_string(), 456))
        );
    }
}
