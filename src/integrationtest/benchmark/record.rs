//! One server asked every question about one project, and its answers
//! written down.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::prepare::ServerCommand;
use super::probe::{Method, Probe};
use super::render::View;
use crate::integrationtest::server::{failure, OnError, Session};

/// How long one request may take on a real project, unless
/// `JDTLS_COMPAT_PATIENCE` says otherwise (seconds).
fn patience() -> Duration {
    let seconds = std::env::var("JDTLS_COMPAT_PATIENCE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    Duration::from_secs(seconds)
}
/// How often a slow question says it is still waiting.
const STILL_WAITING: Duration = Duration::from_secs(5);
/// How long a server may take to import a project before it is asked
/// anyway.
const IMPORT_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// What one server answered about one project.
#[derive(Serialize, Deserialize)]
pub(super) struct Recording {
    pub(super) server: String,
    pub(super) repo: String,
    pub(super) commit: String,
    /// Whether the server said it had imported the project before the
    /// deadline.
    pub(super) ready: bool,
    pub(super) import_seconds: f64,
    pub(super) answer_seconds: f64,
    pub(super) answers: Vec<Answer>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Answer {
    pub(super) probe: Probe,
    pub(super) lines: Vec<String>,
    pub(super) millis: u64,
    /// What the server said where it failed with an internal error
    /// instead of answering: such a question of jdtls's is not compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) failed: Option<String>,
}

impl Recording {
    pub(super) fn load(path: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    pub(super) fn save(&self, path: &Path) {
        let json = serde_json::to_string_pretty(self).expect("a recording serializes");
        std::fs::write(path, json).expect("the recording is writable");
    }
}

/// Which server is asked, and how it says it has read the project.
pub(super) struct Server {
    pub(super) name: &'static str,
    pub(super) command: ServerCommand,
    /// Arguments added for this project (jdtls's `-data`).
    pub(super) extra_arguments: Vec<String>,
    /// The texts of the messages that mean "the project is read", and how
    /// long the server must stay quiet after one.
    pub(super) ready_markers: &'static [(&'static str, Duration)],
}

/// jdtls says `ServiceReady` once the import and the first build are done.
pub(super) const JDTLS_READY: &[(&str, Duration)] = &[("ServiceReady", Duration::from_secs(5))];

/// javak-lsp logs the end of its dependency sync. A project whose sync is
/// up to date says only that its libraries are indexed; a longer quiet
/// after that leaves room for a sync that is still coming.
///
/// Libraries without (all their) sources are decompiled after the sync:
/// the first time, that takes minutes and says nothing until it is done.
pub(super) const JAVAK_READY: &[(&str, Duration)] = &[
    ("dependencies synced", Duration::from_secs(3)),
    ("dependency sync failed", Duration::from_secs(3)),
    ("library indexing done", Duration::from_secs(30)),
    ("decompiling the libraries", Duration::from_secs(15 * 60)),
    ("libraries decompiled", Duration::from_secs(3)),
];

pub(super) async fn record(
    server: &Server,
    repo: &str,
    commit: &str,
    project: &Path,
    probes: &[Probe],
    log: &Path,
) -> Recording {
    let started = Instant::now();
    eprintln!(
        "  {}: starting, its own log goes to {}",
        server.name,
        log.display()
    );
    let stderr = std::fs::File::create(log).expect("the log is writable");
    let mut arguments = server.command.arguments.clone();
    arguments.extend(server.extra_arguments.iter().cloned());
    let mut session = Session::spawn_in_project(
        &server.command.program,
        &arguments,
        project,
        stderr.into(),
        patience(),
    )
    .await;
    let ready = session
        .wait_until_ready(
            server.name,
            server.ready_markers,
            tokio::time::Instant::now() + IMPORT_DEADLINE,
        )
        .await;
    let import_seconds = started.elapsed().as_secs_f64();
    eprintln!(
        "  {}: {} in {import_seconds:.1}s; diagnostics are {}",
        server.name,
        if ready { "ready" } else { "not ready, asking anyway" },
        if session.pulls_diagnostics() { "pulled" } else { "pushed" }
    );

    let answered = Instant::now();
    let mut by_file: BTreeMap<&str, Vec<&Probe>> = BTreeMap::new();
    for probe in probes {
        by_file.entry(&probe.file).or_default().push(probe);
    }
    let mut answers = Vec::new();
    let files = by_file.len();
    for (index, (file, probes)) in by_file.into_iter().enumerate() {
        eprintln!(
            "  {}: [{}/{files}] {} questions about {file}",
            server.name,
            index + 1,
            probes.len()
        );
        let started = Instant::now();
        let text = std::fs::read_to_string(project.join(file)).unwrap_or_default();
        session.open_document(file, &text).await;
        let mut version = 1;
        let count = probes.len();
        for (number, probe) in probes.into_iter().enumerate() {
            let asked = Instant::now();
            let edited = probe.edit.as_ref().map(|edit| edit.apply(&text));
            if let Some(edited) = &edited {
                version += 1;
                session.change_document(file, version, edited).await;
            }
            let current = edited.as_deref().unwrap_or(&text);
            let answer = {
                let asking = ask(&mut session, probe, current);
                tokio::pin!(asking);
                let mut ticker = tokio::time::interval_at(
                    tokio::time::Instant::now() + STILL_WAITING,
                    STILL_WAITING,
                );
                loop {
                    tokio::select! {
                        answer = &mut asking => break answer,
                        _ = ticker.tick() => eprintln!(
                            "      [{}] still waiting on {} ({}s)",
                            server.name,
                            probe.id,
                            asked.elapsed().as_secs()
                        ),
                    }
                }
            };
            let failed = failure(&answer).map(str::to_owned);
            let lines = match &failed {
                Some(said) => vec![format!("(failed: {said})")],
                None => View {
                    session: &session,
                    text: current,
                    word: &probe.word,
                }
                .render(probe.method, &answer),
            };
            if edited.is_some() {
                version += 1;
                session.change_document(file, version, &text).await;
            }
            let took = asked.elapsed();
            let timed_out = answer.is_null() && took >= patience();
            eprintln!(
                "      [{}] {}/{count} {:.1}s {} → {}",
                server.name,
                number + 1,
                took.as_secs_f64(),
                probe.id,
                if timed_out {
                    format!("no answer within {}s", patience().as_secs())
                } else if failed.is_some() {
                    lines.join(" ")
                } else {
                    preview(&lines)
                }
            );
            answers.push(Answer {
                probe: probe.clone(),
                lines,
                millis: took.as_millis() as u64,
                failed,
            });
        }
        session.close_document(file).await;
        eprintln!(
            "  {}: [{}/{files}] answered in {:.1}s",
            server.name,
            index + 1,
            started.elapsed().as_secs_f64()
        );
    }
    session.shutdown().await;
    Recording {
        server: server.name.to_owned(),
        repo: repo.to_owned(),
        commit: commit.to_owned(),
        ready,
        import_seconds,
        answer_seconds: answered.elapsed().as_secs_f64(),
        answers,
    }
}

/// An answer in a few words: its first line, and how many more there are.
fn preview(lines: &[String]) -> String {
    let first = lines.first().map(String::as_str).unwrap_or_default();
    let first: String = first.chars().take(80).collect();
    match lines.len() {
        0 | 1 => first,
        count => format!("{first} (+{} lines)", count - 1),
    }
}

/// The server's answer to `probe`, as JSON; the two hierarchies already as
/// the lines they are rendered as, since each takes three requests.
async fn ask(session: &mut Session, probe: &Probe, text: &str) -> Value {
    if probe.method == Method::Diagnostics {
        // Asked the way an editor asks each server: pulled from one that
        // answers `textDocument/diagnostic` (javak-lsp), else the last
        // push once the file has settled (jdtls). Both read as the same
        // list of diagnostics.
        if session.pulls_diagnostics() {
            let document = json!({"textDocument": {"uri": session.uri(&probe.file)}});
            let report = request(session, "textDocument/diagnostic", document).await;
            return report.get("items").cloned().unwrap_or(Value::Null);
        }
        return session
            .pushed_diagnostics(&probe.file, |published| !published.is_null())
            .await;
    }
    let params = params(session, probe, text);
    let answer = request(session, probe.method.lsp(), params).await;
    match probe.method {
        Method::CallHierarchy => {
            hierarchy(session, &answer, "callHierarchy", "incomingCalls", "outgoingCalls").await
        }
        Method::TypeHierarchy => {
            hierarchy(session, &answer, "typeHierarchy", "supertypes", "subtypes").await
        }
        _ => answer,
    }
}

async fn request(session: &mut Session, method: &str, params: Value) -> Value {
    session
        .request_until(method, params, OnError::AnswerNothingUnlessItFailed, |_| true)
        .await
}

fn params(session: &Session, probe: &Probe, text: &str) -> Value {
    let document = json!({"uri": session.uri(&probe.file)});
    let position = json!({"line": probe.line, "character": probe.character});
    match probe.method {
        Method::DocumentSymbol
        | Method::FoldingRange
        | Method::SemanticTokens
        | Method::CodeLens => json!({"textDocument": document}),
        Method::Formatting => json!({
            "textDocument": document,
            "options": {"tabSize": 4, "insertSpaces": true},
        }),
        Method::InlayHint => json!({
            "textDocument": document,
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": text.lines().count(), "character": 0},
            },
        }),
        Method::CodeAction => json!({
            "textDocument": document,
            "range": {"start": position, "end": position},
            "context": {"diagnostics": []},
        }),
        Method::SelectionRange => json!({"textDocument": document, "positions": [position]}),
        Method::WorkspaceSymbol => json!({"query": probe.word}),
        Method::References => json!({
            "textDocument": document,
            "position": position,
            "context": {"includeDeclaration": true},
        }),
        Method::Rename => json!({
            "textDocument": document,
            "position": position,
            "newName": format!("{}Renamed", probe.word),
        }),
        Method::Completion => json!({
            "textDocument": document,
            "position": position,
            "context": {"triggerKind": if probe.word.is_empty() { 2 } else { 1 },
                        "triggerCharacter": "."},
        }),
        _ => json!({"textDocument": document, "position": position}),
    }
}

