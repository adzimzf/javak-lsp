//! The `javak-debug-server.jar` process: java-debug core serving the Debug
//! Adapter Protocol on a TCP port, with javak-lsp standing in for the JDT
//! Java model behind it.
//!
//! The process talks to javak-lsp over its stdio as newline-delimited JSON:
//!
//! ```text
//! ← {"event":"started","port":41233}
//! ← {"id":1,"method":"breakpointClasses","params":{"uri":"file:///…/Main.kt","lines":[12,40]}}
//! → {"id":1,"result":{"classNames":["com.example.MainKt","com.example.Greeter"]}}
//! ← {"id":2,"method":"sourceUri","params":{"className":"com.example.Greeter$Companion","sourcePath":"com/example/Main.kt"}}
//! → {"id":2,"result":{"uri":"file:///…/Main.kt"}}
//! → {"method":"updateDebugSettings","params":{"settings":"{\"showHex\":true}"}}
//! ```
//!
//! `←` lines come from the debug server, `→` lines from javak-lsp. One debug
//! server serves every debug session (like jdtls' `JavaDebugServer`); it is
//! started on the first `vscode.java.startDebugSession` and restarted if it
//! has exited.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::Url;

use super::jvm_names::{breakpoint_class, SourcePath};
use crate::feature::semantic::FileView;
use crate::indexer::Indexer;
use crate::language::Language;
use crate::parser::syntax;

/// The debug server jar's file name, installed next to `javak-lsp`.
pub(crate) const DEBUG_SERVER_JAR: &str = "javak-debug-server.jar";

/// Overrides where the debug server jar is looked up.
pub(crate) const DEBUG_SERVER_JAR_VARIABLE: &str = "JAVAK_LSP_DEBUG_SERVER_JAR";

/// How long the JVM may take to report its port.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// A running debug server.
pub(crate) struct DebugServer {
    port: u16,
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
}

/// Why no debug server could be started.
#[derive(Debug)]
pub(crate) enum LaunchError {
    JarNotFound,
    JavaNotFound,
    Spawn(std::io::Error),
    NoPort(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JarNotFound => write!(
                formatter,
                "{DEBUG_SERVER_JAR} not found next to javak-lsp, in ../share/javak-lsp, \
                 in ~/.cargo/bin, or at ${DEBUG_SERVER_JAR_VARIABLE} \
                 (build it with `mvn -q package` in java-sidecar/debug-server)"
            ),
            Self::JavaNotFound => write!(
                formatter,
                "no `java` found (set javaHome, JAVA_HOME or put java on the PATH)"
            ),
            Self::Spawn(error) => write!(formatter, "failed to start the debug server: {error}"),
            Self::NoPort(reason) => {
                write!(
                    formatter,
                    "the debug server did not report a port: {reason}"
                )
            }
        }
    }
}

