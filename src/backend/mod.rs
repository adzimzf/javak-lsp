//! The LSP server: every request is answered out of the parser's view of
//! the open files and the libraries they name. How the protocol names a
//! language is behind [`LanguageProtocol`], implemented in `java` and
//! `kotlin`; dependency sync runs from [`dependencies`].

mod dependencies;
pub mod java;
pub mod kotlin;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use serde_json::Value;
use tower_lsp::{Client, LanguageServer};

use crate::debugger::commands::{self as debug_commands, CommandOutcome, DebugCommands};
use crate::feature::features;
use crate::indexer::Indexer;
use crate::language::Language;
use crate::parser::{Fold, Kind, Outline, Position};
use dependencies::DependencySync;

/// How the protocol names a language.
pub trait LanguageProtocol: Send + Sync {
    /// The `languageId` a client opens its documents with, and the
    /// language of the code blocks the server answers with.
    fn language_id(&self) -> &'static str;
}

/// The protocol names of a language.
pub fn protocol(language: Language) -> &'static dyn LanguageProtocol {
    match language {
        Language::Java => &java::Java,
        Language::Kotlin => &kotlin::Kotlin,
    }
}

pub fn backend_over(indexer: Arc<Indexer>, client: Client) -> Backend {
    let indexer_for_debug = Arc::clone(&indexer);
    Backend {
        indexer,
        client,
        dependencies: Arc::new(DependencySync::default()),
        debug: Arc::new(DebugCommands::new(indexer_for_debug)),
    }
}

pub struct Backend {
    indexer: Arc<Indexer>,
    client: Client,
    dependencies: Arc<DependencySync>,
    /// The `vscode.java.*` commands of jdtls' debug plugin.
    debug: Arc<DebugCommands>,
}

impl Backend {
    /// Everything the server has to say about one file: its syntax errors;
    /// where its path names a package it does not declare, that; what it
    /// writes that the project's Java level does not have; and the calls
    /// no method of their name can take.
    fn diagnostics(&self, uri: &Url) -> Vec<Diagnostic> {
        let Some(text) = self.indexer.text(uri) else {
            return Vec::new();
        };
        let language = self.indexer.language(uri).unwrap_or(Language::Java);
        let syntax = self.indexer.syntax(language);
        let mut diagnostics = Vec::new();
        if let Some(expected) = self.indexer.package_of_path(uri) {
            diagnostics.extend(package_mismatch(
                language,
                &text,
                syntax.package(&text),
                &expected,
                syntax.outline(&text).is_empty(),
            ));
        }
        let mut findings = syntax.syntax_errors(&text);
        let snapshot = self.indexer.snapshot();
        if let Some(file) = snapshot.file(uri) {
            if let Some(level) = self.indexer.java_level() {
                findings.extend(syntax.above_java_level(&file.tree, level));
            }
            findings.extend(snapshot.inapplicable_calls(file));
        }
        findings.sort_by_key(|finding| finding.at);
        diagnostics.extend(
            findings
                .into_iter()
                .map(|finding| reported(finding.at, finding.message)),
        );
        diagnostics
    }

    /// A build file changed: its source level may have, and the
    /// dependencies it names.
    async fn build_file_changed(&self) {
        self.indexer.read_java_level();
        self.dependencies
            .start(Arc::clone(&self.indexer), self.client.clone());
        for uri in self.indexer.open_uris() {
            self.publish(uri).await;
        }
    }

    async fn publish(&self, uri: Url) {
        let diagnostics = self.diagnostics(&uri);
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }
}

