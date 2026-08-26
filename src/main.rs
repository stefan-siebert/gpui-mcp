use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

use gpui_mcp_protocol::protocol::*;

mod docs;
mod script;

use script::{Recorder, ReplayOptions, Script};

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

    instances.sort_by_key(|i| std::cmp::Reverse(i.mtime));
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

/// An image lifted out of a tool result and handed to the agent as its own
/// content block.
struct InlineImage {
    data: String,
    mime_type: String,
}

/// Turn the temp-file handoff the app returns into an image the agent can
/// actually see, and take the file with it.
///
/// Replaces the path in `result` with the image's metadata. Returns `None`
/// when the file cannot be read, leaving the path in place: an answer naming
/// a file is at least diagnosable, where a silently missing image is not.
fn inline_screenshot(result: &mut serde_json::Value) -> Option<InlineImage> {
    let shot: ScreenshotResult = serde_json::from_value(result.clone()).ok()?;

    let bytes = std::fs::read(&shot.path).ok()?;
    // The file is a temp handoff from the app; it is ours to delete.
    let _ = std::fs::remove_file(&shot.path);

    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

    *result = json!({
        "width": shot.width,
        "height": shot.height,
        "format": shot.format,
        "scale": shot.scale,
        "element_id": shot.element_id,
        "note": "The image is attached to this answer as its own content block.",
    });

    Some(InlineImage {
        data,
        mime_type: format!("image/{}", shot.format),
    })
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

    fn send_ipc_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut stream = self.connect()?;

        let request = IpcRequest::new(uuid::Uuid::new_v4().to_string(), method.to_string(), params);

        let request_json = serde_json::to_string(&request)?;
        stream.write_all(request_json.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut response_line = String::new();
        BufReader::new(&stream)
            .read_line(&mut response_line)
            .context("Failed to read IPC response")?;
        if response_line.trim().is_empty() {
            anyhow::bail!("GPUI app closed the connection without a response");
        }

        let response: IpcResponse =
            serde_json::from_str(&response_line).context("Failed to parse IPC response")?;

        // The app is the half that gets rebuilt by a normal build, so a
        // disagreement here almost always means *this* binary is the stale one
        // — but name the app's remedy too, since only its owner knows which.
        if let Some(complaint) = version_complaint(
            response.protocol_version,
            "GPUI app",
            "rebuild the app (for Elane: `cargo xtask run`); if the app is current, \
             rebuild this server with `cargo build --release` inside the gpui-mcp checkout",
        ) {
            anyhow::bail!(complaint);
        }

        match response.result {
            Ok(value) => Ok(value),
            Err(error) => Err(anyhow::anyhow!("GPUI app error: {}", error)),
        }
    }

    /// Forward a tool call to the app. Tool names equal IPC method names.
    ///
    /// Any screenshot in the answer — the whole answer for `take_screenshot`,
    /// or one step of a `batch` — is lifted out of the JSON and returned
    /// alongside it, so the agent sees a picture rather than a path into a
    /// temp directory it cannot read.
    fn handle_tool_call(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<InlineImage>)> {
        if !methods::ALL.contains(&tool_name) {
            anyhow::bail!("Unknown tool: {}", tool_name);
        }

        let mut result = self.send_ipc_request(tool_name, arguments)?;
        let mut images = Vec::new();

        match tool_name {
            methods::TAKE_SCREENSHOT => images.extend(inline_screenshot(&mut result)),
            methods::BATCH => {
                for step in result
                    .get_mut("steps")
                    .and_then(|steps| steps.as_array_mut())
                    .into_iter()
                    .flatten()
                {
                    if step["method"] != methods::TAKE_SCREENSHOT || step["ok"] != json!(true) {
                        continue;
                    }
                    if let Some(step_result) = step.get_mut("result") {
                        images.extend(inline_screenshot(step_result));
                    }
                }
            }
            _ => {}
        }

        Ok((result, images))
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

/// Render a `ui_snapshot` answer as the text it already is.
///
/// Everything else is handed to the agent as pretty-printed JSON, which for a
/// snapshot would be the worst of both worlds: every newline escaped, the
/// indentation that carries the structure turned into `\n  ` noise, and the
/// size roughly doubled for nothing. Returns `None` for any other tool.
fn snapshot_text(tool_name: &str, content: &serde_json::Value) -> Option<String> {
    if tool_name != methods::UI_SNAPSHOT {
        return None;
    }
    let snapshot = content.get("snapshot")?.as_str()?;

    let mut header = format!(
        "ui_snapshot {} — {}, {} of {} painted elements",
        content["snapshot_id"],
        content["window_id"].as_str().unwrap_or("?"),
        content["elements"],
        content["painted_elements"],
    );
    if content["truncated"] == json!(true) {
        header.push_str(
            " (stopped early — raise max_elements, or narrow with filter / root_element_id)",
        );
    }

    Some(format!("{header}\n{snapshot}"))
}

/// Name of the tool that replays a recorded script. Server-local, like the
/// guide: it drives the app through the other tools rather than being one of
/// them.
const REPLAY_TOOL: &str = "replay_script";

/// Replay a script file, forwarding each of its steps.
///
/// There is no separate assertion step: a `wait_for` that comes back
/// unsatisfied is a failed assertion, and already says which condition did not
/// hold. So the same file both reaches a state and tests reaching it.
fn replay_result(
    server: &GpuiMcpServer,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    let path = arguments
        .get("path")
        .and_then(|path| path.as_str())
        .ok_or_else(|| anyhow::anyhow!("replay_script needs a `path` to a recorded script"))?;

    let script = Script::read(std::path::Path::new(path))?;
    let options = ReplayOptions {
        seek: arguments
            .get("seek")
            .and_then(|seek| seek.as_bool())
            .unwrap_or(false),
        stop_on_error: arguments
            .get("stop_on_error")
            .and_then(|stop| stop.as_bool())
            .unwrap_or(true),
    };

    let report = script::replay(&script, &options, |method, params| {
        server
            .handle_tool_call(method, params)
            .map(|(value, _images)| value)
    });

    let mut result = serde_json::to_value(&report)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("script".into(), json!(script.name));
        object.insert("path".into(), json!(path));
    }
    Ok(result)
}

/// A successful JSON-RPC result.
fn ok_response(id: serde_json::Value, result: serde_json::Value) -> McpResponse {
    McpResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(result),
        error: None,
    }
}

