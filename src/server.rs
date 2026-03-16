use crate::protocol::*;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::sync::{mpsc, Arc, Mutex};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

/// Interface das die GPUI App implementieren muss.
/// Damit der IPC Server Zugriff auf UI State hat.
pub trait AppInterface: Send + Sync + 'static {
    fn get_ui_tree(&self) -> UiTree;
    fn get_element(&self, id: &str) -> Option<UiElement>;
    fn get_windows(&self) -> Vec<WindowInfo>;
    fn take_screenshot(&self, params: &TakeScreenshotParams) -> Screenshot;
    fn click_element(&self, event: &ClickEvent) -> bool;
    fn send_key(&self, event: &KeyEvent) -> bool;
    fn execute_action(&self, params: &ExecuteActionParams) -> serde_json::Value;
    fn get_app_state(&self) -> serde_json::Value;
    fn get_logs(&self) -> Vec<String>;
}

/// IPC Server für GPUI App.
/// Läuft auf einem Background-Thread und handelt Requests vom MCP Server.
pub struct IpcServer {
    socket_path: String,
    app: Arc<Mutex<Box<dyn AppInterface>>>,
}

impl IpcServer {
    pub fn new(socket_path: String, app: Arc<Mutex<Box<dyn AppInterface>>>) -> Self {
        Self { socket_path, app }
    }

    /// Startet den IPC Server auf einem Background-Thread.
    /// Gibt einen JoinHandle zurück mit dem der Thread gestoppt werden kann.
    pub fn start(self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            if let Err(e) = self.run() {
                eprintln!("IPC Server error: {}", e);
            }
        })
    }

    fn run(&self) -> anyhow::Result<()> {
        // Remove stale socket
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;
        eprintln!("IPC Server listening on {}", self.socket_path);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let app = self.app.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = Self::handle_connection(stream, app) {
                            eprintln!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Accept error: {}", e);
                }
            }
        }

        Ok(())
    }

    fn handle_connection(
        stream: UnixStream,
        app: Arc<Mutex<Box<dyn AppInterface>>>,
    ) -> anyhow::Result<()> {
        let reader = BufReader::new(&stream);
        let mut writer = &stream;

        for line in reader.lines() {
            let line = line?;
            let request: IpcRequest = serde_json::from_str(&line)?;

            let result = Self::handle_request(&request, &app);

            let response = IpcResponse {
                id: request.id,
                result,
            };

            let response_json = serde_json::to_string(&response)?;
            writer.write_all(response_json.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }

        Ok(())
    }

    fn handle_request(
        request: &IpcRequest,
        app: &Arc<Mutex<Box<dyn AppInterface>>>,
    ) -> Result<serde_json::Value, String> {
        let app = app.lock().map_err(|e| e.to_string())?;

        match request.method.as_str() {
            methods::INSPECT_UI_TREE => {
                let tree = app.get_ui_tree();
                serde_json::to_value(&tree).map_err(|e| e.to_string())
            }

            methods::GET_ELEMENT => {
                let params: GetElementParams =
                    serde_json::from_value(request.params.clone()).map_err(|e| e.to_string())?;

                match app.get_element(&params.element_id) {
                    Some(element) => serde_json::to_value(&element).map_err(|e| e.to_string()),
                    None => Err(format!("Element not found: {}", params.element_id)),
                }
            }

            methods::GET_WINDOWS => {
                let windows = app.get_windows();
                serde_json::to_value(&windows).map_err(|e| e.to_string())
            }

            methods::TAKE_SCREENSHOT => {
                let params: TakeScreenshotParams =
                    serde_json::from_value(request.params.clone()).map_err(|e| e.to_string())?;

                let screenshot = app.take_screenshot(&params);
                serde_json::to_value(&screenshot).map_err(|e| e.to_string())
            }

            methods::CLICK_ELEMENT => {
                let event: ClickEvent =
                    serde_json::from_value(request.params.clone()).map_err(|e| e.to_string())?;

                let success = app.click_element(&event);
                Ok(json!({ "success": success }))
            }

            methods::SEND_KEY => {
                let event: KeyEvent =
                    serde_json::from_value(request.params.clone()).map_err(|e| e.to_string())?;

                let success = app.send_key(&event);
                Ok(json!({ "success": success }))
            }

            methods::EXECUTE_ACTION => {
                let params: ExecuteActionParams =
                    serde_json::from_value(request.params.clone()).map_err(|e| e.to_string())?;

                let result = app.execute_action(&params);
                Ok(result)
            }

            methods::GET_APP_STATE => {
                let state = app.get_app_state();
                Ok(state)
            }

            methods::GET_LOGS => {
                let logs = app.get_logs();
                Ok(json!({ "logs": logs }))
            }

            _ => Err(format!("Unknown method: {}", request.method)),
        }
    }
}

