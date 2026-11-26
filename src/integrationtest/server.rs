//! The server a case is asked of, and the LSP conversation with it.
//!
//! The server is this crate's own, run inside the test process and reached
//! through an in-memory pipe: no executable is spawned, and the sources
//! arrive as `didOpen` text. Only a fixture with a file the editor does not
//! open (a build file, a source marked `(not open)`) is written to disk, in
//! a directory of its own that the server is told is the workspace.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::{timeout_at, Instant};
use tower_lsp::lsp_types::Url;
use tower_lsp::Server;

use super::fixture::{Fixture, FixtureFile};
use crate::indexer::library::Libraries;
use crate::indexer::Indexer;

/// Where the server is told its workspace is. Nothing is there.
const MEMORY_ROOT: &str = "/javak-lsp-integrationtest";
const PIPE_CAPACITY: usize = 1 << 20;
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long a case waits for an answer before it takes "nothing" for one.
const PATIENCE: Duration = Duration::from_secs(5);
/// The same, for a server in another process: a JVM answers its first
/// requests while it is still warming up.
const CHILD_PATIENCE: Duration = Duration::from_secs(30);
/// How often a server's import says it is still going.
const HEARTBEAT: Duration = Duration::from_secs(15);
/// How long pushed diagnostics must stay unchanged to be the answer.
const QUIET: Duration = Duration::from_secs(3);
/// `FileChangeType` of `workspace/didChangeWatchedFiles`.
const CREATED: u8 = 1;
const DELETED: u8 = 3;

type Reader = Box<dyn AsyncRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// The task serving a session, aborted with it.
struct Host(tokio::task::JoinHandle<()>);

impl Drop for Host {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What a case makes of a request the server answers with an error.
#[derive(Clone, Copy)]
pub(super) enum OnError {
    /// The request is one the server must answer: the case fails.
    Panic,
    /// The error is the server's way of answering "nothing here".
    AnswerNothing,
    /// As [`OnError::AnswerNothing`], except an internal error (`-32603`):
    /// the server failed, which is answered as [`failure`] says.
    AnswerNothingUnlessItFailed,
}

/// The key of the answer [`OnError::AnswerNothingUnlessItFailed`] gives for
/// a request the server failed: `{"failed": "<what it said>"}`.
pub(super) const FAILED: &str = "failed";

/// What the server said when it failed to answer, where it did.
pub(super) fn failure(answer: &Value) -> Option<&str> {
    answer.get(FAILED).and_then(Value::as_str)
}

/// An initialized server with every file of a fixture open.
pub(super) struct Session {
    writer: Writer,
    incoming: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    /// The URIs the server has published diagnostics for.
    diagnosed: HashSet<String>,
    /// The latest diagnostics each URI was published with, for a server
    /// that pushes them rather than answering `textDocument/diagnostic`.
    published: HashMap<String, Value>,
    /// Where the fixture's files live, as the server sees them.
    root: Url,
    /// What `initialize` is told the workspace is: the same as [`Self::root`]
    /// in memory, the project above it on disk.
    project: Url,
    /// Where [`Self::load`] writes the fixture, when the server reads files
    /// from a disk rather than from `didOpen` text.
    disk: Option<PathBuf>,
    /// How long one request may take. A JVM needs longer than a thread.
    patience: Duration,
    /// What the server said it can do in its `initialize` answer.
    capabilities: Value,
    _host: Option<Host>,
    _child: Option<tokio::process::Child>,
    /// The directory an in-memory server reads unopened files from, removed
    /// with the session.
    _scratch: Option<tempfile::TempDir>,
}

impl Session {
    pub(super) async fn open(fixture: &Fixture) -> Self {
        // A file the editor does not open is one the server reads from
        // disk: the whole fixture goes there, as a project would be.
        let scratch = fixture.files.iter().any(|file| !file.is_opened()).then(|| {
            let directory = tempfile::tempdir().expect("a temporary directory");
            for file in &fixture.files {
                write_file(directory.path(), file);
            }
            directory
        });
        let workspace = scratch
            .as_ref()
            .map_or_else(|| PathBuf::from(MEMORY_ROOT), |directory| directory.path().to_owned());
        let (reader, writer, root, host) = in_memory(workspace);
        let mut session = Self {
            writer,
            incoming: frames(reader),
            next_id: 0,
            diagnosed: HashSet::new(),
            published: HashMap::new(),
            project: root.clone(),
            root,
            disk: None,
            patience: PATIENCE,
            capabilities: Value::Null,
            _host: Some(host),
            _child: None,
            _scratch: scratch,
        };
        session.initialize().await;
        session.load(fixture).await;
        session
    }

