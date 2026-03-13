use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use gpui_mcp_protocol::protocol::*;

/// MCP Server for GPUI inspection and automation.
///
/// Communicates over stdio with Claude (MCP Protocol)
/// Communicates over Unix Socket with the GPUI App
struct GpuiMcpServer {
    socket_path: String,
}

impl GpuiMcpServer {
    fn new(socket_path: String) -> Self {
        Self { socket_path }
    }

    fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket_path)
            .with_context(|| format!("GPUI app not running or not reachable at {}", self.socket_path))
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
                self.send_ipc_request(methods::TAKE_SCREENSHOT, arguments)
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
                "description": "Get the UI element hierarchy for debugging layout and structure. Each element has: id, element_type (derived from source file), bounds, source_location, children. Use max_depth to limit tree size (default: unlimited). Use window_id to inspect a specific window. Use element_type_filter to find specific element types (e.g. 'button', 'input'). WARNING: Without filters this can return very large responses.",
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
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "get_element",
                "description": "Get details about a specific UI element by ID (from inspect_ui_tree). Supports exact full_id match, global_id match, or suffix match. Returns the element with its children, bounds, source location, and properties.",
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
                "description": "List all registered GPUI actions that can be dispatched via execute_action. Actions are the keyboard shortcuts and commands of the app (e.g. 'elane::CursorUp', 'elane::ToggleTerminal'). Use filter to search by name substring.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filter": {
                            "type": "string",
                            "description": "Filter actions by name substring (case-insensitive). E.g. 'cursor', 'toggle'"
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
                "description": "Simulate a mouse click at specific pixel coordinates in a window. Coordinates are relative to the window's content area. Use inspect_ui_tree to find element bounds for targeting.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number", "description": "X coordinate in pixels (from window left edge)" },
                        "y": { "type": "number", "description": "Y coordinate in pixels (from window top edge)" },
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
                    "required": ["x", "y"]
                }
            },
            {
                "name": "take_screenshot",
                "description": "Take a screenshot of a window. Returns base64-encoded PNG image data. Currently only supported on macOS (Metal renderer).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_id": {
                            "type": "string",
                            "description": "Window to screenshot (default: active window)"
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
    let socket_path = std::env::var("GPUI_MCP_SOCKET")
        .unwrap_or_else(|_| "/tmp/gpui-mcp.sock".to_string());

    let server = GpuiMcpServer::new(socket_path.clone());

    eprintln!("GPUI MCP Server starting...");
    eprintln!("Waiting for GPUI app at: {}", socket_path);

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
                    Ok(content) => McpResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string_pretty(&content)?
                            }]
                        })),
                        error: None,
                    },
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
