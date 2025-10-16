use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path as AxumPath, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use minijinja::{context, value::Value, Environment};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::Ipv6Addr,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::SystemTime,
};
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc, Mutex},
};
use tower_http::cors::CorsLayer;

const TEMPLATE_NAME: &str = "main.html";
const DIRECTORY_TEMPLATE_NAME: &str = "directory.html";
static TEMPLATE_ENV: OnceLock<Environment<'static>> = OnceLock::new();
const MERMAID_JS: &str = include_str!("../static/js/mermaid.min.js");
const MERMAID_ETAG: &str = concat!("\"", env!("CARGO_PKG_VERSION"), "\"");

// Trait for content sources (file or directory)
trait ContentSource: Send + Sync {
    fn refresh_if_needed(&mut self) -> Result<bool>;
    fn get_html(&self) -> String;
    fn get_raw_content(&self) -> Result<String>;
    fn get_base_dir(&self) -> &Path;
    fn subscribe_to_changes(&self) -> broadcast::Receiver<ServerMessage>;
}

// Type aliases for content sources
type FileContentSource = MarkdownState;
type SharedMarkdownState = Arc<Mutex<MarkdownState>>;
type SharedDirectoryState = Arc<Mutex<DirectoryState>>;

fn template_env() -> &'static Environment<'static> {
    TEMPLATE_ENV.get_or_init(|| {
        let mut env = Environment::new();
        minijinja_embed::load_templates!(&mut env);
        env
    })
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum ClientMessage {
    Ping,
    RequestRefresh,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum ServerMessage {
    Reload,
    Pong,
}

struct MarkdownState {
    file_path: PathBuf,
    base_dir: PathBuf,
    last_modified: SystemTime,
    cached_html: String,
    change_tx: broadcast::Sender<ServerMessage>,
}

#[derive(Clone)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    is_markdown: bool,
}

struct DirectoryState {
    root_dir: PathBuf,
    file_cache: std::collections::HashMap<PathBuf, CachedFile>,
    change_tx: broadcast::Sender<ServerMessage>,
}

struct CachedFile {
    last_modified: SystemTime,
    cached_html: String,
}

impl ContentSource for MarkdownState {
    fn refresh_if_needed(&mut self) -> Result<bool> {
        let metadata = fs::metadata(&self.file_path)?;
        let current_modified = metadata.modified()?;

        if current_modified > self.last_modified {
            let content = fs::read_to_string(&self.file_path)?;
            self.cached_html = Self::markdown_to_html(&content)?;
            self.last_modified = current_modified;

            // Send reload signal to all WebSocket clients
            let _ = self.change_tx.send(ServerMessage::Reload);

            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn get_html(&self) -> String {
        self.cached_html.clone()
    }

    fn get_raw_content(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.file_path)?)
    }

    fn get_base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn subscribe_to_changes(&self) -> broadcast::Receiver<ServerMessage> {
        self.change_tx.subscribe()
    }
}

impl MarkdownState {
    fn new(file_path: PathBuf) -> Result<Self> {
        let metadata = fs::metadata(&file_path)?;
        let last_modified = metadata.modified()?;
        let content = fs::read_to_string(&file_path)?;
        let cached_html = Self::markdown_to_html(&content)?;
        let (change_tx, _) = broadcast::channel::<ServerMessage>(16);

        let base_dir = file_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
            .canonicalize()
            .unwrap_or_else(|_| {
                file_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf()
            });

        Ok(MarkdownState {
            file_path,
            base_dir,
            last_modified,
            cached_html,
            change_tx,
        })
    }

    fn markdown_to_html(content: &str) -> Result<String> {
        // Create GFM options with HTML rendering enabled
        let mut options = markdown::Options::gfm();
        options.compile.allow_dangerous_html = true;

        let html_body = markdown::to_html_with_options(content, &options)
            .unwrap_or_else(|_| "Error parsing markdown".to_string());
        let has_mermaid = html_body.contains(r#"class="language-mermaid""#);

        let env = template_env();
        let template = env.get_template(TEMPLATE_NAME)?;
        let rendered = template.render(context! {
            content => Value::from_safe_string(html_body),
            mermaid_enabled => has_mermaid,
        })?;

        Ok(rendered)
    }
}

/// Creates a new Router for serving markdown files or directories.
///
/// # Errors
///
/// Returns an error if:
/// - The file/directory cannot be read or doesn't exist
/// - File metadata cannot be accessed
/// - File watcher cannot be created
/// - File watcher cannot watch the parent directory
pub fn new_router(path: impl AsRef<Path>) -> Result<Router> {
    let path = path.as_ref().to_path_buf();
    
    // Detect if path is a file or directory
    let metadata = fs::metadata(&path)?;
    
    if metadata.is_dir() {
        // For now, just create a simple router that accepts directories
        // We'll implement full directory serving in Phase 2
        new_directory_router(path)
    } else {
        new_file_router(path)
    }
}

fn new_file_router(file_path: PathBuf) -> Result<Router> {
    let watcher_file_path = file_path.clone();
    let state = Arc::new(Mutex::new(MarkdownState::new(file_path)?));

    // Set up file watcher
    let watcher_state = state.clone();
    let (tx, mut rx) = mpsc::channel(100);

    let mut watcher = RecommendedWatcher::new(
        move |res: std::result::Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        },
        Config::default(),
    )?;

    // Watch the parent directory to handle atomic writes
    let watch_path = watcher_file_path.parent().unwrap_or_else(|| Path::new("."));
    watcher.watch(watch_path, RecursiveMode::NonRecursive)?;

    // Spawn task to handle events and keep watcher alive
    tokio::spawn(async move {
        let _watcher = watcher; // Move watcher into task to keep it alive
        while let Some(event) = rx.recv().await {
            // Check if any of the event paths match our file
            let file_affected = event.paths.iter().any(|path| {
                path == &watcher_file_path
                    || (path.file_name() == watcher_file_path.file_name()
                        && path.parent() == watcher_file_path.parent())
            });

            if file_affected {
                // Only process modify/create events, ignore remove events
                match event.kind {
                    notify::EventKind::Modify(_) | notify::EventKind::Create(_) => {
                        let mut state = watcher_state.lock().await;
                        let _ = state.refresh_if_needed();
                    }
                    _ => {}
                }
            }
        }
    });

    let router = Router::new()
        .route("/", get(serve_html))
        .route("/raw", get(serve_raw))
        .route("/ws", get(websocket_handler))
        .route("/mermaid.min.js", get(serve_mermaid_js))
        .route("/*path", get(serve_static_file))
        .layer(CorsLayer::permissive())
        .with_state(state);

    Ok(router)
}

fn new_directory_router(dir_path: PathBuf) -> Result<Router> {
    let state = Arc::new(Mutex::new(DirectoryState::new(dir_path)?));
    
    let router = Router::new()
        .route("/", get(serve_directory_index))
        .route("/view/*path", get(serve_file_from_directory))
        .route("/raw/*path", get(serve_raw_from_directory))
        .route("/ws", get(websocket_handler_directory))
        .route("/mermaid.min.js", get(serve_mermaid_js))
        .route("/*path", get(serve_static_from_directory))
        .layer(CorsLayer::permissive())
        .with_state(state);
    
    Ok(router)
}

impl DirectoryState {
    fn new(root_dir: PathBuf) -> Result<Self> {
        let _metadata = fs::metadata(&root_dir)?;
        let (change_tx, _) = broadcast::channel::<ServerMessage>(16);
        
        Ok(DirectoryState {
            root_dir,
            file_cache: std::collections::HashMap::new(),
            change_tx,
        })
    }
    
    fn list_directory(&self, rel_path: &Path) -> Result<Vec<FileEntry>> {
        let full_path = self.root_dir.join(rel_path);
        let canonical = validate_path_within_root(&full_path, &self.root_dir)?;
        
        let mut entries = Vec::new();
        
        for entry in fs::read_dir(&canonical)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let name = entry.file_name().to_string_lossy().to_string();
            
            // Skip hidden files
            if name.starts_with('.') {
                continue;
            }
            
            let is_markdown = if metadata.is_file() {
                matches!(
                    entry.path().extension().and_then(|e| e.to_str()),
                    Some("md") | Some("markdown") | Some("txt")
                )
            } else {
                false
            };
            
            entries.push(FileEntry {
                name,
                path: entry.path(),
                is_dir: metadata.is_dir(),
                is_markdown,
            });
        }
        
        // Sort: directories first, then files alphabetically
        entries.sort_by(|a, b| {
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });
        
        Ok(entries)
    }
    
    fn render_file(&mut self, rel_path: &Path) -> Result<String> {
        let full_path = self.root_dir.join(rel_path);
        let canonical = validate_path_within_root(&full_path, &self.root_dir)?;
        
        // Check cache
        if let Some(cached) = self.file_cache.get(&canonical) {
            let metadata = fs::metadata(&canonical)?;
            let current_modified = metadata.modified()?;
            
            if current_modified <= cached.last_modified {
                return Ok(cached.cached_html.clone());
            }
        }
        
        // Read and render
        let content = fs::read_to_string(&canonical)?;
        let html = MarkdownState::markdown_to_html(&content)?;
        
        // Update cache
        let metadata = fs::metadata(&canonical)?;
        self.file_cache.insert(
            canonical,
            CachedFile {
                last_modified: metadata.modified()?,
                cached_html: html.clone(),
            },
        );
        
        Ok(html)
    }
    
    fn get_raw_file(&self, rel_path: &Path) -> Result<String> {
        let full_path = self.root_dir.join(rel_path);
        let canonical = validate_path_within_root(&full_path, &self.root_dir)?;
        
        Ok(fs::read_to_string(&canonical)?)
    }
}