    /// Another server, run as `program args…`, reading the fixture from
    /// `source_root` inside the project at `project_root`.
    ///
    /// The process outlives one case: its JVM, its JDK scan and its index
    /// cost more than every case put together, so one session is loaded
    /// and unloaded ([`Self::load`]) once per case instead.
    pub(super) async fn spawn(
        program: &str,
        arguments: &[String],
        project_root: &Path,
        source_root: &Path,
    ) -> Self {
        let mut child = tokio::process::Command::new(program)
            .args(arguments)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|error| panic!("{program} starts: {error}"));
        let reader = child.stdout.take().expect("a piped stdout");
        let writer = child.stdin.take().expect("a piped stdin");
        let mut session = Self {
            writer: Box::new(writer),
            incoming: frames(Box::new(reader)),
            next_id: 0,
            diagnosed: HashSet::new(),
            published: HashMap::new(),
            root: directory_url(source_root),
            project: directory_url(project_root),
            disk: Some(source_root.to_owned()),
            patience: CHILD_PATIENCE,
            capabilities: Value::Null,
            _host: None,
            _child: Some(child),
            _scratch: None,
        };
        session.initialize().await;
        session
    }

    /// Another server, run as `program args…` inside a real project at
    /// `project_root`: the project's files are already on disk, every path
    /// is read from the project down, and nothing is written. Documents are
    /// opened one at a time ([`Self::open_document`]).
    pub(super) async fn spawn_in_project(
        program: &str,
        arguments: &[String],
        project_root: &Path,
        stderr: std::process::Stdio,
        patience: Duration,
    ) -> Self {
        let mut child = tokio::process::Command::new(program)
            .args(arguments)
            .current_dir(project_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|error| panic!("{program} starts: {error}"));
        let reader = child.stdout.take().expect("a piped stdout");
        let writer = child.stdin.take().expect("a piped stdin");
        let root = directory_url(project_root);
        let mut session = Self {
            writer: Box::new(writer),
            incoming: frames(Box::new(reader)),
            next_id: 0,
            diagnosed: HashSet::new(),
            published: HashMap::new(),
            project: root.clone(),
            root,
            disk: None,
            patience,
            capabilities: Value::Null,
            _host: None,
            _child: Some(child),
            _scratch: None,
        };
        session.initialize().await;
        session
    }

    /// Waits until the server says it has read the project, then until it
    /// has been quiet for a moment. A message (`language/status`, a log or a
    /// shown message) containing one of `markers`' texts is the signal; each
    /// marker names how long the quiet after it must last. `false` when
    /// `deadline` passes first, or the server goes away.
    ///
    /// What the server says meanwhile is printed as `[label] …`, and a line
    /// every [`HEARTBEAT`] says how long it has been, so a long import is
    /// seen to be moving.
    pub(super) async fn wait_until_ready(
        &mut self,
        label: &str,
        markers: &[(&str, Duration)],
        deadline: Instant,
    ) -> bool {
        let started = Instant::now();
        let mut heartbeat = started + HEARTBEAT;
        let mut quiet: Option<(Duration, Instant)> = None;
        let mut last_said = String::new();
        loop {
            let now = Instant::now();
            if let Some((_, until)) = quiet {
                if now >= until {
                    return true;
                }
            }
            if now >= deadline {
                eprintln!("    [{label}] not ready after {}s", started.elapsed().as_secs());
                return quiet.is_some();
            }
            if now >= heartbeat {
                eprintln!("    [{label}] still importing… {}s", started.elapsed().as_secs());
                heartbeat = now + HEARTBEAT;
            }
            let wait = quiet
                .map_or(deadline, |(_, until)| until.min(deadline))
                .min(heartbeat);
            let message = match timeout_at(wait, self.incoming.recv()).await {
                Err(_) => continue,
                Ok(None) => {
                    eprintln!("    [{label}] the server went away");
                    return false;
                }
                Ok(Some(message)) => message,
            };
            let said = what_it_says(&message);
            if !said.is_empty() && said != last_said {
                eprintln!("    [{label}] {said}");
                last_said = said.clone();
            }
            let text = format!(
                "{} {}",
                message["params"]["type"].as_str().unwrap_or_default(),
                message["params"]["message"].as_str().unwrap_or_default()
            );
            if let Some((_, after)) = markers.iter().find(|(marker, _)| text.contains(marker)) {
                // The latest marker says how long to wait: a server that
                // says it has more to do ("decompiling…") is waited for.
                quiet = Some((*after, Instant::now() + *after));
            } else if let Some((after, _)) = quiet {
                // Still talking: the quiet starts again.
                quiet = Some((after, Instant::now() + after));
            }
            self.answer_server(&message).await;
        }
    }