impl DebugServer {
    /// Start the debug server with `java`, answering its questions from
    /// `indexer` on a background thread.
    pub(crate) fn launch(
        indexer: Arc<Indexer>,
        java: &Path,
        settings: Option<&str>,
    ) -> Result<Self, LaunchError> {
        let jar = find_debug_server_jar().ok_or(LaunchError::JarNotFound)?;
        let mut child = Command::new(java)
            .arg("-jar")
            .arg(&jar)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(LaunchError::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LaunchError::NoPort("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LaunchError::NoPort("no stdout".into()))?;

        let (port, reader) = match read_port(BufReader::new(stdout)) {
            Ok(started) => started,
            Err(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LaunchError::NoPort(reason));
            }
        };
        log::info!("debug: javak-debug-server listening on port {port}");

        let stdin = Arc::new(Mutex::new(stdin));
        spawn_control_loop(reader, Arc::clone(&stdin), indexer);
        let server = Self { port, child, stdin };
        if let Some(settings) = settings {
            server.update_settings(settings);
        }
        Ok(server)
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Whether the process is still running.
    pub(crate) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Forward `vscode.java.updateDebugSettings`' JSON to the debug server.
    pub(crate) fn update_settings(&self, settings: &str) {
        let notification = Notification {
            method: "updateDebugSettings",
            params: UpdateSettingsParams { settings },
        };
        write_message(&self.stdin, &notification);
    }
}

impl Drop for DebugServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The jar: `$JAVAK_LSP_DEBUG_SERVER_JAR`, next to the running `javak-lsp`,
/// in `../share/javak-lsp` from it (a Homebrew or `/usr/local` install), in
/// `~/.cargo/bin`, or where this checkout builds it
/// (`java-sidecar/debug-server/target`), so a development build finds it.
pub(crate) fn find_debug_server_jar() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(DEBUG_SERVER_JAR_VARIABLE).map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf));
    let beside_executable = executable_dir.as_ref().map(|dir| dir.join(DEBUG_SERVER_JAR));
    let in_share = executable_dir
        .as_ref()
        .and_then(|dir| dir.parent())
        .map(|prefix| prefix.join("share").join("javak-lsp").join(DEBUG_SERVER_JAR));
    let in_cargo_bin =
        crate::util::home_dir().map(|home| home.join(".cargo").join("bin").join(DEBUG_SERVER_JAR));
    let in_checkout = Some(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("java-sidecar")
            .join("debug-server")
            .join("target")
            .join(DEBUG_SERVER_JAR),
    );
    [beside_executable, in_share, in_cargo_bin, in_checkout]
        .into_iter()
        .flatten()
        .find(|path| path.is_file())
}

#[derive(Deserialize)]
struct StartedEvent {
    event: String,
    port: u16,
}

type ServerOutput = BufReader<std::process::ChildStdout>;

/// Wait for the `started` event, within [`STARTUP_TIMEOUT`], and hand the
/// reader back for the control loop.
///
/// `read_line` can't time out, so it runs on its own thread; on a timeout
/// the caller kills the child, which closes stdout and ends that thread.
fn read_port(reader: ServerOutput) -> Result<(u16, ServerOutput), String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let result = read_started_event(&mut reader).map(|port| (port, reader));
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(STARTUP_TIMEOUT)
        .unwrap_or_else(|_| Err(format!("no answer within {}s", STARTUP_TIMEOUT.as_secs())))
}

fn read_started_event(reader: &mut ServerOutput) -> Result<u16, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("the process exited".to_owned());
        }
        if let Ok(started) = serde_json::from_str::<StartedEvent>(&line)
            && started.event == "started"
        {
            return Ok(started.port);
        }
    }
}

#[derive(Deserialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    result: serde_json::Value,
}

#[derive(Serialize)]
struct Notification<'a, T: Serialize> {
    method: &'a str,
    params: T,
}

#[derive(Serialize)]
struct UpdateSettingsParams<'a> {
    settings: &'a str,
}

fn write_message(stdin: &Mutex<ChildStdin>, message: &impl Serialize) {
    let Ok(mut line) = serde_json::to_string(message) else {
        return;
    };
    line.push('\n');
    let mut stdin = stdin
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(error) = stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.flush())
    {
        log::warn!("debug: failed to write to javak-debug-server: {error}");
    }
}

fn spawn_control_loop(reader: ServerOutput, stdin: Arc<Mutex<ChildStdin>>, indexer: Arc<Indexer>) {
    let spawned = std::thread::Builder::new()
        .name("javak-debug-server-control".into())
        .spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(request) = serde_json::from_str::<Request>(&line) else {
                    log::debug!("debug: ignoring debug-server line: {line}");
                    continue;
                };
                let result = answer(&indexer, &request.method, &request.params);
                write_message(
                    &stdin,
                    &Response {
                        id: request.id,
                        result,
                    },
                );
            }
            log::info!("debug: javak-debug-server control channel closed");
        });
    if let Err(error) = spawned {
        log::warn!("debug: failed to start the control thread: {error}");
    }
}

