# GPUI MCP Inspector

Model Context Protocol (MCP) Server für GPUI-basierte Anwendungen. Ermöglicht Claude direkten Zugriff auf UI-Struktur, State und Interaktionsmöglichkeiten deiner GPUI App.

## Was ist das?

Ein Toolkit bestehend aus:

1. **MCP Server** (`gpui-mcp-server`) - Kommuniziert mit Claude über stdio
2. **IPC Protocol** - Shared types für die Kommunikation
3. **GPUI Integration** - Code zum Einbauen in deine GPUI App (z.B. Elane)

## Workflow

```
Claude (claude.ai/app)
    ↓ MCP Protocol (stdio)
gpui-mcp-server
    ↓ IPC (Unix Socket)
Deine GPUI App (Elane)
```

## Verfügbare Tools

### UI Inspektion
- `inspect_ui_tree` - Komplette UI-Hierarchie mit Bounds und Properties
- `get_element` - Details zu einem spezifischen Element
- `get_windows` - Liste aller Fenster
- `take_screenshot` - Screenshot mit optionalen Element-Highlights

### Automatisierung
- `click_element` - Maus-Clicks simulieren
- `send_key` - Keyboard-Input senden
- `execute_action` - Benannte Actions ausführen

### Debugging
- `get_app_state` - App-State Snapshot
- `get_logs` - Recent Logs

## Setup

### 1. MCP Server bauen

```bash
cargo build --release
```

Der Binary landet in `target/release/gpui-mcp-server`.

### 2. In GPUI App integrieren

Siehe `examples/gpui_integration.rs` für ein vollständiges Beispiel.

**Minimale Integration:**

```rust
use gpui_mcp_protocol::*;
use std::sync::Arc;
use tokio::sync::Mutex;

// 1. Implementiere AppInterface für deine App
impl AppInterface for MyGpuiApp {
    fn get_ui_tree(&self) -> UiTree {
        // Traversiere GPUI Element Tree
        // Sammle Bounds, Properties, etc.
    }
    
    fn click_element(&self, event: &ClickEvent) -> bool {
        // Dispatche MouseDown/MouseUp
    }
    
    // ... weitere Methods
}

// 2. Starte IPC Server beim App-Start
#[tokio::main]
async fn main() {
    let app_interface = Box::new(MyGpuiApp::new());
    let app_handle = Arc::new(Mutex::new(app_interface));
    
    let ipc_server = GpuiIpcServer::new(
        "/tmp/gpui-mcp.sock".to_string(),
        app_handle
    );
    
    tokio::spawn(async move {
        ipc_server.start().await.unwrap();
    });
    
    // Normale GPUI App
    App::new().run(|cx| {
        // ...
    });
}
```

### 3. Claude Desktop konfigurieren

Füge zu deiner Claude Desktop Config hinzu (`~/Library/Application Support/Claude/claude_desktop_config.json` auf macOS):

```json
{
  "mcpServers": {
    "gpui-inspector": {
      "command": "/pfad/zu/gpui-mcp-server",
      "env": {
        "GPUI_MCP_SOCKET": "/tmp/gpui-mcp.sock"
      }
    }
  }
}
```

### 4. Starten

1. Starte deine GPUI App (die den IPC Server enthält)
2. Starte Claude Desktop
3. Claude hat jetzt Zugriff auf die Tools!

## Beispiel-Session

**Du:** "Inspiziere mal die UI von Elane"

**Claude:** 
```
[verwendet inspect_ui_tree tool]

Die UI hat folgende Struktur:
- Root Window (1920x1080)
  - Left Panel (300px breit)
    - File List
    - 156 Items sichtbar
  - Right Panel (1620px breit)
    - Content View
    - ...
```

**Du:** "Mach mal einen Screenshot vom linken Panel mit Highlight"

**Claude:**
```
[verwendet take_screenshot mit highlight_elements]
[zeigt Screenshot]
```

**Du:** "Click auf das 3. Item in der Liste"

**Claude:**
```
[verwendet get_element um Position zu finden]
[verwendet click_element]
Erledigt!
```

## Entwicklung

### Tests laufen lassen

```bash
cargo test
```

### Beispiel Integration ansehen

```bash
cargo run --example gpui_integration
```

### Debugging

Der MCP Server schreibt Debug-Output nach stderr:

```bash
# In Claude Desktop Config:
"command": "/pfad/zu/gpui-mcp-server 2>/tmp/mcp-debug.log"
```

## IPC Protocol Details

Kommunikation über Unix Domain Socket (`/tmp/gpui-mcp.sock` per Default).

**Request Format:**
```json
{
  "id": "uuid",
  "method": "inspect_ui_tree",
  "params": {}
}
```

**Response Format:**
```json
{
  "id": "uuid",
  "result": {
    "Ok": { /* data */ }
  }
}
```

Bei Fehler:
```json
{
  "id": "uuid",
  "result": {
    "Err": "error message"
  }
}
```

## Performance

- Socket-Verbindung wird für jeden Request neu aufgebaut (stateless)
- UI Tree Traversierung sollte gecached werden wenn möglich
- Screenshots sind teuer - nur bei Bedarf

## Sicherheit

⚠️ **WICHTIG**: Dieser MCP Server gibt Claude vollen Zugriff auf deine App!

- Nur lokal verwenden (Unix Socket)
- Nicht in Produktions-Builds aktivieren
- Socket-File sollte nur für deinen User lesbar sein

## Todo / Ideen

- [ ] Screenshots tatsächlich implementieren (PNG encoding)
- [ ] Element-Highlighting im Screenshot
- [ ] Performance-Profiling
- [ ] Cached UI Tree für bessere Performance
- [ ] WebSocket alternative zu Unix Socket (für Remote Debugging)
- [ ] Replay-Funktion für Test-Automation
- [ ] Integration mit GPUI's eigenen Debugging Tools

## Lizenz

MIT oder Apache 2.0 (wie du möchtest)
