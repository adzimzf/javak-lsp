//! The LSP server: every request is answered out of the parser's view of
//! the open files.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::indexer::Indexer;
use crate::parser::treesitter::Language;
use crate::parser::{Fold, Kind, Outline, Position};

pub fn backend_over(indexer: Arc<Indexer>, client: Client) -> Backend {
    Backend { indexer, client }
}

pub struct Backend {
    indexer: Arc<Indexer>,
    client: Client,
}

impl Backend {
    /// Everything the server has to say about one file: its syntax errors
    /// and, where its path names a package it does not declare, that.
    fn diagnostics(&self, uri: &Url) -> Vec<Diagnostic> {
        let Some(text) = self.indexer.text(uri) else {
            return Vec::new();
        };
        let language = self.indexer.language(uri).unwrap_or(Language::Java);
        let syntax = self.indexer.syntax(language);
        let mut diagnostics = Vec::new();
        if let Some(expected) = self.indexer.package_of_path(uri) {
            if syntax.package(&text).is_none() {
                diagnostics.push(reported(
                    Position { line: 0, column: 0 },
                    format!("Missing package declaration (`{expected}`)"),
                ));
            }
        }
        diagnostics.extend(
            syntax
                .syntax_errors(&text)
                .into_iter()
                .map(|finding| reported(finding.at, finding.message)),
        );
        diagnostics
    }

    async fn publish(&self, uri: Url) {
        let diagnostics = self.diagnostics(&uri);
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }
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

fn language_of(id: &str, uri: &Url) -> Language {
    match id {
        "kotlin" => Language::Kotlin,
        "java" => Language::Java,
        _ if uri.as_str().ends_with(".kt") || uri.as_str().ends_with(".kts") => Language::Kotlin,
        _ => Language::Java,
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if !self.indexer.workspace_pinned.load(Ordering::Relaxed) {
            if let Some(root) = params
                .root_uri
                .as_ref()
                .and_then(|uri| uri.to_file_path().ok())
            {
                let _ = self.indexer.workspace_root.set(root);
            }
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
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
        let Some(signature) = found.as_ref().and_then(|found| snapshot.signature_of(found)) else {
            return Ok(None);
        };
        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
                language: "java".to_owned(),
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
        let places = snapshot
            .uses_of(&found)
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
        if let Some(places) = crate::highlight::special(&snapshot, file, cursor(at.position)) {
            return Ok(Some(places.into_iter().map(highlighted).collect()));
        }
        let Some(found) = snapshot.declaration_at(&uri, cursor(at.position)) else {
            return Ok(None);
        };
        let highlights = snapshot
            .uses_of(&found)
            .into_iter()
            .filter(|(used_in, _)| **used_in == uri)
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
        let Some(help) = crate::signature::help_at(&snapshot, file, cursor(at.position)) else {
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
        let language = self
            .indexer
            .language(&at.text_document.uri)
            .unwrap_or(Language::Java);
        let syntax = self.indexer.syntax(language);
        let offers = crate::completion::offers(&snapshot, syntax, file, cursor(at.position));
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
    }
}