/// A JSON-RPC `Invalid params` error — a resource or prompt that does not
/// exist, with a message naming what does.
fn invalid_params(id: serde_json::Value, message: String) -> McpResponse {
    McpResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(McpError {
            code: -32602,
            message,
        }),
    }
}

/// Serve one documentation topic.
///
/// Deliberately answered without touching the socket: the first thing an
/// agent does should work before the app is started, and a guide that fails
/// with "No running GPUI app found" would teach exactly the wrong lesson.
fn guide_result(arguments: &serde_json::Value) -> serde_json::Value {
    let requested = arguments
        .get("topic")
        .and_then(|topic| topic.as_str())
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .unwrap_or(docs::DEFAULT_TOPIC);

    match docs::topic(requested) {
        Some(topic) => json!({ "content": [{ "type": "text", "text": topic.body }] }),
        None => json!({
            "content": [{ "type": "text", "text": docs::unknown_topic_message(requested) }],
            "isError": true
        }),
    }
}

/// The same topics as MCP resources, for clients that attach resources
/// instead of calling tools.
fn resources_list() -> serde_json::Value {
    let resources: Vec<serde_json::Value> = docs::TOPICS
        .iter()
        .map(|topic| {
            json!({
                "uri": format!("{}{}", docs::RESOURCE_PREFIX, topic.name),
                "name": format!("gpui-mcp guide: {}", topic.name),
                "description": topic.summary,
                "mimeType": "text/markdown",
            })
        })
        .collect();

    json!({ "resources": resources })
}

/// Read one guide resource. `Err` carries the message for a JSON-RPC error.
fn read_resource(uri: &str) -> Result<serde_json::Value, String> {
    let name = uri.strip_prefix(docs::RESOURCE_PREFIX).ok_or_else(|| {
        format!(
            "Unknown resource '{}'. Guide resources start with {}",
            uri,
            docs::RESOURCE_PREFIX
        )
    })?;

    let topic = docs::topic(name).ok_or_else(|| docs::unknown_topic_message(name))?;

    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "text/markdown",
            "text": topic.body,
        }]
    }))
}

/// One prompt, which clients such as Claude Code surface as a slash command.
fn prompts_list() -> serde_json::Value {
    json!({
        "prompts": [{
            "name": docs::PROMPT_NAME,
            "description": "Everything needed to drive a GPUI app through gpui-mcp: \
                            orientation, the tool list and worked examples.",
            "arguments": [{
                "name": "topic",
                "description": format!(
                    "One topic ({}). Omitted: overview, tools and recipes together.",
                    docs::topic_names()
                ),
                "required": false
            }]
        }]
    })
}

