# Quick Start Guide

## 1. Build den MCP Server

```bash
make release
```

Oder manuell:
```bash
cargo build --release
```

## 2. Integriere in Elane (oder deine GPUI App)

### In `Cargo.toml` von Elane:

```toml
[dependencies]
# ... existing dependencies
gpui-mcp-protocol = { path = "../gpui-mcp-inspector" }
tokio = { version = "1", features = ["full"] }
```

### In `main.rs` von Elane:

```rust
use gpui_mcp_protocol::*;
use std::sync::Arc;
use tokio::sync::Mutex;

// Implementiere AppInterface für Elane
// (siehe examples/gpui_integration.rs für Details)

// In main():
#[tokio::main]
async fn main() {
    // Starte IPC Server
    let app_interface: Box<dyn AppInterface> = Box::new(ElaneAppInterface {
        // deine Elane-Handles hier
    });
    let app_handle = Arc::new(Mutex::new(app_interface));
    
    let socket_path = "/tmp/elane-mcp.sock".to_string();
    let ipc_server = GpuiIpcServer::new(socket_path, app_handle);
    
    tokio::spawn(async move {
        if let Err(e) = ipc_server.start().await {
            eprintln!("IPC Server error: {}", e);
        }
    });

    // Normale Elane App
    App::new().run(|cx| {
        // ...
    });
}
```

## 3. Konfiguriere Claude Desktop

### macOS:
```bash
# Datei: ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "elane-inspector": {
      "command": "/absolute/path/to/gpui-mcp-inspector/target/release/gpui-mcp-server",
      "env": {
        "GPUI_MCP_SOCKET": "/tmp/elane-mcp.sock"
      }
    }
  }
}
```

### Linux:
```bash
# Datei: ~/.config/Claude/claude_desktop_config.json
# (gleicher Inhalt wie macOS)
```

## 4. Testen

1. **Starte Elane** (mit IPC Server Integration)
2. **Starte Claude Desktop**
3. **In Claude schreiben:**

```
Kannst du mir zeigen, wie die UI-Struktur von Elane aussieht?
```

Claude sollte jetzt das `inspect_ui_tree` Tool verwenden und dir die Struktur zeigen!

## Debugging

### IPC Server läuft nicht?

Prüfe ob der Socket existiert:
```bash
ls -la /tmp/elane-mcp.sock
```

### MCP Server verbindet nicht?

Debug-Output vom MCP Server ansehen:
```bash
# In Claude Desktop Config:
"command": "/path/to/gpui-mcp-server 2>/tmp/mcp-debug.log"

# Dann:
tail -f /tmp/mcp-debug.log
```

### Claude sieht die Tools nicht?

1. Claude Desktop neu starten
2. Prüfe Config-Datei Syntax (JSON muss valide sein)
3. Prüfe ob Binary existiert und ausführbar ist

## Tipps

- Der Socket-Path muss in beiden Orten übereinstimmen (Claude Config + Elane)
- IPC Server muss laufen BEVOR Claude Desktop startet
- Bei Änderungen am MCP Server: Claude Desktop neu starten
- Bei Änderungen an der Elane Integration: Elane neu starten

## Nächste Schritte

1. Implementiere `get_ui_tree()` richtig (GPUI Element Tree traversieren)
2. Implementiere `take_screenshot()` (Frame zu PNG)
3. Erweitere `AppInterface` mit Elane-spezifischen Actions
4. Füge mehr Debug-Informationen zu `get_app_state()` hinzu