/// References as jdtls lists them: a type named in an import is found at
/// the start of the import's qualified name (`org.jsoup.parser.Parser`),
/// and the variable of an enhanced `for` is not listed at its own
/// declaration.
fn as_jdtls_reports<'a>(
    snapshot: &'a crate::feature::semantic::Snapshot,
    found: &crate::feature::semantic::Found<'_>,
    uses: Vec<(&'a Url, Position)>,
) -> Vec<(&'a Url, Position)> {
    let declared_by_for = found.node.kind == "enhanced_for_statement";
    uses.into_iter()
        .filter(|(uri, at)| {
            !(declared_by_for && **uri == found.file.uri && *at == found.selection())
        })
        .map(|(uri, at)| {
            let Some(file) = snapshot.file(uri) else {
                return (uri, at);
            };
            let path = crate::feature::semantic::path_to(&file.tree, at);
            let import = path.iter().find(|node| node.kind == "import_declaration");
            let name = import.and_then(|import| {
                import.children.iter().find(|child| {
                    matches!(child.kind.as_str(), "scoped_identifier" | "identifier")
                })
            });
            (uri, name.map_or(at, |name| name.start))
        })
        .collect()
}

/// What is said of a file whose package is not the one its folder names.
///
/// Java says it as the Eclipse compiler (jdtls) does: a file without a
/// package declares `""`, reported at the start of the file, and a
/// different package at its name. A file that declares nothing is not
/// compiled, so nothing is said of it. Kotlin keeps its own wording, and
/// only a missing declaration is reported there.
fn package_mismatch(
    language: Language,
    text: &str,
    declared: Option<String>,
    expected: &str,
    declares_nothing: bool,
) -> Option<Diagnostic> {
    match language {
        Language::Kotlin => declared.is_none().then(|| {
            reported(
                Position { line: 0, column: 0 },
                format!("Missing package declaration (`{expected}`)"),
            )
        }),
        Language::Java => {
            let declared_name = declared.unwrap_or_default();
            if declared_name == expected || (declared_name.is_empty() && declares_nothing) {
                return None;
            }
            let at = package_name_offset(text, &declared_name)
                .map_or(Position { line: 0, column: 0 }, |offset| {
                    crate::parser::position_of(text, offset)
                });
            Some(reported(
                at,
                format!(
                    "The declared package \"{declared_name}\" does not match the expected package \"{expected}\""
                ),
            ))
        }
    }
}

/// Where the name of the package declaration starts.
fn package_name_offset(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let mut from = 0;
    while let Some(found) = text[from..].find("package") {
        let keyword = from + found;
        let after = keyword + "package".len();
        let rest = &text[after..];
        let gap = rest.len() - rest.trim_start().len();
        if gap > 0 && rest.trim_start().starts_with(name) {
            return Some(after + gap);
        }
        from = after;
    }
    None
}

fn reported(at: Position, message: String) -> Diagnostic {
    let start = lsp_position(at);
    Diagnostic {
        range: Range { start, end: start },
        severity: Some(DiagnosticSeverity::ERROR),
        message,
        ..Diagnostic::default()
    }
}

fn lsp_position(at: Position) -> tower_lsp::lsp_types::Position {
    tower_lsp::lsp_types::Position {
        line: at.line,
        character: at.column,
    }
}

fn cursor(at: tower_lsp::lsp_types::Position) -> Position {
    Position {
        line: at.line,
        column: at.character,
    }
}

fn highlighted(at: Position) -> DocumentHighlight {
    DocumentHighlight {
        range: at_name(at),
        kind: Some(DocumentHighlightKind::TEXT),
    }
}

/// An empty range at a declaration's name: what a client is sent to.
fn at_name(at: Position) -> Range {
    let start = lsp_position(at);
    Range { start, end: start }
}

/// The language of an opened document: the client's language id, else
/// the file's extension, else Java.
fn language_of(id: &str, uri: &Url) -> Language {
    Language::ALL
        .into_iter()
        .find(|language| protocol(*language).language_id() == id)
        .or_else(|| Language::of_path(uri.path()))
        .unwrap_or(Language::Java)
}