/// The first item a hierarchy is prepared at, and what is above and below
/// it, as `item`, `<up>` and `<down>` lines.
async fn hierarchy(
    session: &mut Session,
    prepared: &Value,
    prefix: &str,
    up: &str,
    down: &str,
) -> Value {
    if failure(prepared).is_some() {
        return prepared.clone();
    }
    let Some(item) = prepared.as_array().and_then(|items| items.first()).cloned() else {
        return Value::Null;
    };
    let mut lines = vec![format!("item {}", describe(session, &item))];
    for (direction, end) in [(up, "from"), (down, "to")] {
        let answer = request(session, &format!("{prefix}/{direction}"), json!({"item": item})).await;
        if failure(&answer).is_some() {
            return answer;
        }
        let mut found: Vec<String> = answer
            .as_array()
            .into_iter()
            .flatten()
            // Calls name the caller or callee under `from`/`to`; a type
            // hierarchy answers the items themselves.
            .map(|entry| format!("{direction}: {}", describe(session, entry.get(end).unwrap_or(entry))))
            .collect();
        found.sort();
        found.dedup();
        lines.extend(found);
    }
    json!(lines)
}

fn describe(session: &Session, item: &Value) -> String {
    let view = View {
        session,
        text: "",
        word: "",
    };
    format!(
        "{} {} at {}",
        super::render::symbol_kind(&item["kind"]),
        item["name"].as_str().unwrap_or_default(),
        view.location(&json!({"uri": item["uri"], "range": item["selectionRange"]}))
    )
}
