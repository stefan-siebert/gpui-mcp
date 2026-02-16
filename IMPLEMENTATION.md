# Implementierungs-Guide für Elane

## Schritt-für-Schritt Integration

### 1. Dependencies in Elane's Cargo.toml

```toml
[dependencies]
tokio = { version = "1.41", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Lokaler Pfad zum MCP Protocol
gpui-mcp-protocol = { path = "../gpui-mcp-inspector" }
```

### 2. Elane AppInterface Implementation

Erstelle eine neue Datei `src/mcp_integration.rs`:

```rust
use gpui::*;
use gpui_mcp_protocol::*;
use std::collections::HashMap;

pub struct ElaneAppInterface {
    // Halte Referenzen zu wichtigen Elane-Komponenten
    // Diese müssen thread-safe sein (Arc<Mutex<...>>)
}

impl AppInterface for ElaneAppInterface {
    fn get_ui_tree(&self) -> UiTree {
        // TODO: Implementiere echtes UI Tree Traversing
        // 
        // In GPUI könntest du:
        // 1. Über alle Windows iterieren
        // 2. Für jedes Window: Element Tree traversieren
        // 3. Bounds von jedem Element sammeln
        // 4. In UiElement Struktur packen
        //
        // Beispiel Pseudo-Code:
        // let windows = cx.windows();
        // for window in windows {
        //     let root_view = window.root_view();
        //     traverse_element(root_view, ...);
        // }
        
        UiTree {
            root: self.build_element_tree(),
            window_count: 1,
            timestamp: current_timestamp(),
        }
    }

    fn get_element(&self, id: &str) -> Option<UiElement> {
        // Finde Element by ID
        // Du könntest eine HashMap<String, WeakHandle<Element>> pflegen
        None
    }

    fn get_windows(&self) -> Vec<WindowInfo> {
        // In GPUI: cx.windows() iterieren
        vec![]
    }

    fn take_screenshot(&self, params: &TakeScreenshotParams) -> Screenshot {
        // GPUI kann Frames rendern
        // Du brauchst:
        // 1. Rendere current frame to image
        // 2. Optional: Zeichne Highlights über bestimmte Elements
        // 3. Encode zu PNG
        // 4. Base64 encode
        
        Screenshot {
            png_base64: "".to_string(),
            width: 1920,
            height: 1080,
            highlighted_elements: params.highlight_elements.clone(),
        }
    }

    fn click_element(&self, event: &ClickEvent) -> bool {
        // GPUI Event System verwenden
        // cx.dispatch_action(...) oder ähnlich
        true
    }

    fn send_key(&self, event: &KeyEvent) -> bool {
        // Keyboard Event ins GPUI Event System einspeisen
        true
    }

    fn execute_action(&self, params: &ExecuteActionParams) -> serde_json::Value {
        // Elane-spezifische Actions
        // z.B. "navigate_to_path", "select_item", etc.
        json!({ "success": true })
    }

    fn get_app_state(&self) -> serde_json::Value {
        // Serialize wichtigen App State
        // z.B.:
        // - Current path
        // - Selected items
        // - Panel states
        // - etc.
        json!({
            "current_path": "/home/user",
            "selected_items": [],
        })
    }

    fn get_logs(&self) -> Vec<String> {
        // Wenn Elane Logs sammelt, hier zurückgeben
        vec![]
    }
}

impl ElaneAppInterface {
    fn build_element_tree(&self) -> UiElement {
        // Haupt-Methode für UI Tree Building
        UiElement {
            id: "root".to_string(),
            element_type: "Window".to_string(),
            bounds: Bounds { x: 0.0, y: 0.0, width: 1920.0, height: 1080.0 },
            visible: true,
            children: vec![],
            properties: HashMap::new(),
        }
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
```

### 3. In main.rs integrieren

