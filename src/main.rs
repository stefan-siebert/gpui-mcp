use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use gpui_mcp_protocol::protocol::*;

/// MCP Server für GPUI Inspektion und Automatisierung
///
/// Kommuniziert über stdio mit Claude (MCP Protocol)
/// Kommuniziert über Unix Socket mit der GPUI App
struct GpuiMcpServer {
    socket_path: String,
}

impl GpuiMcpServer {
    fn new(socket_path: String) -> Self {
        Self { socket_path }
    }

    /// Verbindet mit der GPUI App via Unix Socket
    fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket_path)
            .with_context(|| format!("Failed to connect to GPUI app at {}", self.socket_path))
    }

    /// Sendet IPC Request an GPUI App und wartet auf Response
    fn send_ipc_request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let mut stream = self.connect()?;

        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            method: method.to_string(),
            params,
        };

        // Request senden
        let request_json = serde_json::to_string(&request)?;
        stream.write_all(request_json.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        // Response lesen (bis Newline)
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

    /// Handlet ein MCP Tool Call
    fn handle_tool_call(&self, tool_name: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
        match tool_name {
            "inspect_ui_tree" => {
                let result = self.send_ipc_request(methods::INSPECT_UI_TREE, json!({}))?;
                let tree: UiTree = serde_json::from_value(result)?;
                Ok(json!({
                    "tree": tree,
                    "summary": format!("UI tree with {} windows, root has {} children",
                        tree.window_count, tree.root.children.len())
                }))
            }

            "get_element" => {
                let params: GetElementParams = serde_json::from_value(arguments)?;
                let result = self.send_ipc_request(methods::GET_ELEMENT, json!(params))?;
                Ok(result)
            }

            "get_windows" => {
                let result = self.send_ipc_request(methods::GET_WINDOWS, json!({}))?;
                let windows: Vec<WindowInfo> = serde_json::from_value(result)?;
                Ok(json!({
                    "windows": windows,
                    "count": windows.len()
                }))
            }

            "take_screenshot" => {
                let params: TakeScreenshotParams = serde_json::from_value(arguments)?;
                let result = self.send_ipc_request(methods::TAKE_SCREENSHOT, json!(params))?;
                Ok(result)
            }

            "click_element" => {
                let params: ClickEvent = serde_json::from_value(arguments)?;
                let result = self.send_ipc_request(methods::CLICK_ELEMENT, json!(params))?;
                Ok(result)
            }

            "send_key" => {
                let params: KeyEvent = serde_json::from_value(arguments)?;
                let result = self.send_ipc_request(methods::SEND_KEY, json!(params))?;
                Ok(result)
            }

            "execute_action" => {
                let params: ExecuteActionParams = serde_json::from_value(arguments)?;
                let result = self.send_ipc_request(methods::EXECUTE_ACTION, json!(params))?;
                Ok(result)
            }

            "get_app_state" => {
                let result = self.send_ipc_request(methods::GET_APP_STATE, json!({}))?;
                Ok(result)
            }

            "get_logs" => {
                let result = self.send_ipc_request(methods::GET_LOGS, json!({}))?;
                Ok(result)
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

fn main() -> Result<()> {
    // Socket Path aus Environment oder Default
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
        let request: McpRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to parse request: {}", e);
                continue;
            }
        };

        let id = request.id.clone().unwrap_or(json!(null));

        // Handle initialize
        if request.method == "initialize" {
            let response = McpResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "gpui-mcp-inspector",
                        "version": "0.1.0"
                    }
                })),
                error: None,
            };

            let response_json = serde_json::to_string(&response)?;
            stdout.write_all(response_json.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
            continue;
        }

        // Handle tools/list
        if request.method == "tools/list" {
            let response = McpResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "tools": [
                        {
                            "name": "inspect_ui_tree",
                            "description": "Get complete UI element tree with hierarchy, bounds, and properties",
                            "inputSchema": {
                                "type": "object",
                                "properties": {},
                                "required": []
                            }
                        },
                        {
                            "name": "get_element",
                            "description": "Get detailed information about a specific UI element by ID",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "element_id": {
                                        "type": "string",
                                        "description": "The unique ID of the element to inspect"
                                    }
                                },
                                "required": ["element_id"]
                            }
                        },
                        {
                            "name": "get_windows",
                            "description": "Get list of all windows with their properties",
                            "inputSchema": {
                                "type": "object",
                                "properties": {},
                                "required": []
                            }
                        },
                        {
                            "name": "take_screenshot",
                            "description": "Take a screenshot of the app, optionally highlighting specific elements",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "highlight_elements": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "Element IDs to highlight in the screenshot"
                                    },
                                    "window_id": {
                                        "type": "string",
                                        "description": "Optional window ID to screenshot"
                                    }
                                },
                                "required": []
                            }
                        },
                        {
                            "name": "click_element",
                            "description": "Simulate a mouse click on an element or coordinates",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "element_id": {
                                        "type": "string",
                                        "description": "Element ID to click (optional if x/y provided)"
                                    },
                                    "x": {
                                        "type": "number",
                                        "description": "X coordinate to click"
                                    },
                                    "y": {
                                        "type": "number",
                                        "description": "Y coordinate to click"
                                    },
                                    "button": {
                                        "type": "string",
                                        "enum": ["Left", "Right", "Middle"],
                                        "description": "Mouse button to use"
                                    }
                                },
                                "required": ["x", "y"]
                            }
                        },
                        {
                            "name": "send_key",
                            "description": "Send keyboard input to the app",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "key": {
                                        "type": "string",
                                        "description": "Key to send (e.g. 'a', 'Enter', 'Escape')"
                                    },
                                    "modifiers": {
                                        "type": "object",
                                        "properties": {
                                            "ctrl": { "type": "boolean" },
                                            "alt": { "type": "boolean" },
                                            "shift": { "type": "boolean" },
                                            "meta": { "type": "boolean" }
                                        }
                                    }
                                },
                                "required": ["key"]
                            }
                        },
                        {
                            "name": "execute_action",
                            "description": "Execute a named action/command in the app",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": {
                                        "type": "string",
                                        "description": "Action name to execute"
                                    },
                                    "args": {
                                        "type": "object",
                                        "description": "Arguments for the action"
                                    }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "get_app_state",
                            "description": "Get current application state snapshot",
                            "inputSchema": {
                                "type": "object",
                                "properties": {},
                                "required": []
                            }
                        },
                        {
                            "name": "get_logs",
                            "description": "Get recent application logs",
                            "inputSchema": {
                                "type": "object",
                                "properties": {},
                                "required": []
                            }
                        }
                    ]
                })),
                error: None,
            };

            let response_json = serde_json::to_string(&response)?;
            stdout.write_all(response_json.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
            continue;
        }

        // Handle tools/call
        if request.method == "tools/call" {
            let params = request.params.unwrap_or(json!({}));
            let tool_name = params["name"].as_str().unwrap_or("");
            let arguments = params["arguments"].clone();

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
                    result: None,
                    error: Some(McpError {
                        code: -32000,
                        message: e.to_string(),
                    }),
                },
            };

            let response_json = serde_json::to_string(&response)?;
            stdout.write_all(response_json.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }

    Ok(())
}