async fn serve_directory_index(State(state): State<SharedDirectoryState>) -> impl IntoResponse {
    let state = state.lock().await;
    
    match state.list_directory(Path::new("")) {
        Ok(entries) => {
            // Convert entries to template format
            let template_entries: Vec<serde_json::Value> = entries
                .iter()
                .map(|entry| {
                    let icon = if entry.is_dir { "📁" } else { "📄" };
                    let link = if entry.is_dir {
                        format!("/view/{}/", entry.name)
                    } else {
                        format!("/view/{}", entry.name)
                    };
                    serde_json::json!({
                        "icon": icon,
                        "name": entry.name,
                        "link": link,
                    })
                })
                .collect();
            
            let env = template_env();
            let template = match env.get_template(DIRECTORY_TEMPLATE_NAME) {
                Ok(t) => t,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html(format!("<h1>Template Error</h1><p>{}</p>", e)),
                    );
                }
            };
            
            let rendered = match template.render(context! {
                title => "Directory Index",
                current_path => "",
                entries => template_entries,
            }) {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html(format!("<h1>Render Error</h1><p>{}</p>", e)),
                    );
                }
            };
            
            (StatusCode::OK, Html(rendered))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!("<h1>Error</h1><p>{}</p>", e)),
        ),
    }
}