/// The files the server is told about when they change: the build files,
/// whose change can change the dependencies, and the source files, which
/// the server reads from disk while no editor has them open.
const WATCHED_GLOBS: &[&str] = &[
    "**/*.java",
    "**/*.kt",
    "**/build.gradle",
    "**/build.gradle.kts",
    "**/settings.gradle",
    "**/settings.gradle.kts",
    "**/gradle.properties",
    "**/gradle-wrapper.properties",
    "**/*.versions.toml",
    "**/pom.xml",
];

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.dependencies
            .configure(params.initialization_options.as_ref());
        self.debug.configure(params.initialization_options.as_ref());
        if !self.indexer.workspace_pinned.load(Ordering::Relaxed) {
            if let Some(root) = params
                .root_uri
                .as_ref()
                .and_then(|uri| uri.to_file_path().ok())
            {
                let _ = self.indexer.workspace_root.set(root);
            }
        }
        // jdtls compiles the whole project, not only what is open: a name
        // resolves into a file no editor has opened yet.
        let indexer = Arc::clone(&self.indexer);
        let _ = tokio::task::spawn_blocking(move || {
            indexer.read_java_level();
            indexer.read_workspace()
        })
        .await;
        let indexer = Arc::clone(&self.indexer);
        tokio::task::spawn_blocking(move || indexer.list_declared_names());
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..TextDocumentSyncOptions::default()
                    },
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
                    ..SignatureHelpOptions::default()
                }),
                references_provider: Some(OneOf::Left(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_owned()]),
                    ..CompletionOptions::default()
                }),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: debug_commands::COMMANDS.iter().map(|command| (*command).to_owned()).collect(),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                diagnostic_provider: Some(DiagnosticServerCapabilities::Options(
                    DiagnosticOptions {
                        inter_file_dependencies: false,
                        workspace_diagnostics: false,
                        ..DiagnosticOptions::default()
                    },
                )),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let watchers = WATCHED_GLOBS
            .iter()
            .map(|glob| FileSystemWatcher {
                glob_pattern: GlobPattern::String((*glob).to_owned()),
                kind: None,
            })
            .collect();
        let registration = Registration {
            id: "javak-lsp-build-files".to_owned(),
            method: "workspace/didChangeWatchedFiles".to_owned(),
            register_options: serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                watchers,
            })
            .ok(),
        };
        let indexer = Arc::clone(&self.indexer);
        let client = self.client.clone();
        let dependencies = Arc::clone(&self.dependencies);
        // Registering waits on the client; the sync does not.
        tokio::spawn(async move {
            let _ = client.register_capability(vec![registration]).await;
        });
        dependencies.start(indexer, self.client.clone());
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let build_file_changed = params.changes.iter().any(|change| {
            change
                .uri
                .to_file_path()
                .is_ok_and(|path| DependencySync::is_build_file(&path))
        });
        for change in &params.changes {
            self.indexer.reread(&change.uri);
        }
        if build_file_changed {
            self.build_file_changed().await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        self.indexer.reread(&uri);
        if uri
            .to_file_path()
            .is_ok_and(|path| DependencySync::is_build_file(&path))
        {
            self.build_file_changed().await;
        }
    }

    /// jdtls' debug plugin's delegate commands, which nvim-jdtls, Zed and
    /// vscode-java-debug drive a debug session with.
    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        match self.debug.execute(&params.command, &params.arguments).await {
            CommandOutcome::Answered(value) => Ok(Some(value)),
            CommandOutcome::Failed(reason) => Err(tower_lsp::jsonrpc::Error {
                code: tower_lsp::jsonrpc::ErrorCode::InternalError,
                message: reason.into(),
                data: None,
            }),
            CommandOutcome::NotDebugCommand => Err(tower_lsp::jsonrpc::Error::method_not_found()),
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        let language = language_of(&document.language_id, &document.uri);
        self.indexer
            .open(document.uri.clone(), document.text, language);
        self.publish(document.uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().next_back() {
            self.indexer.change(&uri, change.text);
        }
        self.publish(uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.indexer.close(&params.text_document.uri);
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> Result<DocumentDiagnosticReportResult> {
        let items = self.diagnostics(&params.text_document.uri);
        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items,
                },
            }),
        ))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let Some(text) = self.indexer.text(&uri) else {
            return Ok(None);
        };
        let language = self.indexer.language(&uri).unwrap_or(Language::Java);
        let symbols: Vec<DocumentSymbol> = self
            .indexer
            .syntax(language)
            .outline(&text)
            .into_iter()
            .map(symbol_of)
            .collect();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: found.file.uri.clone(),
            range: at_name(found.selection()),
        })))
    }

    async fn goto_type_definition(
        &self,
        params: request::GotoTypeDefinitionParams,
    ) -> Result<Option<request::GotoTypeDefinitionResponse>> {
        let at = params.text_document_position_params;
        let language = self
            .indexer
            .language(&at.text_document.uri)
            .unwrap_or(Language::Java);
        if !features(language).answers_type_definition() {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        let Some(written) = snapshot.type_of_declaration(&found) else {
            return Ok(None);
        };
        let Some(declaration) = snapshot.of_type(&written, found.file) else {
            return Ok(None);
        };
        Ok(Some(request::GotoTypeDefinitionResponse::Scalar(Location {
            uri: declaration.file.uri.clone(),
            range: at_name(declaration.selection()),
        })))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let found = snapshot.declaration_at(&at.text_document.uri, cursor(at.position));
        let Some(found) = found else {
            return Ok(None);
        };
        let Some(signature) = snapshot.signature_of(&found) else {
            return Ok(None);
        };
        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
                language: protocol(found.file.language).language_id().to_owned(),
                value: signature,
            })),
            range: None,
        }))
    }

    async fn goto_implementation(
        &self,
        params: request::GotoImplementationParams,
    ) -> Result<Option<request::GotoImplementationResponse>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let places: Vec<Location> = snapshot
            .implementations_at(&at.text_document.uri, cursor(at.position))
            .into_iter()
            .map(|found| Location {
                uri: found.file.uri.clone(),
                range: at_name(found.selection()),
            })
            .collect();
        Ok(Some(request::GotoImplementationResponse::Array(places)))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let at = params.text_document_position;
        let snapshot = self.indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        let mut uses = snapshot.uses_of(&found);
        for also in features(found.file.language).also_referenced(&snapshot, &found) {
            uses.extend(snapshot.uses_of(&also));
        }
        if found.file.language == Language::Java {
            uses = as_jdtls_reports(&snapshot, &found, uses);
        }
        uses.sort_by(|left, right| (left.0.as_str(), left.1).cmp(&(right.0.as_str(), right.1)));
        uses.dedup();
        let places = uses
            .into_iter()
            .map(|(uri, at)| Location {
                uri: uri.clone(),
                range: at_name(at),
            })
            .collect();
        Ok(Some(places))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let at = params.text_document_position_params;
        let uri = at.text_document.uri;
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&uri) else {
            return Ok(None);
        };
        if let Some(places) = crate::feature::highlight::special(&snapshot, file, cursor(at.position)) {
            return Ok(Some(places.into_iter().map(highlighted).collect()));
        }
        let Some(found) = snapshot.declaration_at(&uri, cursor(at.position)) else {
            return Ok(None);
        };
        let highlights = snapshot
            .uses_in(&found, Some(&uri))
            .into_iter()
            .map(|(_, at)| highlighted(at))
            .collect();
        Ok(Some(highlights))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let at = params.text_document_position;
        let snapshot = self.indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        let mut places: Vec<(&Url, Position)> = Vec::new();
        for tied in snapshot.tied_to(&found) {
            places.extend(snapshot.uses_of(&tied));
        }
        places.extend(snapshot.javadoc_tags_of(&found));
        places.sort_by(|left, right| (left.0.as_str(), left.1).cmp(&(right.0.as_str(), right.1)));
        places.dedup();
        for (uri, at) in places {
            let end = Position {
                line: at.line,
                column: at.column + found.name.text.chars().count() as u32,
            };
            changes.entry(uri.clone()).or_default().push(TextEdit {
                range: Range {
                    start: lsp_position(at),
                    end: lsp_position(end),
                },
                new_text: params.new_name.clone(),
            });
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&at.text_document.uri) else {
            return Ok(None);
        };
        let Some(help) = crate::feature::signature::help_at(&snapshot, file, cursor(at.position)) else {
            return Ok(None);
        };
        let label = format!("{}({})", help.name, help.parameters.join(", "));
        let parameters = help
            .parameters
            .iter()
            .map(|written| ParameterInformation {
                label: ParameterLabel::Simple(written.clone()),
                documentation: None,
            })
            .collect();
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation: None,
                parameters: Some(parameters),
                active_parameter: help.active.map(|at| at as u32),
            }],
            active_signature: Some(0),
            active_parameter: help.active.map(|at| at as u32),
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let at = params.text_document_position;
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&at.text_document.uri) else {
            return Ok(None);
        };
        let implied = self.indexer.implied_package(&at.text_document.uri);
        let offers = crate::feature::completion::offers(
            &snapshot,
            file,
            cursor(at.position),
            implied.as_deref(),
        );
        Ok(Some(CompletionResponse::Array(
            offers
                .into_iter()
                .map(|offer| CompletionItem {
                    label: offer.name,
                    sort_text: Some(offer.rank),
                    ..CompletionItem::default()
                })
                .collect(),
        )))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let Some(text) = self.indexer.text(&uri) else {
            return Ok(None);
        };
        let language = self.indexer.language(&uri).unwrap_or(Language::Java);
        let at = Position {
            line: params.position.line,
            column: params.position.character,
        };
        let Some(name) = self.indexer.syntax(language).name_at(&text, at) else {
            return Ok(None);
        };
        let is_the_workspaces = !name.in_package_or_import && self.indexer.is_declared(&name.text);
        if !is_the_workspaces {
            return Ok(None);
        }
        Ok(Some(PrepareRenameResponse::Range(Range {
            start: lsp_position(name.start),
            end: lsp_position(name.end),
        })))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        Ok(Some(self.indexer.types_matching(&params.query)))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        let Some(text) = self.indexer.text(&uri) else {
            return Ok(None);
        };
        let language = self.indexer.language(&uri).unwrap_or(Language::Java);
        let folds = self.indexer.syntax(language).folds(&text);
        Ok(Some(folds.into_iter().map(folding_range).collect()))
    }
}