    /// Opens the file at `path` (below the root) with `text`. Diagnostics
    /// published for it before are forgotten, so the next ones are fresh.
    pub(super) async fn open_document(&mut self, path: &str, text: &str) {
        self.published.remove(self.uri(path).as_str());
        self.did_open(path, text).await;
    }

    /// Replaces the open file's text, as an editor with full sync does.
    pub(super) async fn change_document(&mut self, path: &str, version: i32, text: &str) {
        let uri = self.uri(path);
        self.published.remove(uri.as_str());
        let params = json!({
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"text": text}],
        });
        self.notify("textDocument/didChange", params).await;
    }

    pub(super) async fn close_document(&mut self, path: &str) {
        let document = json!({"uri": self.uri(path)});
        self.notify("textDocument/didClose", json!({"textDocument": document}))
            .await;
        self.published.remove(self.uri(path).as_str());
    }

    /// Asks the server to stop, without waiting long for it.
    pub(super) async fn shutdown(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        self.request("shutdown", Value::Null, OnError::AnswerNothing, deadline)
            .await;
        self.notify("exit", Value::Null).await;
    }

    /// Puts the fixture in front of the server and waits until it has read
    /// it: on disk where there is one, and the files an editor would have
    /// open opened.
    pub(super) async fn load(&mut self, fixture: &Fixture) {
        let files = self.served(fixture);
        if let Some(disk) = self.disk.clone() {
            for file in &files {
                write_file(&disk, file);
            }
            self.watched(&files, CREATED).await;
        }
        let opened: Vec<&FixtureFile> = files.into_iter().filter(|file| file.is_opened()).collect();
        for file in &opened {
            self.did_open(&file.path, &file.text).await;
        }
        self.wait_until_read(&opened).await;
    }

    /// Takes the fixture away again, so the next case's `com.demo.Main` is
    /// the only one the server knows.
    pub(super) async fn unload(&mut self, fixture: &Fixture) {
        let files = self.served(fixture);
        for file in files.iter().filter(|file| file.is_opened()) {
            let document = json!({"uri": self.uri(&file.path)});
            self.notify("textDocument/didClose", json!({"textDocument": document}))
                .await;
        }
        if let Some(disk) = self.disk.clone() {
            for file in &files {
                let _ = std::fs::remove_file(disk.join(&file.path));
            }
            self.watched(&files, DELETED).await;
        }
        self.diagnosed.clear();
        self.published.clear();
    }

    /// Whether the server answers `textDocument/diagnostic` (pull), as it
    /// said in its `initialize` answer; otherwise it only pushes them.
    pub(super) fn pulls_diagnostics(&self) -> bool {
        !self.capabilities["diagnosticProvider"].is_null()
    }

    /// The token types and modifiers the server's semantic tokens index.
    pub(super) fn semantic_legend(&self) -> &Value {
        &self.capabilities["semanticTokensProvider"]["legend"]
    }

    /// Whether the server runs in another process: one that is not this
    /// crate's, so it may only push diagnostics (jdtls does).
    pub(super) fn is_child(&self) -> bool {
        self._child.is_some()
    }

    /// The diagnostics last pushed for `path`, waited for the way
    /// [`Self::request_until`] waits for an answer: until `settled` accepts
    /// them and nothing new has arrived for a moment, or patience runs out.
    /// A server builds before it reports, and reports again as it learns
    /// more, so the last word is the one that counts.
    pub(super) async fn pushed_diagnostics(
        &mut self,
        path: &str,
        settled: impl Fn(&Value) -> bool,
    ) -> Value {
        let uri = self.uri(path).to_string();
        let deadline = Instant::now() + self.patience;
        let mut quiet_until = Instant::now() + QUIET;
        loop {
            let current = self.published.get(&uri).cloned().unwrap_or(Value::Null);
            let now = Instant::now();
            if now >= deadline || (settled(&current) && now >= quiet_until) {
                return current;
            }
            let wait = if settled(&current) { quiet_until } else { deadline };
            match self.receive(wait).await {
                Some(message) => {
                    if message["method"] == "textDocument/publishDiagnostics" {
                        quiet_until = Instant::now() + QUIET;
                    }
                    self.answer_server(&message).await;
                }
                None if Instant::now() >= deadline => return current,
                None => {}
            }
        }
    }

    /// The fixture's files this server is handed. A case gives this crate's
    /// server, which runs without a JDK, the JDK classes it uses as files
    /// of their own (`/java/lang/Deprecated.java`); a server with a real
    /// JDK refuses a second `java.lang`, so it is given its own instead.
    fn served<'a>(&self, fixture: &'a Fixture) -> Vec<&'a FixtureFile> {
        fixture
            .files
            .iter()
            .filter(|file| {
                self.disk.is_none()
                    || !(file.path.starts_with("java/") || file.path.starts_with("javax/"))
            })
            .collect()
    }

    /// Tells the server the fixture's files appeared or went away, the way
    /// an editor would: a server that reads a disk indexes what it is told
    /// about, not what it is handed.
    async fn watched(&mut self, files: &[&FixtureFile], change: u8) {
        let changes: Vec<Value> = files
            .iter()
            .map(|file| json!({"uri": self.uri(&file.path), "type": change}))
            .collect();
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": changes}),
        )
        .await;
    }

    /// Reads the fixture's paths from `directory` from now on: a case that
    /// spells its source root itself (`/src/main/java/…`) is placed at the
    /// project, every other one under the source root.
    pub(super) fn place_at(&mut self, directory: &Path) {
        self.root = directory_url(directory);
        if self.disk.is_some() {
            self.disk = Some(directory.to_owned());
        }
    }

    pub(super) fn uri(&self, path: &str) -> Url {
        self.root.join(path).expect("a fixture path is relative")
    }

    /// `uri` as the fixture spelled it, or, outside the workspace, as
    /// [`library_place`] names it.
    pub(super) fn display(&self, uri: &str) -> String {
        if let Some(below_root) = uri.strip_prefix(self.root.as_str()) {
            return format!("/{below_root}");
        }
        library_place(uri).unwrap_or_else(|| uri.to_owned())
    }

    /// The `result` of `method`, asked again until `settled` accepts it or
    /// patience runs out: a server answers `null` or a partial list while it
    /// is still reading what was just opened, and a client asks again.
    pub(super) async fn request_until(
        &mut self,
        method: &str,
        params: Value,
        on_error: OnError,
        settled: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + self.patience;
        loop {
            let result = self
                .request(method, params.clone(), on_error, deadline)
                .await;
            if settled(&result) || Instant::now() >= deadline {
                return result;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Opening a file only hands it over; the server reads it afterwards,
    /// and reports the file's diagnostics once it has. Waits for those, for
    /// every file, or until patience runs out.
    async fn wait_until_read(&mut self, files: &[&FixtureFile]) {
        let deadline = Instant::now() + self.patience;
        let all_read = |session: &Self| {
            files
                .iter()
                .all(|file| session.diagnosed.contains(session.uri(&file.path).as_str()))
        };
        while !all_read(self) {
            let Some(message) = self.receive(deadline).await else {
                return;
            };
            self.answer_server(&message).await;
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        on_error: OnError,
        deadline: Instant,
    ) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let message = self.receive(deadline).await;
            let Some(message) = message else {
                return Value::Null;
            };
            if message.get("method").is_some() {
                self.answer_server(&message).await;
            } else if message["id"] == json!(id) {
                if let Some(error) = message.get("error") {
                    match on_error {
                        OnError::Panic => panic!("{method} failed: {error}"),
                        OnError::AnswerNothing => return Value::Null,
                        OnError::AnswerNothingUnlessItFailed => {
                            if error["code"] != json!(-32603) {
                                return Value::Null;
                            }
                            // The message says little ("Internal error");
                            // the first line of the data names the exception.
                            let data = error["data"].as_str().unwrap_or_default();
                            let cause = data
                                .lines()
                                .next()
                                .unwrap_or_default()
                                .trim_start_matches("java.util.concurrent.CompletionException: ");
                            let said = if cause.is_empty() {
                                error["message"].as_str().unwrap_or("internal error")
                            } else {
                                cause
                            };
                            return json!({ FAILED: said });
                        }
                    }
                }
                return message.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    async fn send(&mut self, message: Value) {
        let body = message.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.writer
            .write_all(frame.as_bytes())
            .await
            .expect("the server reads what it is sent");
        self.writer.flush().await.expect("the pipe flushes");
    }

    /// The next message, or `None` once `deadline` passes or the server is gone.
    async fn receive(&mut self, deadline: Instant) -> Option<Value> {
        let message = timeout_at(deadline, self.incoming.recv())
            .await
            .ok()
            .flatten()?;
        if message["method"] == "textDocument/publishDiagnostics" {
            if let Some(uri) = message["params"]["uri"].as_str() {
                self.diagnosed.insert(uri.to_owned());
                self.published
                    .insert(uri.to_owned(), message["params"]["diagnostics"].clone());
            }
        }
        Some(message)
    }

    /// Servers ask their client things (`workspace/configuration`,
    /// `client/registerCapability`) and wait: every one gets an empty answer.
    async fn answer_server(&mut self, request: &Value) {
        let Some(id) = request.get("id") else {
            return;
        };
        let sections = request["params"]["items"].as_array().map_or(0, Vec::len);
        let result = match request["method"].as_str() {
            Some("workspace/configuration") => json!(vec![Value::Null; sections]),
            _ => Value::Null,
        };
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    async fn initialize(&mut self) {
        let deadline = Instant::now() + self.patience;
        let params = json!({
            "processId": std::process::id(),
            "rootUri": self.project.as_str().trim_end_matches('/'),
            "workspaceFolders": [{
                "uri": self.project.as_str().trim_end_matches('/'),
                "name": "fixture",
            }],
            // jdtls answers signature help only when it is switched on.
            "initializationOptions": {
                "settings": {"java": {
                    "signatureHelp": {"enabled": true},
                    "configuration": java_runtimes(),
                }},
                // Without it jdtls answers nothing for a declaration in the
                // JDK or a library, which an editor opens through `jdt://`.
                "extendedClientCapabilities": {"classFileContentsSupport": true},
            },
            "capabilities": {
                "textDocument": {
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "rename": {"prepareSupport": true},
                    "signatureHelp": {},
                    "codeAction": {
                        "codeActionLiteralSupport": {"codeActionKind": {"valueSet": [
                            "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                            "refactor.rewrite", "source", "source.organizeImports",
                        ]}},
                    },
                    "inlayHint": {},
                    "codeLens": {},
                    "callHierarchy": {},
                    "typeHierarchy": {},
                    "selectionRange": {},
                    "formatting": {},
                    "rangeFormatting": {},
                    "semanticTokens": {
                        "requests": {"full": true},
                        "tokenTypes": [],
                        "tokenModifiers": [],
                        "formats": ["relative"],
                    },
                },
                // Progress while a project is imported, for whoever watches.
                "window": {"workDoneProgress": true},
                "workspace": {
                    "workspaceEdit": {"documentChanges": true},
                    "symbol": {},
                    "configuration": true,
                    "didChangeWatchedFiles": {"dynamicRegistration": true},
                    "workspaceFolders": true,
                },
            },
        });
        let answer = self
            .request("initialize", params, OnError::Panic, deadline)
            .await;
        self.capabilities = answer["capabilities"].clone();
        self.notify("initialized", json!({})).await;
    }

    async fn did_open(&mut self, path: &str, text: &str) {
        let language = Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map_or("java", |extension| match extension {
                "kt" | "kts" => "kotlin",
                "swift" => "swift",
                _ => "java",
            });
        let document = json!({
            "uri": self.uri(path),
            "languageId": language,
            "version": 1,
            "text": text,
        });
        self.notify("textDocument/didOpen", json!({"textDocument": document}))
            .await;
    }
}

/// What a message from the server tells a person watching: a status, a log
/// line, a progress report. Empty for everything else.
fn what_it_says(message: &Value) -> String {
    let params = &message["params"];
    let text = |value: &Value| value.as_str().unwrap_or_default().trim().to_owned();
    match message["method"].as_str().unwrap_or_default() {
        "window/logMessage" | "window/showMessage" => text(&params["message"]),
        "language/status" => format!("{}: {}", text(&params["type"]), text(&params["message"])),
        "$/progress" => {
            let value = &params["value"];
            let parts: Vec<String> = [text(&value["title"]), text(&value["message"])]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect();
            let percentage = value["percentage"]
                .as_u64()
                .map(|percentage| format!(" ({percentage}%)"))
                .unwrap_or_default();
            match value["kind"].as_str() {
                Some("end") if parts.is_empty() => "done".to_owned(),
                _ if parts.is_empty() => String::new(),
                _ => format!("{}{percentage}", parts.join(" — ")),
            }
        }
        _ => String::new(),
    }
}

/// `directory` as a `file://` URL ending in `/`, so a fixture path joins
/// onto it.
fn directory_url(directory: &Path) -> Url {
    Url::from_directory_path(directory).expect("an absolute directory")
}

/// Writes a fixture file below `directory`.
fn write_file(directory: &Path, file: &FixtureFile) {
    let path = directory.join(&file.path);
    let parent = path.parent().expect("a fixture path has a directory");
    std::fs::create_dir_all(parent).expect("the fixture directory is writable");
    std::fs::write(&path, &file.text).expect("the fixture file is writable");
}

/// This crate's server on the other end of an in-memory pipe, its
/// workspace pinned at `workspace`.
fn in_memory(workspace: PathBuf) -> (Reader, Writer, Url, Host) {
    let (client_side, server_side) = tokio::io::duplex(PIPE_CAPACITY);
    let (server_reader, server_writer) = tokio::io::split(server_side);
    let root = directory_url(&workspace);
    let (service, socket) = crate::backend::service::service(Arc::new(pinned_indexer(workspace)));
    let server = tokio::spawn(Server::new(server_reader, server_writer, socket).concurrency_level(64).serve(service));
    let (reader, writer) = tokio::io::split(client_side);
    (Box::new(reader), Box::new(writer), root, Host(server))
}

/// An indexer whose workspace is pinned at the fixture's: it reads what is
/// below it (nothing, at [`MEMORY_ROOT`]) and nothing else — no Gradle
/// cache, not the machine's JDK, only the harness's own ([`stub_jdk`]). What the server derives from the root (a file's
/// package from its directory) and what it reads below it are the
/// fixture's alone, never the directory the tests run in.
fn pinned_indexer(workspace: PathBuf) -> Indexer {
    let mut indexer = Indexer::new();
    indexer.libraries = Arc::clone(stub_jdk());
    indexer.workspace_pinned.store(true, Ordering::Relaxed);
    let _ = indexer.workspace_root.set(workspace);
    indexer
}

/// Where the stub JDK's sources are: a few `java.lang` and `java.util`
/// types, laid out as the JDK's `src.zip` lays them out.
const STUB_JDK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/integrationtest/jdk");

/// The JDK every in-memory server reads: the sources below [`STUB_JDK`],
/// zipped once per test run. A case may name `String`, `List` or
/// `Runnable` as jdtls's tests do, and its answer is the same on every
/// machine, whichever JDK (if any) is installed. Only what those types
/// declare is there — a member the stub leaves out is one no case asks for.
fn stub_jdk() -> &'static Arc<Libraries> {
    static JDK: OnceLock<(tempfile::TempDir, Arc<Libraries>)> = OnceLock::new();
    let (_, libraries) = JDK.get_or_init(|| {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let zip = directory.path().join("src.zip");
        write_zip(Path::new(STUB_JDK), &zip);
        let libraries = Libraries::extracting_into(directory.path().join("sources"));
        libraries.replace(&[zip]);
        (directory, Arc::new(libraries))
    });
    libraries
}

/// `root`'s files zipped into `zip`, each under its path below `root`.
fn write_zip(root: &Path, zip: &Path) {
    use std::io::Write;
    let mut files = Vec::new();
    crate::indexer::walk_sources(root, |path| {
        files.push(path.to_owned());
        std::ops::ControlFlow::Continue(())
    });
    files.sort();
    let mut writer = zip::ZipWriter::new(std::fs::File::create(zip).expect("the zip is writable"));
    for file in files {
        let entry = file.strip_prefix(root).expect("below the root");
        writer
            .start_file(entry.to_string_lossy(), zip::write::SimpleFileOptions::default())
            .expect("an entry starts");
        writer
            .write_all(&std::fs::read(&file).expect("a stub source is readable"))
            .expect("an entry is written");
    }
    writer.finish().expect("the zip is finished");
}

/// The `Content-Length` framed messages `reader` carries, one per item.
fn frames(reader: Reader) -> mpsc::UnboundedReceiver<Value> {
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        while let Some(message) = read_frame(&mut reader).await {
            if sender.send(message).is_err() {
                return;
            }
        }
    });
    receiver
}

async fn read_frame(reader: &mut BufReader<Reader>) -> Option<Value> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.ok()? == 0 {
            return None;
        }
        let header = header.trim();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).await.ok()?;
    serde_json::from_slice(&body).ok()
}