/// Answer one debug-server question. Unknown methods answer `null`, which
/// the debug server treats as "no information".
pub(crate) fn answer(
    indexer: &Indexer,
    method: &str,
    params: &serde_json::Value,
) -> serde_json::Value {
    match method {
        "breakpointClasses" => {
            let Ok(params) = serde_json::from_value::<BreakpointClassesParams>(params.clone())
            else {
                return serde_json::Value::Null;
            };
            let class_names = breakpoint_classes(indexer, &params.uri, &params.lines);
            serde_json::json!({ "classNames": class_names })
        }
        "sourceUri" => {
            let Ok(params) = serde_json::from_value::<SourceUriParams>(params.clone()) else {
                return serde_json::Value::Null;
            };
            let uri = source_uri(indexer, &params.class_name, params.source_path.as_deref());
            serde_json::json!({ "uri": uri })
        }
        _ => serde_json::Value::Null,
    }
}

#[derive(Deserialize)]
struct BreakpointClassesParams {
    uri: String,
    /// 1-based, as java-debug passes them.
    lines: Vec<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceUriParams {
    class_name: String,
    source_path: Option<String>,
}

/// The JVM class to install each breakpoint line in; `None` where no class
/// holds code on that line, or the file can't be read.
pub(crate) fn breakpoint_classes(indexer: &Indexer, uri_or_path: &str, lines: &[u32]) -> Vec<Option<String>> {
    let Some(file) = source_file(indexer, uri_or_path) else {
        return vec![None; lines.len()];
    };
    let file_name = file.uri.path().to_owned();
    lines
        .iter()
        .map(|line| breakpoint_class(file.language, &file_name, &file.text, &file.tree, line.saturating_sub(1)))
        .collect()
}

/// The workspace's parse of a file, or a fresh one of it from disk.
fn source_file(indexer: &Indexer, uri_or_path: &str) -> Option<FileView> {
    let url = Url::parse(uri_or_path)
        .ok()
        .filter(|url| url.scheme() == "file")
        .or_else(|| Url::from_file_path(uri_or_path).ok())?;
    if let Some(file) = indexer.workspace_file(&url) {
        return Some(file);
    }
    let path = url.to_file_path().ok()?;
    let language = Language::of_path(&path.to_string_lossy())?;
    let text = std::fs::read_to_string(&path).ok()?;
    let tree = syntax(language).tree(&text);
    Some(FileView {
        uri: url,
        language,
        text: Arc::new(text),
        tree: Arc::new(tree),
        library: false,
    })
}

/// The source file a class was compiled from, as a `file://` URI: a
/// workspace file, else a library's sources (the JDK's `src.zip` among
/// them), extracted where go to definition extracts them.
///
/// `source_path` is what JDI reports (`com/example/Main.kt`); without one
/// the class name implies `com/example/Outer.{java,kt}`. Files are matched
/// by file name *and* declared package, so a Kotlin file whose directory
/// doesn't follow its package is still found.
pub(crate) fn source_uri(indexer: &Indexer, class_name: &str, source_path: Option<&str>) -> Option<String> {
    let candidates: Vec<SourcePath> = match source_path.and_then(SourcePath::parse) {
        Some(source_path) => vec![source_path],
        None => vec![
            SourcePath::of_class(class_name, "java"),
            SourcePath::of_class(class_name, "kt"),
        ],
    };
    let workspace = indexer.workspace_files();
    for candidate in &candidates {
        let found = workspace.iter().find(|file| {
            let package = syntax(file.language).package(&file.text);
            candidate.matches(file.uri.path(), package.as_deref())
        });
        if let Some(file) = found {
            return Some(file.uri.to_string());
        }
    }
    library_source_uri(indexer, class_name, &candidates)
}

fn library_source_uri(indexer: &Indexer, class_name: &str, candidates: &[SourcePath]) -> Option<String> {
    let top_level = class_name.split('$').next().unwrap_or(class_name);
    let simple = top_level.rsplit('.').next().unwrap_or(top_level);
    // A Kotlin facade (`CollectionsKt`) declares no type of its name: look
    // for the file instead, under the names a type of it would have.
    let names = [simple.to_owned(), simple.trim_end_matches("Kt").to_owned()];
    for name in names.iter().filter(|name| !name.is_empty()) {
        for file in indexer.libraries.declaring(name) {
            let matches = candidates.iter().any(|candidate| {
                candidate.matches(&file.entry, Some(file.package.as_str()))
            });
            if matches {
                return indexer.libraries.load(&file).map(|loaded| loaded.uri.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