/// Answer `prompts/get`. `Err` carries the message for a JSON-RPC error.
fn get_prompt(name: &str, arguments: &serde_json::Value) -> Result<serde_json::Value, String> {
    if name != docs::PROMPT_NAME {
        return Err(format!(
            "Unknown prompt '{}'. This server offers '{}'.",
            name,
            docs::PROMPT_NAME
        ));
    }

    let requested = arguments
        .get("topic")
        .and_then(|topic| topic.as_str())
        .map(str::trim)
        .filter(|topic| !topic.is_empty());

    let text = match requested {
        Some(name) => docs::topic(name)
            .map(|topic| topic.body.to_string())
            .ok_or_else(|| docs::unknown_topic_message(name))?,
        // No topic asked for: the three that get an agent working, in reading
        // order. The rest are one tool call away.
        None => ["overview", "tools", "recipes"]
            .iter()
            .filter_map(|name| docs::topic(name))
            .map(|topic| topic.body)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
    };

    Ok(json!({
        "description": "How to drive a GPUI app through gpui-mcp.",
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": text }
        }]
    }))
}

fn tools_list() -> serde_json::Value {
    json!({
        "tools": [
            {
                "name": docs::TOOL_NAME,
                "description": format!(
                    "Read this first. How to drive a GPUI app through this server: the three-step \
                     start, worked examples for every common task, how element ids resolve, and \
                     the traps that otherwise cost a round trip each. Answered by the server \
                     itself, so it works before the app is even running. Topics: {}. \
                     Example: {{\"topic\": \"recipes\"}}",
                    docs::topic_names()
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "topic": {
                            "type": "string",
                            "enum": docs::TOPICS.iter().map(|t| t.name).collect::<Vec<_>>(),
                            "description": "Which topic to read. Default: overview."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": REPLAY_TOOL,
                "description": "Replay a recorded script: a list of steps in the same shape a batch takes, saved to a file. Two uses from one file. seek=true skips the read-only steps and just puts the app back where work happens — worth doing at the start of a session instead of clicking your way there again. seek=false runs everything as a test: a wait_for that comes back unsatisfied is a failed assertion and the report says which condition did not hold. Record a script by starting this server with GPUI_MCP_RECORD=path.json. Example: {\"path\": \"tests/open-file.json\", \"seek\": true}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the script file."
                        },
                        "seek": {
                            "type": "boolean",
                            "description": "Skip steps that only read (inspect_ui_tree, screenshots, …) and run the rest. Default: false, which replays everything as a test."
                        },
                        "stop_on_error": {
                            "type": "boolean",
                            "description": "Stop at the first failing step. Default: true — the steps after a failure act on a state nobody intended."
                        }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "get_windows",
                "description": "List all open GPUI windows with their ID, title, bounds, and active status. Use this first to discover window IDs for other tools. Returns: [{id, title, bounds: {x,y,width,height}, is_active}]. Example: {}",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "ui_snapshot",
                "description": "Read the window as a short, readable list: one line per element that means something, with the layout scaffolding left out. START HERE instead of inspect_ui_tree — on a real UI this is a fraction of the size and answers the question you actually have, which is what is on screen and what can be acted on. Every line ends with a @ref (@e7) that works as element_id in click_element, wait_for, get_element and take_screenshot; the next snapshot replaces them. Lines read: role \"name\" #test-id @ref. Example: {\"interactive_only\": true}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_id": {
                            "type": "string",
                            "description": "Window to snapshot (default: active window)"
                        },
                        "root_element_id": {
                            "type": "string",
                            "description": "Snapshot this element's subtree instead of the whole window. Takes an id or a @ref."
                        },
                        "filter": {
                            "type": "string",
                            "description": "Keep only elements whose role, name or test id contains this (case-insensitive), plus the ancestors leading to them."
                        },
                        "interactive_only": {
                            "type": "boolean",
                            "description": "Keep only what you can act on — buttons, inputs, list items, tabs. The fastest way to answer 'what can I click here?'."
                        },
                        "max_elements": {
                            "type": "integer",
                            "description": "Stop after this many lines and say so. Default 200."
                        },
                        "include_bounds": {
                            "type": "boolean",
                            "description": "Add each element's bounds. Off by default — a snapshot is about structure, and coordinates are what make the tree expensive."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "a11y_audit",
                "description": "Check the window for accessibility problems that are also targeting problems: a control with no text (nothing can name it, nothing can match on it), an id that names several elements (a suffix match takes the first, so a script targeting it may click the wrong one), a target below the 24px minimum, a control painted with no area. Each finding names the element, its id, and the source location gpui recorded — for a gpui-component widget that is the widget own file, so it says what the element is; the id and the element path are what locate it in your app. Reads the same derived layer ui_snapshot prints, so it cannot see colours and does not check contrast. As a step in a recorded script it fails the replay when findings reach fail_on, which is how this stays checked. Example: {\"fail_on\": \"serious\"}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_id": {
                            "type": "string",
                            "description": "Window to audit (default: active window)"
                        },
                        "root_element_id": {
                            "type": "string",
                            "description": "Audit this element's subtree instead of the whole window. Takes an id or a @ref."
                        },
                        "fail_on": {
                            "type": "string",
                            "enum": ["serious", "warning", "none"],
                            "description": "Severity at which the audit reports ok:false. Default: serious."
                        },
                        "min_target_size": {
                            "type": "number",
                            "description": "Smallest acceptable side of an interactive element, in pixels. Default 24 (WCAG 2.2)."
                        },
                        "max_findings": {
                            "type": "integer",
                            "description": "Stop after this many findings. Default 50."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "a11y_tree",
                "description": "The accessibility tree for the window: what a screen reader would actually be handed. Real roles (Button, MenuBar, TextInput) rather than roles guessed from a filename, the label a control announces even when it paints no text, an input's current value, and the actions each node offers (Click, Focus, SetValue). GPUI builds this tree only while assistive technology is attached, so this turns it on for the window and waits a frame. It does NOT replace ui_snapshot: only elements somebody annotated get a node, so the tree is far smaller than the window — the answer says how many of the painted elements made it in. Each node carries the element id and source location, so a node can be matched to a snapshot line. Example: {}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_id": {
                            "type": "string",
                            "description": "Window to read (default: active window)"
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "inspect_ui_tree",
                "description": "Get the UI element hierarchy for debugging layout and structure. Each element has: id, element_type (derived from source file), bounds, source_location, children, properties. Use max_depth to limit tree size (default: unlimited). Use root_element_id to inspect a subtree instead of the whole app. Use format='compact' to strip verbose fields (bounds, content_mask, source_location, content_size). WARNING: Without filters this can return very large responses. Example: {\"max_depth\": 3, \"format\": \"compact\"}",
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
                "description": "Get a UI element and its full subtree by ID. Supports exact full_id, global_id, or suffix match. Returns the element with all descendants, text content, bounds, source location, and properties. Example: {\"element_id\": \"results\"}",
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
                "description": "Get information about the currently focused element and active key contexts. Essential for debugging keyboard/focus issues. Returns: {has_focus, focus_id, window_id, key_contexts: [...]}. Example: {}",
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
                "description": "List GPUI actions that can be dispatched via execute_action. Actions are the keyboard shortcuts and commands of the app (e.g. 'elane::CursorUp', 'elane::ToggleTerminal'). Use filter to search by name substring. Set include_bindings=true for keybinding and context info. Set only_available=true to restrict results to actions whose binding context matches the current focus chain — i.e. 'what can I actually press right now?' (implies include_bindings=true). Example: {\"filter\": \"toggle\", \"include_bindings\": true}",
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
                        },
                        "only_available": {
                            "type": "boolean",
                            "description": "If true, only return actions whose keybinding predicate matches the current focus chain. Implies include_bindings=true. Default: false."
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Window to evaluate only_available against (default: active window)"
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
                "description": "Send a keyboard keystroke to the app. The key is dispatched to the focused element. Use GPUI key format: lowercase key name with modifier prefixes. Examples: 'a', 'enter', 'escape', 'tab', 'f1', 'up', 'down'. Modifiers via the modifiers object. Example: {\"key\": \"s\", \"modifiers\": {\"ctrl\": true}}",
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
                "description": "Simulate a mouse click. Provide EITHER element_id (clicks center of that element) OR x/y pixel coordinates. Element ID supports full_id, global_id, or suffix match. Example: {\"element_id\": \"save-button\"}",
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
                "description": "Type a text string into the focused element by dispatching individual keystrokes. Much more convenient than send_key for entering text in input fields and dialogs. Example: {\"text\": \"src/main.rs\"}",
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
                "name": "wait_for",
                "description": "Wait until the app looks a certain way, then answer. The app checks once per painted frame, so waiting here costs nothing — NEVER poll by calling inspect_ui_tree or get_app_state in a loop, that costs a round trip per look. Every condition given must hold at the same time. Set absent=true to wait for them to STOP holding, which is how you wait for a dialog to close or a spinner to disappear. Running out of time is not an error: the answer says satisfied:false and 'checks' names the part that was missing. Example: {\"text\": \"Saved\", \"timeout_ms\": 5000}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "element_id": {
                            "type": "string",
                            "description": "Wait until an element with this id is painted. Full id, global_id or suffix, as everywhere else."
                        },
                        "text": {
                            "type": "string",
                            "description": "Wait until this text is painted anywhere in the window (case-insensitive)."
                        },
                        "key_context": {
                            "type": "string",
                            "description": "Wait until this key context is on the focus chain (substring, case-insensitive) — how you wait for a dialog or a mode to take the keyboard."
                        },
                        "app_state_path": {
                            "type": "string",
                            "description": "JSON pointer into the get_app_state answer, e.g. '/app/rows'. Without app_state_equals the condition is 'this resolves to something other than null'."
                        },
                        "app_state_equals": {
                            "description": "The value app_state_path must reach."
                        },
                        "absent": {
                            "type": "boolean",
                            "description": "Invert: wait until the conditions stop holding. Default: false."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "Give up after this long. Default 3000, capped at 30000."
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Window to watch (default: active window)"
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "batch",
                "description": "Run several tools in one call. Steps run in order inside the app and the answer carries each step's result plus one app_state/focus_info at the end. This is the main way to spend fewer turns: click, type, enter, wait is ONE call instead of four. Steps stop at the first failure unless stop_on_error is false. A batch cannot contain another batch. Screenshots taken inside a batch come back as images attached to the answer. Example: {\"steps\": [{\"method\": \"click_element\", \"params\": {\"element_id\": \"search\"}}, {\"method\": \"type_text\", \"params\": {\"text\": \"main.rs\"}}, {\"method\": \"send_key\", \"params\": {\"key\": \"enter\"}}, {\"method\": \"wait_for\", \"params\": {\"text\": \"main.rs\"}}]}",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "steps": {
                            "type": "array",
                            "description": "The steps, in order. At most 32.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "method": {
                                        "type": "string",
                                        "enum": methods::ALL.iter().filter(|method| **method != methods::BATCH).collect::<Vec<_>>(),
                                        "description": "The tool to run for this step."
                                    },
                                    "params": {
                                        "type": "object",
                                        "description": "That tool's arguments."
                                    }
                                },
                                "required": ["method"]
                            }
                        },
                        "stop_on_error": {
                            "type": "boolean",
                            "description": "Stop at the first failing step. Default: true. Turning it off means later steps run against whatever state the failure left behind."
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Default window for steps that do not name one."
                        }
                    },
                    "required": ["steps"]
                }
            },
            {
                "name": "take_screenshot",
                "description": "Take a screenshot of a window or a specific element. Renders the current window content to a PNG image. Optionally crop to a specific element by ID for a focused, higher-detail view. Example: {\"element_id\": \"sidebar\"}",
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
                        },
                        "max_width": {
                            "type": "integer",
                            "description": "Downscale until the image is at most this wide in pixels. Default 1400; 0 keeps every pixel. An image costs tokens by its dimensions, so ask for full size only when you need to read fine detail. When the image was scaled the answer says so, and coordinates read off it are no longer window coordinates."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "get_app_state",
                "description": "Get a snapshot of the application state: window count, active window, and per-window bounds/titles. Quick overview without the full UI tree. Example: {}",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "get_logs",
                "description": "Get recent MCP-related log entries (up to 500 buffered). Useful for debugging MCP interactions and seeing results of dispatched actions/keys. Example: {}",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        ]
    })
}