fn folding_range(fold: Fold) -> FoldingRange {
    FoldingRange {
        start_line: fold.first_line,
        end_line: fold.last_line,
        ..FoldingRange::default()
    }
}

#[allow(deprecated)]
fn symbol_of(entry: Outline) -> DocumentSymbol {
    let selection = Range {
        start: lsp_position(entry.selection),
        end: lsp_position(entry.selection),
    };
    DocumentSymbol {
        name: entry.name,
        detail: None,
        kind: symbol_kind(entry.kind),
        tags: None,
        deprecated: None,
        range: Range {
            start: lsp_position(entry.start),
            end: lsp_position(entry.end),
        },
        selection_range: selection,
        children: None,
    }
}

fn symbol_kind(kind: Kind) -> SymbolKind {
    match kind {
        Kind::Class => SymbolKind::CLASS,
        Kind::Interface => SymbolKind::INTERFACE,
        Kind::Enum => SymbolKind::ENUM,
        Kind::EnumMember => SymbolKind::ENUM_MEMBER,
        Kind::Field => SymbolKind::FIELD,
        Kind::Constant => SymbolKind::CONSTANT,
        Kind::Method => SymbolKind::METHOD,
        Kind::Constructor => SymbolKind::CONSTRUCTOR,
        Kind::Property => SymbolKind::PROPERTY,
        Kind::Function => SymbolKind::FUNCTION,
        Kind::Object => SymbolKind::OBJECT,
    }
}
