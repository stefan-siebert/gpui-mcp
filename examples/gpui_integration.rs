/// Beispiel Integration in eine GPUI App
///
/// Zeigt wie man:
/// 1. AppInterface implementiert
/// 2. IpcServer startet
/// 3. MCP Server kann dann mit der App kommunizieren

use gpui_mcp_protocol::protocol::*;
use gpui_mcp_protocol::server::{AppInterface, start_ipc_server};
use serde_json::json;

/// Beispiel AppInterface Implementierung
struct DemoApp;

impl AppInterface for DemoApp {
    fn get_ui_tree(&self) -> UiTree {
        UiTree {
            root: UiElement {
                id: "root".to_string(),
                element_type: "Window".to_string(),
                bounds: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1920.0,
                    height: 1080.0,
                },
                visible: true,
                children: vec![],
                properties: Default::default(),
                source_location: None,
                style_json: None,
                content_size: None,
            },
            window_count: 1,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    fn get_element(&self, _id: &str) -> Option<UiElement> {
        None
    }

    fn get_windows(&self) -> Vec<WindowInfo> {
        vec![WindowInfo {
            id: "main".to_string(),
            title: "Demo App".to_string(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            is_active: true,
            display_id: Some(0),
        }]
    }

    fn take_screenshot(&self, _params: &TakeScreenshotParams) -> Screenshot {
        Screenshot {
            png_base64: String::new(),
            width: 1920,
            height: 1080,
            highlighted_elements: vec![],
        }
    }

    fn click_element(&self, event: &ClickEvent) -> bool {
        eprintln!("Click at ({}, {})", event.x, event.y);
        true
    }

    fn send_key(&self, event: &KeyEvent) -> bool {
        eprintln!("Key: {}", event.key);
        true
    }

    fn execute_action(&self, params: &ExecuteActionParams) -> serde_json::Value {
        eprintln!("Action: {}", params.action);
        json!({ "success": true })
    }

    fn get_app_state(&self) -> serde_json::Value {
        json!({ "status": "running" })
    }

    fn get_logs(&self) -> Vec<String> {
        vec!["Demo app started".to_string()]
    }
}

fn main() {
    println!("GPUI MCP Integration Demo");
    println!("=========================");
    println!();
    println!("Starte IPC Server...");

    let _handle = start_ipc_server(Box::new(DemoApp));

    println!("IPC Server läuft. MCP Server kann jetzt verbinden.");
    println!("Drücke Ctrl+C zum Beenden.");

    // Warte auf Ctrl+C
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