/// What a place in the JDK or a library is shown as: `<library>/`, then the
/// module or jar and the file's path inside it
/// (`<library>/java.base/java/lang/reflect/Array.java`). Each server has its
/// own copy of those files, and jdtls points at a class file
/// (`jdt://contents/java.base/java.lang.reflect/Array.class?…`) where
/// javak-lsp points at the source it extracted
/// (`file:///…/sources/src-…/java.base/java/lang/reflect/Array.java`), so
/// the file is what the two can agree on, not the line.
pub(super) fn library_place(uri: &str) -> Option<String> {
    if let Some(contents) = uri.strip_prefix("jdt://contents/") {
        let class = contents.split('?').next()?;
        let (container, rest) = class.split_once('/')?;
        let (package, file) = rest.rsplit_once('/')?;
        // jdtls names the class file (`Array.class`) or, since 1.5x, the
        // source file it would show (`Array.java`, the top-level class's
        // file for a nested one).
        let name = file
            .strip_suffix(".class")
            .or_else(|| file.strip_suffix(".java"))?;
        // A nested class's own class file (`Flags$StringJoiner.class`),
        // named where its class file does not say which source file it
        // comes from (compiled without `SourceFile`, as bcprov and xerces
        // are): it is in its top-level class's file.
        let name = name.split('$').next().unwrap_or(name);
        return Some(format!(
            "{LIBRARY}/{container}/{}/{name}.java",
            package.replace('.', "/")
        ));
    }
    let path = uri.strip_prefix("file://")?;
    let (_, extracted) = path.split_once("/sources/")?;
    let (_, inside) = extracted.split_once('/')?;
    Some(format!("{LIBRARY}/{inside}"))
}