async fn serve_file_from_directory(
    AxumPath(path): AxumPath<String>,
    State(state): State<SharedDirectoryState>,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    
    match state.render_file(Path::new(&path)) {
        Ok(html) => (StatusCode::OK, Html(html)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Html(format!("<h1>Not Found</h1><p>{}</p>", e)),
        ),
    }
}

async fn serve_raw_from_directory(
    AxumPath(path): AxumPath<String>,
    State(state): State<SharedDirectoryState>,
) -> impl IntoResponse {
    let state = state.lock().await;
    
    match state.get_raw_file(Path::new(&path)) {
        Ok(content) => (StatusCode::OK, content),
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!("Error: {}", e),
        ),
    }
}

async fn serve_static_from_directory(
    AxumPath(file_path): AxumPath<String>,
    State(state): State<SharedDirectoryState>,
) -> impl IntoResponse {
    let state = state.lock().await;

    if !is_image_file(&file_path) {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "File not found".to_string(),
        )
            .into_response();
    }

    let full_path = state.root_dir.join(&file_path);

    match full_path.canonicalize() {
        Ok(canonical_path) => {
            if !canonical_path.starts_with(&state.root_dir) {
                return (
                    StatusCode::FORBIDDEN,
                    [(header::CONTENT_TYPE, "text/plain")],
                    "Access denied".to_string(),
                )
                    .into_response();
            }

            match fs::read(&canonical_path) {
                Ok(contents) => {
                    let content_type = guess_image_content_type(&file_path);
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, content_type.as_str())],
                        contents,
                    )
                        .into_response()
                }
                Err(_) => (
                    StatusCode::NOT_FOUND,
                    [(header::CONTENT_TYPE, "text/plain")],
                    "File not found".to_string(),
                )
                    .into_response(),
            }
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "File not found".to_string(),
        )
            .into_response(),
    }
}