```rust
mod mcp_integration;

use mcp_integration::ElaneAppInterface;
use gpui_mcp_protocol::GpuiIpcServer;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    // MCP Integration starten (wenn enabled)
    let enable_mcp = std::env::var("ELANE_MCP_ENABLED")
        .unwrap_or_else(|_| "false".to_string())
        == "true";
    
    if enable_mcp {
        start_mcp_server();
    }

    // Normale Elane App
    App::new().run(|cx| {
        // ... existing code
    });
}

fn start_mcp_server() {
    let app_interface: Box<dyn AppInterface> = Box::new(ElaneAppInterface {
        // Initialisiere mit Elane-spezifischen Handles
    });
    let app_handle = Arc::new(Mutex::new(app_interface));
    
    let socket_path = std::env::var("GPUI_MCP_SOCKET")
        .unwrap_or_else(|_| "/tmp/elane-mcp.sock".to_string());
    
    let ipc_server = GpuiIpcServer::new(socket_path.clone(), app_handle);
    
    tokio::spawn(async move {
        eprintln!("Starting Elane MCP Server on {}", socket_path);
        if let Err(e) = ipc_server.start().await {
            eprintln!("MCP Server error: {}", e);
        }
    });
}
```

### 4. UI Tree Traversierung (Wichtigster Teil!)

Das ist der knifflige Teil. GPUI's Element-System ist etwas anders als andere UI Frameworks.

```rust
// Pseudo-Code für UI Tree Traversing in GPUI
fn traverse_view(view: &AnyView, cx: &AppContext) -> UiElement {
    let bounds = view.bounds(cx);  // oder wie auch immer man bounds in GPUI kriegt
    
    UiElement {
        id: format!("{:?}", view.entity_id()),  // oder eindeutige ID
        element_type: std::any::type_name_of_val(view).to_string(),
        bounds: Bounds {
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
        },
        visible: true,  // prüfen ob sichtbar
        children: vec![],  // rekursiv Children traversieren
        properties: HashMap::new(),
    }
}
```

### 5. Element ID System

Du brauchst ein System um Elements eindeutig zu identifizieren:

```rust
// Option 1: EntityId als String
let id = format!("{:?}", entity.entity_id());

// Option 2: Custom ID System
struct ElementId(usize);
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

// Option 3: Path-basiert
let id = "window.left_panel.file_list.item_5";
```

### 6. Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ui_tree() {
        let interface = ElaneAppInterface::new();
        let tree = interface.get_ui_tree();
        assert!(tree.window_count > 0);
    }
}
```

## Wichtige Überlegungen

### Performance
- UI Tree Traversierung kann teuer sein
- Caching erwägen (mit Invalidierung bei UI Changes)
- Nur traversieren wenn MCP Request kommt

### Threading
- GPUI läuft auf Main Thread
- IPC Server läuft in separatem Tokio Thread
- Kommunikation muss thread-safe sein (Arc, Mutex, Channels)

### Element Lifetime
- GPUI Elements können kurzlebig sein
- WeakHandle verwenden wenn du Referenzen speicherst
- IDs müssen trotzdem funktionieren auch wenn Element neu erstellt wird

## Debugging Tipps

1. **Logging hinzufügen**:
```rust
eprintln!("MCP: inspect_ui_tree called");
```

2. **Test Socket Connection**:
```bash
echo '{"id":"test","method":"inspect_ui_tree","params":{}}' | nc -U /tmp/elane-mcp.sock
```

3. **MCP Server Logs**:
```bash
# In Claude Desktop Config stderr redirecten
"command": "/path/to/gpui-mcp-server 2>/tmp/mcp.log"
```

## Nächste Schritte

1. ✅ Basic Integration (IPC Server starten)
2. ⬜ UI Tree Traversierung implementieren
3. ⬜ Element ID System
4. ⬜ Screenshot Funktion
5. ⬜ Click/Key Events
6. ⬜ Elane-spezifische Actions
7. ⬜ Performance Optimierung
8. ⬜ Tests schreiben