/// jdtls's JDKs: only `JAVA_HOME`'s, the one javak-lsp reads too, rather
/// than every JDK it can find. jdtls looks for the installed JDKs at start
/// and gives a Java 8 project the oldest that runs it (a JDK 21 beside
/// the JDK 25 both are started with): the two would answer from different
/// JDKs. Without `JAVA_HOME`, jdtls chooses as it does.
fn java_runtimes() -> Value {
    let Some(home) = std::env::var_os("JAVA_HOME").map(std::path::PathBuf::from) else {
        return json!({});
    };
    let release = std::fs::read_to_string(home.join("release")).unwrap_or_default();
    let version = release
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))
        .map(|version| version.trim_matches('"').to_owned())
        .unwrap_or_default();
    // `1.8.0_412` is JavaSE-1.8, `25.0.1` JavaSE-25.
    let major = if let Some(rest) = version.strip_prefix("1.") {
        format!("1.{}", rest.split(['.', '_']).next().unwrap_or_default())
    } else {
        version.split('.').next().unwrap_or_default().to_owned()
    };
    if major.is_empty() {
        return json!({});
    }
    json!({
        "detectJdksAtStart": false,
        "runtimes": [{"name": format!("JavaSE-{major}"), "path": home, "default": true}],
    })
}

/// How [`library_place`] starts, so a place in a library is told apart.
pub(super) const LIBRARY: &str = "<library>";