/// Convenience: Startet den IPC Server mit Default-Socket-Pfad (PID-basiert).
pub fn start_ipc_server(app: Box<dyn AppInterface>) -> std::thread::JoinHandle<()> {
    let socket_path = std::env::var("GPUI_MCP_SOCKET").unwrap_or_else(|_| {
        let pid = std::process::id();
        let dir = std::env::temp_dir();
        dir.join(format!("gpui-mcp-{}.sock", pid))
            .to_string_lossy()
            .into_owned()
    });

    let server = IpcServer::new(socket_path, Arc::new(Mutex::new(app)));
    server.start()
}

/// Typ für Request-Channel zwischen IPC-Thread und Main-Thread.
/// Der IPC-Thread sendet Requests und wartet auf die Response via oneshot.
pub type RequestChannel = (
    mpsc::Sender<(IpcRequest, mpsc::Sender<IpcResponse>)>,
    mpsc::Receiver<(IpcRequest, mpsc::Sender<IpcResponse>)>,
);

/// Erstellt einen Channel für die Kommunikation zwischen IPC-Thread und Main-Thread.
/// Nützlich wenn die AppInterface-Methoden auf dem Main-Thread laufen müssen (z.B. GPUI).
pub fn create_request_channel() -> RequestChannel {
    mpsc::channel()
}

/// IPC Server der Requests über einen Channel an den Main-Thread weiterleitet.
/// Wird für GPUI Apps gebraucht, da UI-Zugriff nur auf dem Main-Thread möglich ist.
pub struct ChannelIpcServer {
    socket_path: String,
    sender: mpsc::Sender<(IpcRequest, mpsc::Sender<IpcResponse>)>,
}

impl ChannelIpcServer {
    pub fn new(
        socket_path: String,
        sender: mpsc::Sender<(IpcRequest, mpsc::Sender<IpcResponse>)>,
    ) -> Self {
        Self {
            socket_path,
            sender,
        }
    }

    /// Startet den IPC Server auf einem Background-Thread.
    pub fn start(self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            if let Err(e) = self.run() {
                eprintln!("IPC Server error: {}", e);
            }
        })
    }

    fn run(&self) -> anyhow::Result<()> {
        // Remove stale socket
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;
        eprintln!("IPC Server listening on {}", self.socket_path);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let sender = self.sender.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = Self::handle_connection(stream, sender) {
                            eprintln!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Accept error: {}", e);
                }
            }
        }

        Ok(())
    }

    fn handle_connection(
        stream: UnixStream,
        sender: mpsc::Sender<(IpcRequest, mpsc::Sender<IpcResponse>)>,
    ) -> anyhow::Result<()> {
        let reader = BufReader::new(&stream);
        let mut writer = &stream;

        for line in reader.lines() {
            let line = line?;
            let request: IpcRequest = serde_json::from_str(&line)?;

            // Response-Channel erstellen (oneshot-Pattern via mpsc mit Kapazität 1)
            let (resp_tx, resp_rx) = mpsc::channel();

            // Request an Main-Thread senden
            sender.send((request, resp_tx)).map_err(|e| {
                anyhow::anyhow!("Failed to send request to main thread: {}", e)
            })?;

            // Auf Response warten
            let response = resp_rx.recv().map_err(|e| {
                anyhow::anyhow!("Failed to receive response from main thread: {}", e)
            })?;

            let response_json = serde_json::to_string(&response)?;
            writer.write_all(response_json.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }

        Ok(())
    }
}