async fn websocket_handler_directory(
    ws: WebSocketUpgrade,
    State(state): State<SharedDirectoryState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket_directory(socket, state))
}

async fn handle_websocket_directory(socket: WebSocket, state: SharedDirectoryState) {
    let (mut sender, mut receiver) = socket.split();

    let mut change_rx = {
        let state = state.lock().await;
        state.change_tx.subscribe()
    };

    let recv_task = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                        match client_msg {
                            ClientMessage::Ping | ClientMessage::RequestRefresh => {}
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                _ => {}
            }
        }
    });

    let send_task = tokio::spawn(async move {
        while let Ok(reload_msg) = change_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&reload_msg) {
                if sender.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = recv_task => {},
        _ = send_task => {},
    }
}

/// Serves a markdown file with live reload support.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read or doesn't exist
/// - Cannot bind to the specified host address
/// - Server fails to start
/// - Axum serve encounters an error
pub async fn serve_markdown(
    file_path: impl AsRef<Path>,
    hostname: impl AsRef<str>,
    port: u16,
) -> Result<()> {
    let file_path = file_path.as_ref();
    let hostname = hostname.as_ref();

    let router = new_router(file_path)?;

    let listener = TcpListener::bind((hostname, port)).await?;

    let listen_addr = format_host(hostname, port);
    println!("📄 Serving markdown file: {}", file_path.display());
    println!("🌐 Server running at: http://{listen_addr}");
    println!("⚡ Live reload enabled");
    println!("\nPress Ctrl+C to stop the server");

    axum::serve(listener, router).await?;

    Ok(())
}

/// Format the host address (hostname + port) for printing.
fn format_host(hostname: &str, port: u16) -> String {
    if hostname.parse::<Ipv6Addr>().is_ok() {
        format!("[{hostname}]:{port}")
    } else {
        format!("{hostname}:{port}")
    }
}

async fn serve_html(State(state): State<SharedMarkdownState>) -> impl IntoResponse {
    let mut state = state.lock().await;
    if let Err(e) = state.refresh_if_needed() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!("<h1>Error</h1><p>{e}</p>")),
        );
    }

    (StatusCode::OK, Html(state.get_html()))
}

async fn serve_raw(State(state): State<SharedMarkdownState>) -> impl IntoResponse {
    let state = state.lock().await;
    match state.get_raw_content() {
        Ok(content) => (StatusCode::OK, content),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Error reading file: {e}"),
        ),
    }
}

async fn serve_mermaid_js(headers: HeaderMap) -> impl IntoResponse {
    // Check if client has current version cached
    if is_etag_match(&headers) {
        return mermaid_response(StatusCode::NOT_MODIFIED, None);
    }

    mermaid_response(StatusCode::OK, Some(MERMAID_JS))
}

fn is_etag_match(headers: &HeaderMap) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|etags| etags.split(',').any(|tag| tag.trim() == MERMAID_ETAG))
}

fn mermaid_response(status: StatusCode, body: Option<&'static str>) -> impl IntoResponse {
    // Use no-cache to force revalidation on each request. This ensures clients
    // get updated content when mdserve is rebuilt with a new Mermaid version,
    // while still benefiting from 304 responses via ETag matching.
    let headers = [
        (header::CONTENT_TYPE, "application/javascript"),
        (header::ETAG, MERMAID_ETAG),
        (header::CACHE_CONTROL, "public, no-cache"),
    ];

    match body {
        Some(content) => (status, headers, content).into_response(),
        None => (status, headers).into_response(),
    }
}

async fn serve_static_file(
    AxumPath(file_path): AxumPath<String>,
    State(state): State<SharedMarkdownState>,
) -> impl IntoResponse {
    let state = state.lock().await;

    // Only serve image files
    if !is_image_file(&file_path) {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "File not found".to_string(),
        )
            .into_response();
    }

    // Construct the full path by joining base_dir with the requested path
    let full_path = state.get_base_dir().join(&file_path);

    // Security check: ensure the resolved path is still within base_dir
    match full_path.canonicalize() {
        Ok(canonical_path) => {
            if !canonical_path.starts_with(state.get_base_dir()) {
                return (
                    StatusCode::FORBIDDEN,
                    [(header::CONTENT_TYPE, "text/plain")],
                    "Access denied".to_string(),
                )
                    .into_response();
            }

            // Try to read and serve the file
            match fs::read(&canonical_path) {
                Ok(contents) => {
                    let content_type = guess_image_content_type(&file_path);
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, content_type.as_str())],
                        contents,
                    )
                        .into_response()
                }
                Err(_) => (
                    StatusCode::NOT_FOUND,
                    [(header::CONTENT_TYPE, "text/plain")],
                    "File not found".to_string(),
                )
                    .into_response(),
            }
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "File not found".to_string(),
        )
            .into_response(),
    }
}