/// What this binary prints when asked, and when told something it does not
/// understand.
const USAGE: &str = "\
gpui-mcp-server — an MCP server for inspecting and driving a running GPUI app.

  gpui-mcp-server                       speak MCP on stdin/stdout (what an
                                        agent launches; the usual case)
  gpui-mcp-server replay <script.json>  replay a recorded script and report
      --seek                            skip the read-only steps: reach the
                                        state, do not test the way there
      --keep-going                      do not stop at the first failure
  gpui-mcp-server --help

Environment:
  GPUI_MCP_APP      restrict discovery to one app name
  GPUI_MCP_PID      with GPUI_MCP_APP: one exact instance, no discovery
  GPUI_MCP_RECORD   write every successful tool call to this script file
";

/// The command-line side.
///
/// Replaying without an agent is what makes a recording usable in CI: the same
/// file an agent produced by exploring becomes a test that costs no model
/// tokens to run.
fn run_command(arguments: &[String]) -> Result<()> {
    match arguments[0].as_str() {
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "replay" => run_replay(&arguments[1..]),
        other => {
            eprintln!("Unknown command: {other}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn run_replay(arguments: &[String]) -> Result<()> {
    let mut path: Option<&str> = None;
    let mut options = ReplayOptions {
        seek: false,
        stop_on_error: true,
    };

    for argument in arguments {
        match argument.as_str() {
            "--seek" => options.seek = true,
            "--keep-going" => options.stop_on_error = false,
            other if other.starts_with('-') => {
                anyhow::bail!("Unknown option for replay: {other}");
            }
            other => path = Some(other),
        }
    }

    let path = path.ok_or_else(|| anyhow::anyhow!("replay needs a script file"))?;
    let script = Script::read(std::path::Path::new(path))?;
    let server = GpuiMcpServer::new();

    let report = script::replay(&script, &options, |method, params| {
        server
            .handle_tool_call(method, params)
            .map(|(value, _images)| value)
    });

    for step in &report.steps {
        match &step.detail {
            Some(detail) => println!(
                "{:>3}  {:<7}  {} — {}",
                step.index + 1,
                step.status,
                step.method,
                detail
            ),
            None => println!("{:>3}  {:<7}  {}", step.index + 1, step.status, step.method),
        }
    }

    println!(
        "\n{}: {} passed, {} failed, {} skipped, of {}",
        script.name, report.passed, report.failed, report.skipped, report.of
    );

    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if !arguments.is_empty() {
        return run_command(&arguments);
    }

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
                Some((app, pid)) => {
                    eprintln!("Initial resolve: app='{}', pid={}, path={}", app, pid, path)
                }
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

    // `GPUI_MCP_RECORD` turns the session into a script as it happens: an
    // agent finding its way around an app is already writing the test, and
    // the seek script that gets the next session there in one call.
    let mut recorder = match std::env::var("GPUI_MCP_RECORD") {
        Ok(path) if !path.trim().is_empty() => {
            let recorder = Recorder::new(path.trim(), std::env::var("GPUI_MCP_APP").ok());
            eprintln!(
                "[MCP] Recording this session to {}",
                recorder.path().display()
            );
            Some(recorder)
        }
        _ => None,
    };

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
                send_response(
                    &mut stdout,
                    &McpResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {
                                "tools": {},
                                "resources": {},
                                "prompts": {}
                            },
                            "serverInfo": {
                                "name": "gpui-mcp-inspector",
                                "version": env!("CARGO_PKG_VERSION")
                            },
                            // Short orientation, always in the agent's
                            // context. The long form is the `gpui_guide` tool.
                            "instructions": docs::INSTRUCTIONS
                        })),
                        error: None,
                    },
                )?;
            }

            // Client sends this after initialize — acknowledge silently
            "notifications/initialized" => {
                eprintln!("[MCP] Client initialized");
                // Notifications don't get responses
            }

            // Health check
            "ping" => {
                send_response(
                    &mut stdout,
                    &McpResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(json!({})),
                        error: None,
                    },
                )?;
            }

            "tools/list" => {
                send_response(
                    &mut stdout,
                    &McpResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: Some(tools_list()),
                        error: None,
                    },
                )?;
            }

            "tools/call" => {
                let params = request.params.unwrap_or(json!({}));
                let tool_name = params["name"].as_str().unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                // The guide is the server's own; forwarding it would fail
                // whenever no app is running, which is precisely when an
                // agent is most likely to be reading it.
                if tool_name == docs::TOOL_NAME {
                    send_response(&mut stdout, &ok_response(id, guide_result(&arguments)))?;
                    continue;
                }

                // Replay drives the app through the other tools rather than
                // being one of them, so it is answered here too.
                if tool_name == REPLAY_TOOL {
                    let response = match replay_result(&server, &arguments) {
                        Ok(report) => ok_response(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&report)?,
                                }]
                            }),
                        ),
                        Err(error) => ok_response(
                            id,
                            json!({
                                "content": [{ "type": "text", "text": format!("Error: {error}") }],
                                "isError": true,
                            }),
                        ),
                    };
                    send_response(&mut stdout, &response)?;
                    continue;
                }

                let recorded_arguments = arguments.clone();

                let response = match server.handle_tool_call(tool_name, arguments) {
                    Ok((content, images)) => {
                        let text = match snapshot_text(tool_name, &content) {
                            Some(text) => text,
                            None => serde_json::to_string_pretty(&content)?,
                        };
                        // Only successful calls are worth recording: a script
                        // of things that did not work replays nothing.
                        if let Some(recorder) = recorder.as_mut() {
                            let snapshot =
                                (tool_name == methods::UI_SNAPSHOT).then_some(text.as_str());
                            if let Err(error) =
                                recorder.record(tool_name, &recorded_arguments, snapshot)
                            {
                                eprintln!(
                                    "[MCP] Could not write {}: {}",
                                    recorder.path().display(),
                                    error
                                );
                            }
                        }

                        let mut blocks = vec![json!({ "type": "text", "text": text })];
                        // One image for a screenshot, possibly several from a
                        // batch that took more than one.
                        for image in images {
                            blocks.push(json!({
                                "type": "image",
                                "data": image.data,
                                "mimeType": image.mime_type,
                            }));
                        }

                        McpResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: Some(json!({ "content": blocks })),
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

            // The guide again, as resources — some clients attach those
            // rather than call a tool for them.
            "resources/list" => {
                send_response(&mut stdout, &ok_response(id, resources_list()))?;
            }

            // Nothing here is templated, but answering keeps a client that
            // probes for templates from logging a "method not found".
            "resources/templates/list" => {
                send_response(
                    &mut stdout,
                    &ok_response(id, json!({ "resourceTemplates": [] })),
                )?;
            }

            "resources/read" => {
                let uri = request
                    .params
                    .as_ref()
                    .and_then(|params| params.get("uri"))
                    .and_then(|uri| uri.as_str())
                    .unwrap_or("")
                    .to_string();

                let response = match read_resource(&uri) {
                    Ok(contents) => ok_response(id, contents),
                    Err(message) => invalid_params(id, message),
                };
                send_response(&mut stdout, &response)?;
            }

            "prompts/list" => {
                send_response(&mut stdout, &ok_response(id, prompts_list()))?;
            }

            "prompts/get" => {
                let params = request.params.clone().unwrap_or(json!({}));
                let name = params["name"].as_str().unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                let response = match get_prompt(name, &arguments) {
                    Ok(prompt) => ok_response(id, prompt),
                    Err(message) => invalid_params(id, message),
                };
                send_response(&mut stdout, &response)?;
            }

            // Unknown methods — ignore notifications, error on requests
            method => {
                if method.starts_with("notifications/") {
                    // Notifications don't need responses
                    eprintln!("[MCP] Ignoring unknown notification: {}", method);
                } else {
                    send_response(
                        &mut stdout,
                        &McpResponse {
                            jsonrpc: "2.0".to_string(),
                            id,
                            result: None,
                            error: Some(McpError {
                                code: -32601,
                                message: format!("Method not found: {}", method),
                            }),
                        },
                    )?;
                }
            }
        }
    }

    if let Some(recorder) = &recorder {
        eprintln!(
            "[MCP] Recorded {} steps to {}",
            recorder.steps(),
            recorder.path().display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guide only helps if it is the first thing an agent sees.
    #[test]
    fn the_guide_is_advertised_first() {
        let tools = tools_list();
        let tools = tools["tools"].as_array().expect("tools array");
        assert_eq!(tools[0]["name"], docs::TOOL_NAME);
        assert_eq!(
            tools.len(),
            methods::ALL.len() + 2,
            "every IPC method plus the two the server answers itself"
        );
        assert!(
            tools.iter().any(|tool| tool["name"] == REPLAY_TOOL),
            "replay is not advertised"
        );
        for method in methods::ALL {
            assert!(
                tools.iter().any(|tool| tool["name"] == *method),
                "tool '{method}' is not advertised"
            );
        }
    }

    /// A `topic` outside the schema's enum would be refused by strict clients
    /// before it ever reaches `guide_result`.
    #[test]
    fn the_guide_schema_offers_every_topic() {
        let tools = tools_list();
        let enum_values = tools["tools"][0]["inputSchema"]["properties"]["topic"]["enum"]
            .as_array()
            .expect("topic enum")
            .clone();
        assert_eq!(enum_values.len(), docs::TOPICS.len());
        for topic in docs::TOPICS {
            assert!(
                enum_values.iter().any(|value| value == topic.name),
                "{} missing from the schema enum",
                topic.name
            );
        }
    }

    #[test]
    fn the_guide_defaults_to_the_overview() {
        let overview = docs::topic(docs::DEFAULT_TOPIC).unwrap().body;
        for arguments in [json!({}), json!({ "topic": "" }), json!({ "topic": "  " })] {
            let result = guide_result(&arguments);
            assert_eq!(result["content"][0]["text"], overview, "{arguments}");
            assert!(result.get("isError").is_none());
        }
    }

    #[test]
    fn the_guide_serves_the_topic_asked_for() {
        let result = guide_result(&json!({ "topic": "recipes" }));
        assert_eq!(
            result["content"][0]["text"],
            docs::topic("recipes").unwrap().body
        );
    }

    #[test]
    fn a_bad_topic_gets_the_menu_not_a_shrug() {
        let result = guide_result(&json!({ "topic": "nonsense" }));
        assert_eq!(result["isError"], json!(true));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("nonsense"));
        for topic in docs::TOPICS {
            assert!(text.contains(topic.name), "{} not offered", topic.name);
        }
    }

    #[test]
    fn every_topic_is_readable_as_a_resource() {
        let listed = resources_list();
        let listed = listed["resources"].as_array().unwrap().clone();
        assert_eq!(listed.len(), docs::TOPICS.len());

        for resource in listed {
            let uri = resource["uri"].as_str().unwrap();
            let contents = read_resource(uri).expect("listed resource must be readable");
            let name = uri.strip_prefix(docs::RESOURCE_PREFIX).unwrap();
            assert_eq!(contents["contents"][0]["uri"], uri);
            assert_eq!(
                contents["contents"][0]["text"],
                docs::topic(name).unwrap().body
            );
        }
    }

    #[test]
    fn a_resource_outside_the_guide_is_refused() {
        for uri in [
            "gpui://guide/nonsense",
            "file:///etc/passwd",
            "gpui://something-else",
            "",
        ] {
            assert!(read_resource(uri).is_err(), "{uri} should not resolve");
        }
    }

    #[test]
    fn the_prompt_without_a_topic_carries_the_starting_three() {
        let prompt = get_prompt(docs::PROMPT_NAME, &json!({})).expect("prompt");
        let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
        for name in ["overview", "tools", "recipes"] {
            assert!(
                text.contains(docs::topic(name).unwrap().body),
                "{name} missing from the onboarding prompt"
            );
        }
        assert_eq!(prompt["messages"][0]["role"], "user");
    }

    #[test]
    fn the_prompt_can_be_narrowed_to_one_topic() {
        let prompt = get_prompt(docs::PROMPT_NAME, &json!({ "topic": "focus" })).expect("prompt");
        assert_eq!(
            prompt["messages"][0]["content"]["text"],
            docs::topic("focus").unwrap().body
        );
    }

    #[test]
    fn an_unknown_prompt_names_the_one_that_exists() {
        let error = get_prompt("nope", &json!({})).expect_err("unknown prompt");
        assert!(error.contains(docs::PROMPT_NAME), "{error}");
        let error =
            get_prompt(docs::PROMPT_NAME, &json!({ "topic": "nope" })).expect_err("unknown topic");
        assert!(error.contains("nope"), "{error}");
    }

    /// `handle_tool_call` forwards over the socket; the guide must never get
    /// that far, or it would fail exactly when it is needed most. Replay is
    /// server-local for a different reason: it drives the app *through* the
    /// other tools rather than being one of them.
    #[test]
    fn the_server_local_tools_are_not_ipc_methods() {
        assert!(!methods::ALL.contains(&docs::TOOL_NAME));
        assert!(!methods::ALL.contains(&REPLAY_TOOL));
    }

    /// A replay that recorded itself would grow without end.
    #[test]
    fn the_server_local_tools_are_never_recorded() {
        assert!(script::NOT_RECORDED.contains(&docs::TOOL_NAME));
        assert!(script::NOT_RECORDED.contains(&REPLAY_TOOL));
    }

    #[test]
    fn replay_needs_a_path() {
        let server = GpuiMcpServer::new();
        let error = replay_result(&server, &json!({})).expect_err("no path");
        assert!(error.to_string().contains("path"), "{error}");
    }

    #[test]
    fn replay_says_when_the_script_is_not_one() {
        let server = GpuiMcpServer::new();
        let path =
            std::env::temp_dir().join(format!("gpui-mcp-not-a-script-{}.json", std::process::id()));
        std::fs::write(&path, "{\"nope\": true}").unwrap();

        let error = replay_result(&server, &json!({ "path": path.to_string_lossy() }))
            .expect_err("not a script");
        assert!(
            error.to_string().contains("not a gpui-mcp script"),
            "{error}"
        );

        std::fs::remove_file(&path).ok();
    }

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