/// Validates that a path is within the root directory (prevents path traversal)
fn validate_path_within_root(path: &Path, root: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    
    if !canonical.starts_with(root) {
        anyhow::bail!("Access denied: path outside root directory");
    }
    
    Ok(canonical)
}

fn is_image_file(file_path: &str) -> bool {
    let extension = std::path::Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    matches!(
        extension.to_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "ico"
    )
}

fn guess_image_content_type(file_path: &str) -> String {
    let extension = std::path::Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    match extension.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
    .to_string()
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedMarkdownState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

async fn handle_websocket(socket: WebSocket, state: SharedMarkdownState) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to file change notifications
    let mut change_rx = {
        let state = state.lock().await;
        state.subscribe_to_changes()
    };

    // Spawn task to handle incoming messages from client
    let recv_task = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                        match client_msg {
                            ClientMessage::Ping | ClientMessage::RequestRefresh => {
                                // Currently no special handling needed
                            }
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                _ => {}
            }
        }
    });

    // Spawn task to send messages to client
    let send_task = tokio::spawn(async move {
        // Listen for file changes
        while let Ok(reload_msg) = change_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&reload_msg) {
                if sender.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    // Wait for either task to complete
    tokio::select! {
        _ = recv_task => {},
        _ = send_task => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_file_content_source_implements_trait() {
        let temp_file = NamedTempFile::new().expect("Failed to create temp file");
        fs::write(&temp_file, "# Test").expect("Failed to write");

        let source = FileContentSource::new(temp_file.path().to_path_buf());
        assert!(source.is_ok(), "FileContentSource should be created");
    }

    #[test]
    fn test_file_content_source_get_html() {
        let temp_file = NamedTempFile::new().expect("Failed to create temp file");
        fs::write(&temp_file, "# Hello\n\n**Bold**").expect("Failed to write");

        let source = FileContentSource::new(temp_file.path().to_path_buf())
            .expect("Failed to create source");

        let html = source.get_html();
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<strong>Bold</strong>"));
    }

    #[test]
    fn test_file_content_source_get_raw_content() {
        let temp_file = NamedTempFile::new().expect("Failed to create temp file");
        let content = "# Raw Content\n\nTest";
        fs::write(&temp_file, content).expect("Failed to write");

        let source = FileContentSource::new(temp_file.path().to_path_buf())
            .expect("Failed to create source");

        let raw = source.get_raw_content().expect("Failed to get raw content");
        assert_eq!(raw, content);
    }

    #[test]
    fn test_file_content_source_refresh_detects_changes() {
        let temp_file = NamedTempFile::new().expect("Failed to create temp file");
        fs::write(&temp_file, "# Original").expect("Failed to write");

        let mut source = FileContentSource::new(temp_file.path().to_path_buf())
            .expect("Failed to create source");

        // First check - no changes
        let changed = source.refresh_if_needed().expect("Refresh failed");
        assert!(!changed, "Should not detect changes on first check");

        // Modify file
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&temp_file, "# Modified").expect("Failed to modify");

        // Second check - should detect changes
        let changed = source.refresh_if_needed().expect("Refresh failed");
        assert!(changed, "Should detect file modification");

        // Verify content updated
        let html = source.get_html();
        assert!(html.contains("<h1>Modified</h1>"));
    }

    #[test]
    fn test_content_source_trait_object() {
        let temp_file = NamedTempFile::new().expect("Failed to create temp file");
        fs::write(&temp_file, "# Trait Test").expect("Failed to write");

        let source: Box<dyn ContentSource> =
            Box::new(FileContentSource::new(temp_file.path().to_path_buf()).unwrap());

        let html = source.get_html();
        assert!(html.contains("<h1>Trait Test</h1>"));
    }
}
