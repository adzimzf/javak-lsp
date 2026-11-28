//! The LSP server: every request is answered out of the parser's view of
//! the open files and the libraries they name. How the protocol names a
//! language is behind [`LanguageProtocol`], implemented in `java` and
//! `kotlin`; dependency sync runs from [`dependencies`].

mod dependencies;
pub mod service;
pub mod java;
pub mod kotlin;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use serde_json::Value;
use tower_lsp::{Client, LanguageServer};

use crate::feature::call_hierarchy;
use crate::debugger::commands::{self as debug_commands, CommandOutcome, DebugCommands};
use crate::feature::format;
use crate::feature::highlight;
use crate::feature::features;
use crate::feature::semantic::{compiler, Found, Snapshot};
use crate::indexer::Indexer;
use crate::language::Language;
use crate::parser::{Fold, FoldKind, Kind, Outline, Position};
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
        symbol_tags: AtomicBool::new(false),
        work_done_progress: AtomicBool::new(false),
    }
}

pub struct Backend {
    indexer: Arc<Indexer>,
    client: Client,
    dependencies: Arc<DependencySync>,
    /// The `vscode.java.*` commands of jdtls' debug plugin.
    debug: Arc<DebugCommands>,
    /// Whether the client reads a symbol's `tags`; one that does not is
    /// told a symbol is deprecated by its `deprecated` field, as jdtls does.
    symbol_tags: AtomicBool,
    /// Whether the client shows `$/progress` (`window.workDoneProgress`).
    work_done_progress: AtomicBool,
}

impl Backend {
    /// Decompile the class whose stub is at `uri`, telling the user while
    /// it runs (a second or a few: a JVM starts). Whether it was.
    async fn decompile_class_at(&self, uri: &Url) -> bool {
        let libraries = Arc::clone(&self.indexer.libraries);
        let Some(file) = libraries.file_at(uri).filter(|file| libraries.is_undecompiled_stub(file)) else {
            return false;
        };
        let class = file.entry.trim_end_matches(".java").replace('/', ".");
        let progress = self.begin_progress("Decompiling", &class).await;
        let (jar, entry) = (file.jar.clone(), file.entry.clone());
        let outcome = tokio::task::spawn_blocking(move || crate::dependency_sync::decompile::decompile_class(&jar, &entry))
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
        self.end_progress(progress).await;
        match outcome {
            Ok(_) => {
                libraries.reread(&file);
                true
            }
            Err(message) => {
                log::warn!("decompiling {class}: {message}");
                self.client
                    .show_message(
                        MessageType::WARNING,
                        format!("javak-lsp: cannot decompile {class} ({message}); showing its declarations"),
                    )
                    .await;
                false
            }
        }
    }

    /// Show `title: message` as work in progress: `$/progress` where the
    /// client shows it, else a message.
    async fn begin_progress(&self, title: &str, message: &str) -> Option<NumberOrString> {
        if !self.work_done_progress.load(Ordering::Relaxed) {
            self.client
                .show_message(MessageType::INFO, format!("javak-lsp: {title} {message}\u{2026}"))
                .await;
            return None;
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let token = NumberOrString::String(format!("javak-lsp/{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        let created = self
            .client
            .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams { token: token.clone() })
            .await;
        if created.is_err() {
            return None;
        }
        self.client
            .send_notification::<notification::Progress>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                    title: title.to_owned(),
                    cancellable: Some(false),
                    message: Some(message.to_owned()),
                    percentage: None,
                })),
            })
            .await;
        Some(token)
    }

    async fn end_progress(&self, token: Option<NumberOrString>) {
        let Some(token) = token else {
            return;
        };
        self.client
            .send_notification::<notification::Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd { message: None })),
            })
            .await;
    }

    fn prepared_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let at = params.text_document_position_params;
        if self.indexer.language(&at.text_document.uri) != Some(Language::Java) {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&at.text_document.uri) else {
            return Ok(None);
        };
        let item = crate::feature::type_hierarchy::item_at(&snapshot, file, cursor(at.position));
        Ok(item.map(|item| vec![type_item(&snapshot, &item, None)]))
    }

    fn type_hierarchy_related(indexer: &Indexer, item: &TypeHierarchyItem, up: bool) -> Result<Option<Vec<TypeHierarchyItem>>> {
        use crate::feature::type_hierarchy as hierarchy;
        let snapshot = indexer.snapshot();
        let place = |key: &str| -> Option<(Url, Position)> {
            let data = item.data.as_ref()?.get(key)?;
            let uri = Url::parse(data.get("uri")?.as_str()?).ok()?;
            let at = Position {
                line: data.get("line")?.as_u64()? as u32,
                column: data.get("character")?.as_u64()? as u32,
            };
            Some((uri, at))
        };
        let Some((uri, at)) = place("type") else {
            return Ok(None);
        };
        let Some(of) = snapshot.declaration_at(&uri, at) else {
            return Ok(None);
        };
        // The method asked about first, which every expansion matches.
        let target = place("method").and_then(|(uri, at)| snapshot.declaration_at(&uri, at));
        let method = target.as_ref().and_then(|target| {
            if (&target.file.uri, target.selection()) == (&of.file.uri, of.selection()) {
                return Some(target.clone());
            }
            None
        });
        let of = match &method {
            Some(method) => hierarchy::owner(method).unwrap_or(of),
            None => of,
        };
        let asked = hierarchy::Item {
            of,
            method: method.or_else(|| target.clone()),
            without_method: false,
        };
        let related = if up {
            hierarchy::supertypes(&snapshot, &asked)
        } else {
            let mut below = hierarchy::subtypes(&snapshot, &asked);
            // A file no build compiles is in jdtls's invisible project,
            // whose one source folder is the one its package implies: a
            // subtype in another such folder is in no project it sees.
            if !indexer.on_source_root(&asked.of.file.uri) {
                let root = package_root(&snapshot, asked.of.file);
                below.retain(|related| {
                    related.of.file.library || package_root(&snapshot, related.of.file) == root
                });
            }
            below
        };
        Ok(Some(
            related
                .iter()
                .map(|related| type_item(&snapshot, related, target.as_ref()))
                .collect(),
        ))
    }


    /// The edits that format a Java file, or the lines `range` touches.
    /// Other files are not formatted.
    fn formatted(
        &self,
        uri: &Url,
        options: &FormattingOptions,
        range: Option<(Position, Position)>,
    ) -> Option<Vec<TextEdit>> {
        let text = self.indexer.text(uri)?;
        if self.indexer.language(uri)? != Language::Java {
            return None;
        }
        let options = format::Options {
            tab_size: options.tab_size.max(1) as usize,
            insert_spaces: options.insert_spaces,
        };
        let edits = format::java(&text, options, range)
            .into_iter()
            .map(|edit| TextEdit {
                range: span_range((edit.start, edit.end)),
                new_text: edit.text,
            })
            .collect();
        Some(edits)
    }

    /// Everything the server has to say about one file: its syntax errors;
    /// where its path names a package it does not declare, that; what it
    /// writes that the project's Java level does not have; the calls no
    /// method of their name can take; and what else the Eclipse compiler
    /// says of its text ([`compiler::reports`]).
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
        let mut reports = Vec::new();
        if let Some(file) = snapshot.file(uri) {
            if let Some(level) = self.indexer.java_level_of(uri) {
                findings.extend(syntax.above_java_level(&file.tree, level));
            }
            findings.extend(snapshot.inapplicable_calls(file));
            let on_source_root = self.indexer.on_source_root(uri);
            // So far `diagnostics` holds the package mismatch alone.
            let elsewhere = compiler::Errors {
                any: !findings.is_empty() || !diagnostics.is_empty(),
                wrong_package: !diagnostics.is_empty(),
            };
            reports = compiler::reports(file, on_source_root, elsewhere);
            if on_source_root {
                reports.extend(snapshot.type_reports(file));
            }
            // Eclipse weighs `@SuppressWarnings` only in a file without errors.
            let has_errors = !findings.is_empty() || reports.iter().any(|report| report.level == compiler::Level::Error);
            if has_errors {
                reports.retain(|report| !report.finding.message.starts_with("At least one of the problems in category"));
            }
        }
        findings.sort_by_key(|finding| finding.at);
        diagnostics.extend(
            findings
                .into_iter()
                .map(|finding| {
                    let end = finding.end;
                    let mut diagnostic = reported(finding.at, finding.message);
                    if let Some(end) = end {
                        diagnostic.range.end = lsp_position(end);
                    }
                    diagnostic
                }),
        );
        diagnostics.extend(reports.into_iter().map(reported_by_compiler));
        diagnostics
    }

    /// A build file changed: its source level may have, and the
    /// dependencies it names.
    async fn build_file_changed(&self) {
        self.indexer.read_projects();
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

fn is_constructor(found: &crate::feature::semantic::Found<'_>) -> bool {
    matches!(
        found.node.kind.as_str(),
        "constructor_declaration" | "compact_constructor_declaration"
    )
}

/// References as jdtls lists them: a type named in an import is found at
/// the start of the import's qualified name (`org.jsoup.parser.Parser`),
/// a constructor called by `new Reader(in)` at its `new`, and the variable
/// of an enhanced `for` is not listed at its own declaration.
/// A type hierarchy item as jdtls makes one: the type's name and kind, at
/// the method shown where there is one; `null` for a type without the
/// method asked about. Its data says where the type (or method) and the
/// method first asked about are.
fn type_item(
    snapshot: &Snapshot,
    item: &crate::feature::type_hierarchy::Item<'_>,
    target: Option<&Found<'_>>,
) -> TypeHierarchyItem {
    let shown = item.shown();
    let (name, detail) = crate::feature::type_hierarchy::name_and_detail(snapshot, &item.of);
    let kind = if item.without_method {
        SymbolKind::NULL
    } else {
        match item.of.node.kind.as_str() {
            "interface_declaration" | "annotation_type_declaration" => SymbolKind::INTERFACE,
            "enum_declaration" => SymbolKind::ENUM,
            _ => SymbolKind::CLASS,
        }
    };
    let place = |found: &Found<'_>| {
        serde_json::json!({
            "uri": found.file.uri,
            "line": found.selection().line,
            "character": found.selection().column,
        })
    };
    let target = target.or(item.method.as_ref());
    let name_end = Position {
        line: shown.name.start.line,
        column: shown.name.start.column + shown.name.text.split('<').next().unwrap_or_default().trim().encode_utf16().count() as u32,
    };
    TypeHierarchyItem {
        name,
        kind,
        tags: None,
        detail: Some(detail),
        uri: shown.file.uri.clone(),
        range: span_range((shown.node.start, shown.node.end)),
        selection_range: span_range((shown.name.start, name_end)),
        data: Some(serde_json::json!({
            "type": place(shown),
            "method": target.map(place),
        })),
    }
}

/// The names jdtls puts a code lens on: each type, nested ones too, and
/// each method, constructor or annotation element a type declares — not
/// what is declared inside a method.
/// The folder a file's packages start at: its own folder less the
/// folders its `package` names.
fn package_root(snapshot: &crate::feature::semantic::Snapshot, file: &crate::feature::semantic::FileView) -> Option<std::path::PathBuf> {
    let mut folder = file.uri.to_file_path().ok()?.parent()?.to_path_buf();
    let package = crate::feature::semantic::package_of(snapshot, file).unwrap_or_default();
    for segment in package.split('.').filter(|segment| !segment.is_empty()).rev() {
        if !folder.ends_with(segment) {
            return None;
        }
        folder.pop();
    }
    Some(folder)
}

fn lens_names<'a>(node: &'a crate::parser::Syn, names: &mut Vec<&'a crate::parser::Syn>) {
    // What Lombok generates has no lens, as in jdtls.
    for child in node
        .children
        .iter()
        .filter(|child| !child.is_generated() && !crate::feature::semantic::is_lombok_generated(child))
    {
        let is_type = crate::feature::semantic::is_type_declaration(child);
        let is_member = matches!(
            child.kind.as_str(),
            "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "annotation_type_element_declaration"
        );
        if (is_type || is_member)
            && let Some(name) = child.child("name")
        {
            names.push(name);
        }
        // Into a type's body, never into a member's.
        let container = matches!(
            child.kind.as_str(),
            "class_body" | "interface_body" | "enum_body" | "enum_body_declarations" | "annotation_type_body"
        ) || is_type;
        if container {
            lens_names(child, names);
        }
    }
}

/// Whether the name at `at` is called as `super.name(…)`.
fn is_super_call(snapshot: &crate::feature::semantic::Snapshot, uri: &Url, at: Position) -> bool {
    let Some(file) = snapshot.file(uri) else {
        return false;
    };
    crate::feature::semantic::path_to(&file.tree, at)
        .iter()
        .rev()
        .find(|node| node.kind == "method_invocation")
        .is_some_and(|call| {
            call.child("name").is_some_and(|name| name.start == at)
                && call.child("object").is_some_and(|object| object.kind == "super")
        })
}

fn as_jdtls_reports<'a>(
    snapshot: &'a crate::feature::semantic::Snapshot,
    found: &crate::feature::semantic::Found<'_>,
    uses: Vec<(&'a Url, Position)>,
) -> Vec<(&'a Url, Position)> {
    let declared_by_for = found.node.kind == "enhanced_for_statement";
    let constructor = is_constructor(found);
    let is_type = crate::feature::semantic::is_type_declaration(found.node);
    uses.into_iter()
        .filter(|(uri, at)| {
            !(declared_by_for && **uri == found.file.uri && *at == found.selection())
        })
        .map(|(uri, at)| {
            let Some(file) = snapshot.file(uri) else {
                return (uri, at);
            };
            let path = crate::feature::semantic::path_to(&file.tree, at);
            let creation = path.iter().rev().find(|node| {
                node.kind == "object_creation_expression"
                    && node.child("type").is_some_and(|created| created.covers(at))
            });
            if let Some(creation) = creation.filter(|_| constructor) {
                return (uri, creation.start);
            }
            let import = path.iter().find(|node| node.kind == "import_declaration");
            let name = import.and_then(|import| {
                import.children.iter().find(|child| {
                    matches!(child.kind.as_str(), "scoped_identifier" | "identifier")
                })
            });
            if let Some(name) = name {
                return (uri, name.start);
            }
            // A type named with its qualifier (`org.demo.Shape`,
            // `Outer.Inner`) is reported from where the qualifier starts.
            if is_type {
                let qualified = path.iter().rev().find(|node| {
                    matches!(
                        node.kind.as_str(),
                        "scoped_type_identifier" | "scoped_identifier" | "field_access"
                    ) && node.children.last().is_some_and(|last| last.start == at)
                        && node
                            .text
                            .chars()
                            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$' | '.') || character.is_whitespace())
                });
                if let Some(qualified) = qualified {
                    return (uri, qualified.start);
                }
            }
            (uri, at)
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

fn reported_by_compiler(report: compiler::Report) -> Diagnostic {
    let severity = match report.level {
        compiler::Level::Error => DiagnosticSeverity::ERROR,
        compiler::Level::Warning => DiagnosticSeverity::WARNING,
        compiler::Level::Information => DiagnosticSeverity::INFORMATION,
    };
    let end = report.finding.end;
    let mut diagnostic = Diagnostic {
        severity: Some(severity),
        tags: report.unnecessary.then(|| vec![DiagnosticTag::UNNECESSARY]),
        ..reported(report.finding.at, report.finding.message)
    };
    if let Some(end) = end {
        diagnostic.range.end = lsp_position(end);
    }
    diagnostic
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

fn highlighted(place: highlight::Highlight) -> DocumentHighlight {
    DocumentHighlight {
        range: Range {
            start: lsp_position(place.start),
            end: lsp_position(place.end),
        },
        kind: Some(if place.write {
            DocumentHighlightKind::WRITE
        } else {
            DocumentHighlightKind::READ
        }),
    }
}

/// An empty range at a declaration's name: what a client is sent to.
/// A Java proposal as the item jdtls sends: the label with its parameters
/// beside it and the type or package as their description, the kind, what
/// it inserts (a snippet where it writes arguments), its detail and
/// documentation, and the import it adds.
fn java_completion_item(
    proposal: crate::feature::completion::Proposal,
    imports_at: (tower_lsp::lsp_types::Position, bool),
    cursor_at: tower_lsp::lsp_types::Position,
) -> CompletionItem {
    use crate::feature::completion::ItemKind;
    let shown = proposal.shown;
    let kind = match shown.kind.unwrap_or_default() {
        ItemKind::Text => CompletionItemKind::TEXT,
        ItemKind::Method => CompletionItemKind::METHOD,
        ItemKind::Constructor => CompletionItemKind::CONSTRUCTOR,
        ItemKind::Field => CompletionItemKind::FIELD,
        ItemKind::Constant => CompletionItemKind::CONSTANT,
        ItemKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        ItemKind::Variable => CompletionItemKind::VARIABLE,
        ItemKind::Class => CompletionItemKind::CLASS,
        ItemKind::Interface => CompletionItemKind::INTERFACE,
        ItemKind::Enum => CompletionItemKind::ENUM,
        ItemKind::Struct => CompletionItemKind::STRUCT,
        ItemKind::Module => CompletionItemKind::MODULE,
        ItemKind::Keyword => CompletionItemKind::KEYWORD,
        ItemKind::Snippet => CompletionItemKind::SNIPPET,
    };
    let label = shown.label.unwrap_or(proposal.label);
    let label_details = (proposal.label_detail.is_some() || shown.description.is_some()).then_some(
        CompletionItemLabelDetails {
            detail: proposal.label_detail,
            description: shown.description,
        },
    );
    let additional_text_edits = shown.import.map(|import| {
        let (at, after_a_line) = imports_at;
        let new_text = if after_a_line { format!("\n{}", import.trim_end()) } else { import };
        vec![TextEdit { range: Range { start: at, end: at }, new_text }]
    });
    let inserted = shown.insert.unwrap_or(proposal.name.clone());
    // What a qualified name being written is replaced with, whole.
    let text_edit = shown.replaces.map(|from| {
        CompletionTextEdit::Edit(TextEdit {
            range: Range { start: lsp_position(from), end: cursor_at },
            new_text: inserted.clone(),
        })
    });
    CompletionItem {
        filter_text: Some(proposal.name.clone()),
        insert_text: Some(inserted),
        text_edit,
        insert_text_format: shown.snippet.then_some(InsertTextFormat::SNIPPET),
        label,
        label_details,
        kind: Some(kind),
        detail: shown.detail,
        documentation: shown.documentation.map(|text| {
            Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value: text })
        }),
        deprecated: shown.deprecated.then_some(true),
        tags: shown.deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
        sort_text: Some(proposal.sort),
        additional_text_edits,
        ..CompletionItem::default()
    }
}

/// Where an import goes in `file`: after its last import, else after its
/// package declaration (the edit then starts a line of its own), else at
/// its start.
fn import_position(file: &crate::feature::semantic::FileView) -> (tower_lsp::lsp_types::Position, bool) {
    let children = &file.tree.children;
    let last = children
        .iter()
        .rev()
        .find(|child| child.kind == "import_declaration")
        .or_else(|| children.iter().find(|child| child.kind == "package_declaration"));
    match last {
        Some(declaration) => (lsp_position(declaration.end), true),
        None => (tower_lsp::lsp_types::Position { line: 0, character: 0 }, false),
    }
}

/// Where a declaration's name is, start to end, as a location shows it.
fn name_range(name: &crate::parser::Syn) -> Range {
    Range { start: lsp_position(name.start), end: lsp_position(name.end) }
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
        let tags = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|document| document.document_symbol.as_ref())
            .and_then(|symbols| symbols.tag_support.as_ref())
            .is_some_and(|support| support.value_set.contains(&SymbolTag::DEPRECATED));
        self.symbol_tags.store(tags, Ordering::Relaxed);
        let progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or(false);
        self.work_done_progress.store(progress, Ordering::Relaxed);
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
            indexer.read_projects();
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
                declaration_provider: Some(DeclarationCapability::Simple(true)),
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
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(true),
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                    SemanticTokensOptions {
                        legend: SemanticTokensLegend {
                            token_types: crate::feature::semantic::TOKEN_TYPES
                                .iter()
                                .map(|name| SemanticTokenType::new(name))
                                .collect(),
                            token_modifiers: crate::feature::semantic::TOKEN_MODIFIERS
                                .iter()
                                .map(|name| SemanticTokenModifier::new(name))
                                .collect(),
                        },
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        range: Some(false),
                        ..SemanticTokensOptions::default()
                    },
                )),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
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
        if language == Language::Java {
            let tree = self.indexer.syntax(language).tree(&text);
            let tags = self.symbol_tags.load(Ordering::Relaxed);
            let symbols = crate::feature::outline::java(&tree)
                .into_iter()
                .map(|symbol| java_symbol(symbol, tags))
                .collect();
            return Ok(Some(DocumentSymbolResponse::Nested(symbols)));
        }
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
        let target = found.file.uri.clone();
        let mut range = name_range(found.name);
        // jdtls goes from the loop (or its label) a `break`/`continue`
        // leaves to the end of that `break`/`continue` keyword.
        if let Some(keyword) = snapshot
            .jump_at(&at.text_document.uri, cursor(at.position))
            .and_then(|jump| jump.children.first())
        {
            range = Range { start: lsp_position(found.node.start), end: lsp_position(keyword.end) };
        }
        let location = Location { uri: target.clone(), range };
        drop(snapshot);
        // A class read from its class file is decompiled when it is opened,
        // as jdtls does; then the declaration is found again in the code.
        if self.decompile_class_at(&target).await {
            let snapshot = self.indexer.snapshot();
            if let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position)) {
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: found.file.uri.clone(),
                    range: name_range(found.name),
                })));
            }
        }
        Ok(Some(GotoDefinitionResponse::Scalar(location)))
    }

    /// The method the one at the cursor overrides, as jdtls answers: a
    /// declaration request names nothing else.
    async fn goto_declaration(
        &self,
        params: request::GotoDeclarationParams,
    ) -> Result<Option<request::GotoDeclarationResponse>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        let Some(overridden) = snapshot.overridden(&found) else {
            return Ok(None);
        };
        Ok(Some(request::GotoDeclarationResponse::Scalar(Location {
            uri: overridden.file.uri.clone(),
            range: name_range(overridden.name),
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
        if language == Language::Java {
            match found.node.kind.as_str() {
                // jdtls goes to the type of a variable, not of what a
                // method returns; a constructor's is its class.
                "method_declaration" => return Ok(None),
                "constructor_declaration" | "compact_constructor_declaration" => {
                    let path = crate::feature::semantic::path_to(&found.file.tree, found.node.start);
                    let Some(class) = path
                        .iter()
                        .rev()
                        .find(|node| !std::ptr::eq(**node, found.node) && crate::feature::semantic::is_type_declaration(node))
                        .and_then(|class| class.child("name"))
                    else {
                        return Ok(None);
                    };
                    return Ok(Some(request::GotoTypeDefinitionResponse::Scalar(Location {
                        uri: found.file.uri.clone(),
                        range: name_range(class),
                    })));
                }
                // A type variable is a type: its own declaration.
                "type_parameter" => {
                    return Ok(Some(request::GotoTypeDefinitionResponse::Scalar(Location {
                        uri: found.file.uri.clone(),
                        range: name_range(found.name),
                    })));
                }
                _ => {}
            }
        }
        let Some(written) = snapshot.type_of_declaration(&found) else {
            return Ok(None);
        };
        let Some(declaration) = snapshot.of_type(&written, found.file) else {
            return Ok(None);
        };
        Ok(Some(request::GotoTypeDefinitionResponse::Scalar(Location {
            uri: declaration.file.uri.clone(),
            range: name_range(declaration.name),
        })))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let Some(hover) = snapshot.hover_at(&at.text_document.uri, cursor(at.position)) else {
            let package = snapshot.package_at(&at.text_document.uri, cursor(at.position));
            return Ok(package.map(|package| Hover {
                contents: HoverContents::Array(vec![MarkedString::LanguageString(LanguageString {
                    language: "java".to_owned(),
                    value: package,
                })]),
                range: None,
            }));
        };
        let signature = MarkedString::LanguageString(LanguageString {
            language: protocol(hover.found.file.language).language_id().to_owned(),
            value: hover.signature,
        });
        if hover.found.file.language != Language::Java {
            return Ok(Some(Hover {
                contents: HoverContents::Scalar(signature),
                range: None,
            }));
        }
        // jdtls follows the signature with the javadoc; a declaration in
        // the workspace's sources says nothing of where it is.
        let mut parts = vec![signature];
        if let Some(documentation) = hover.documentation {
            parts.push(MarkedString::String(documentation));
        }
        Ok(Some(Hover {
            contents: HoverContents::Array(parts),
            range: None,
        }))
    }

    async fn goto_implementation(
        &self,
        params: request::GotoImplementationParams,
    ) -> Result<Option<request::GotoImplementationResponse>> {
        // Searches the libraries: off the request loop, so that other
        // requests are answered meanwhile.
        let indexer = Arc::clone(&self.indexer);
        blocking(move || goto_implementation_now(&indexer, params)).await
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        // Searches the libraries: off the request loop, so that other
        // requests are answered meanwhile.
        let indexer = Arc::clone(&self.indexer);
        blocking(move || references_now(&indexer, params)).await
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
        if let Some(places) = highlight::special(&snapshot, file, cursor(at.position)) {
            return Ok(Some(places.into_iter().map(highlighted).collect()));
        }
        let found = highlight::var_at(&snapshot, file, cursor(at.position))
            .or_else(|| snapshot.declaration_at(&uri, cursor(at.position)));
        let Some(mut found) = found else {
            return Ok(None);
        };
        // The class `new Point(…)` names is the class, though the
        // constructor it calls is what a definition goes to.
        let is_constructor = matches!(
            found.node.kind.as_str(),
            "constructor_declaration" | "compact_constructor_declaration"
        );
        let at_the_declaration =
            found.name.covers(cursor(at.position)) || found.name.end == cursor(at.position);
        if is_constructor
            && !(found.file.uri == uri && at_the_declaration)
            && let Some(class) = snapshot.tied_to(&found).into_iter().nth(1)
        {
            found = class;
        }
        let names_a_type = matches!(
            found.node.kind.as_str(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration"
        );
        let mut uses = snapshot.uses_in(&found, Some(&uri));
        // `new Point(…)` names the class as much as the constructor it
        // calls.
        if names_a_type {
            for constructor in snapshot.tied_to(&found).into_iter().skip(1) {
                uses.extend(
                    snapshot
                        .uses_in(&constructor, Some(&uri))
                        .into_iter()
                        .filter(|(_, at)| *at != constructor.selection()),
                );
            }
        }
        let mut positions: Vec<Position> = uses.into_iter().map(|(_, at)| at).collect();
        if names_a_type {
            positions.extend(highlight::vars_of(&snapshot, file, &found));
        }
        let positions = highlight::of_a_component(file, found.name, cursor(at.position), positions);
        let mut places: Vec<highlight::Highlight> = positions
            .into_iter()
            .filter_map(|at| highlight::shown(file, at, names_a_type))
            .collect();
        // A javadoc naming it (`{@link #area()}`) is read there.
        let length = found.name.text.encode_utf16().count() as u32;
        places.extend(
            snapshot
                .javadoc_uses_of(&found)
                .into_iter()
                .filter(|(in_file, _)| **in_file == uri)
                .map(|(_, at)| highlight::Highlight {
                    start: at,
                    end: Position {
                        line: at.line,
                        column: at.column + length,
                    },
                    write: false,
                }),
        );
        places.extend(highlight::generated(&snapshot, file, &found, names_a_type));
        places.sort();
        places.dedup();
        Ok(Some(places.into_iter().map(highlighted).collect()))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let at = params.text_document_position;
        let snapshot = self.indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        // A library's declaration is not the workspace's to rename.
        if found.file.library {
            return Ok(None);
        }
        let tied = snapshot.tied_to(&found);
        // A method overriding a library's cannot be renamed alone, and the
        // library's is not the workspace's to rename: jdtls refuses.
        if found.file.language == Language::Java && tied.iter().any(|tied| tied.file.library) {
            return Ok(None);
        }
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        let mut places: Vec<(&Url, Position)> = Vec::new();
        for tied in tied {
            places.extend(snapshot.uses_of(&tied));
            places.extend(snapshot.javadoc_uses_of(&tied));
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
        let information = |parameters: &[String]| -> Vec<ParameterInformation> {
            parameters
                .iter()
                .map(|written| ParameterInformation {
                    label: ParameterLabel::Simple(written.clone()),
                    documentation: None,
                })
                .collect()
        };
        if file.language != Language::Java {
            let label = format!("{}({})", help.name, help.parameters.join(", "));
            return Ok(Some(SignatureHelp {
                signatures: vec![SignatureInformation {
                    label,
                    documentation: None,
                    parameters: Some(information(&help.parameters)),
                    active_parameter: help.active.map(|at| at as u32),
                }],
                active_signature: Some(0),
                active_parameter: help.active.map(|at| at as u32),
            }));
        }
        // jdtls offers every overload, `name(String text) : void`.
        let signatures = help
            .overloads
            .iter()
            .map(|overload| {
                let returns = overload
                    .returns
                    .as_ref()
                    .map(|returns| format!(" : {returns}"))
                    .unwrap_or_default();
                SignatureInformation {
                    label: format!("{}({}){returns}", overload.name, overload.parameters.join(", ")),
                    documentation: None,
                    parameters: Some(information(&overload.parameters)),
                    active_parameter: None,
                }
            })
            .collect();
        Ok(Some(SignatureHelp {
            signatures,
            active_signature: Some(help.active_overload as u32),
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
        if file.language == Language::Java {
            // As jdtls offers them: an item per overload, ranked, at most
            // fifty, the list then incomplete; each as jdtls shows it.
            let (proposals, incomplete) = crate::feature::completion::items(
                &snapshot,
                file,
                cursor(at.position),
                implied.as_deref(),
            );
            let imports_at = import_position(file);
            let items = proposals
                .into_iter()
                .map(|proposal| java_completion_item(proposal, imports_at, at.position))
                .collect();
            return Ok(Some(CompletionResponse::List(CompletionList {
                is_incomplete: incomplete,
                items,
            })));
        }
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
        // A name the workspace declares can still mean a library's
        // declaration here (`super.read(…)`), which jdtls does not rename.
        // (One overriding a library's method is refused by the rename.)
        if language == Language::Java
            && self
                .indexer
                .snapshot()
                .declaration_at(&uri, at)
                .is_some_and(|found| found.file.library)
        {
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
        // Searches the libraries: off the request loop, so that other
        // requests are answered meanwhile.
        let indexer = Arc::clone(&self.indexer);
        blocking(move || symbol_now(&indexer, params)).await
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let at = params.text_document_position_params;
        let snapshot = self.indexer.snapshot();
        let Some(found) =
            call_hierarchy::item_at(&snapshot, &at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        Ok(Some(vec![call_item(&snapshot, &found, None)]))
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        // Searches the libraries: off the request loop, so that other
        // requests are answered meanwhile.
        let indexer = Arc::clone(&self.indexer);
        blocking(move || incoming_calls_now(&indexer, params)).await
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        // Searches the libraries: off the request loop, so that other
        // requests are answered meanwhile.
        let indexer = Arc::clone(&self.indexer);
        blocking(move || outgoing_calls_now(&indexer, params)).await
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

    /// As jdtls's references code lens: one at the name of each type and
    /// of each method or constructor a type declares, counted on resolve.
    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        if self.indexer.language(&uri) != Some(Language::Java) {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&uri) else {
            return Ok(None);
        };
        let mut names = Vec::new();
        lens_names(&file.tree, &mut names);
        let lenses = names
            .into_iter()
            .map(|name| CodeLens {
                range: span_range((name.start, name.end)),
                command: None,
                data: Some(serde_json::json!([uri, lsp_position(name.start), "references"])),
            })
            .collect();
        Ok(Some(lenses))
    }

    async fn code_lens_resolve(&self, lens: CodeLens) -> Result<CodeLens> {
        let Some(uri) = lens
            .data
            .as_ref()
            .and_then(|data| data.get(0))
            .and_then(Value::as_str)
            .and_then(|uri| Url::parse(uri).ok())
        else {
            return Ok(lens);
        };
        let position = lens.range.start;
        let places = self
            .references(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: ReferenceContext {
                    include_declaration: false,
                },
            })
            .await?
            .unwrap_or_default();
        let title = if places.len() == 1 {
            "1 reference".to_owned()
        } else {
            format!("{} references", places.len())
        };
        Ok(CodeLens {
            command: Some(Command {
                title,
                command: "java.show.references".to_owned(),
                arguments: Some(vec![
                    serde_json::json!(uri),
                    serde_json::json!(position),
                    serde_json::json!(places),
                ]),
            }),
            ..lens
        })
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        if self.indexer.language(&uri) != Some(Language::Java) {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&uri) else {
            return Ok(None);
        };
        let actions = crate::feature::code_action::actions(&snapshot, file, cursor(params.range.start), cursor(params.range.end));
        Ok(Some(
            actions
                .into_iter()
                .map(|action| {
                    let changes = action
                        .edits
                        .into_iter()
                        .map(|(uri, edits)| {
                            let edits = edits
                                .into_iter()
                                .map(|edit| TextEdit {
                                    range: Range {
                                        start: lsp_position(edit.start),
                                        end: lsp_position(edit.end),
                                    },
                                    new_text: edit.text,
                                })
                                .collect();
                            (uri, edits)
                        })
                        .collect();
                    CodeActionOrCommand::CodeAction(CodeAction {
                        title: action.title,
                        kind: Some(CodeActionKind::from(action.kind.to_owned())),
                        edit: action.disabled.is_none().then(|| WorkspaceEdit {
                            changes: Some(changes),
                            ..WorkspaceEdit::default()
                        }),
                        disabled: action.disabled.map(|reason| CodeActionDisabled { reason: reason.to_owned() }),
                        ..CodeAction::default()
                    })
                })
                .collect(),
        ))
    }

    async fn semantic_tokens_full(&self, params: SemanticTokensParams) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        if self.indexer.language(&uri) != Some(Language::Java) {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&uri) else {
            return Ok(None);
        };
        let mut data = Vec::new();
        let (mut line, mut column) = (0, 0);
        for token in snapshot.semantic_tokens(file) {
            let delta_line = token.at.line - line;
            let delta_start = if delta_line == 0 { token.at.column - column } else { token.at.column };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: token.length,
                token_type: token.kind,
                token_modifiers_bitset: token.modifiers,
            });
            (line, column) = (token.at.line, token.at.column);
        }
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data })))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        if self.indexer.language(&uri) != Some(Language::Java) {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&uri) else {
            return Ok(None);
        };
        let hints = crate::feature::inlay::hints(
            &snapshot,
            file,
            cursor(params.range.start),
            cursor(params.range.end),
        )
        .into_iter()
        .map(|hint| InlayHint {
            position: lsp_position(hint.at),
            label: InlayHintLabel::String(hint.label),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: Some(true),
            data: None,
        })
        .collect();
        Ok(Some(hints))
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        self.prepared_type_hierarchy(params)
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let indexer = Arc::clone(&self.indexer);
        blocking(move || Backend::type_hierarchy_related(&indexer, &params.item, true)).await
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let indexer = Arc::clone(&self.indexer);
        blocking(move || Backend::type_hierarchy_related(&indexer, &params.item, false)).await
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = params.text_document.uri;
        if self.indexer.language(&uri) != Some(Language::Java) {
            return Ok(None);
        }
        let snapshot = self.indexer.snapshot();
        let Some(file) = snapshot.file(&uri) else {
            return Ok(None);
        };
        let answers = params
            .positions
            .into_iter()
            .filter_map(|position| {
                let spans = crate::feature::selection::ranges(&file.tree, &file.text, cursor(position));
                spans.into_iter().rev().fold(None, |parent, (start, end)| {
                    Some(SelectionRange {
                        range: Range {
                            start: lsp_position(start),
                            end: lsp_position(end),
                        },
                        parent: parent.map(Box::new),
                    })
                })
            })
            .collect();
        Ok(Some(answers))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        Ok(self.formatted(&params.text_document.uri, &params.options, None))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let range = (cursor(params.range.start), cursor(params.range.end));
        Ok(self.formatted(&params.text_document.uri, &params.options, Some(range)))
    }
}

/// A member as a call hierarchy item, selected at its name — or at
/// `shown_at`, a call it makes. `data` keeps where its name is, so the
/// item is found again whatever it is shown at.
fn call_item(
    snapshot: &Snapshot,
    found: &Found<'_>,
    shown_at: Option<(Position, Position)>,
) -> CallHierarchyItem {
    let name_end = Position {
        line: found.name.start.line,
        column: found.name.start.column
            + if std::ptr::eq(found.name, found.node) {
                0
            } else {
                found.name.text.encode_utf16().count() as u32
            },
    };
    let selection = shown_at.unwrap_or((found.name.start, name_end));
    let package = snapshot.package(found.file);
    CallHierarchyItem {
        name: call_hierarchy::label(snapshot, found),
        kind: symbol_kind(call_hierarchy::kind_of(found).unwrap_or(Kind::Method)),
        tags: None,
        detail: package,
        uri: found.file.uri.clone(),
        range: span_range((found.node.start, found.node.end)),
        selection_range: span_range(selection),
        data: Some(serde_json::json!({
            "line": found.selection().line,
            "character": found.selection().column,
        })),
    }
}

/// The member a call hierarchy item stands for, found again where its
/// name is.
fn item_of<'a>(snapshot: &'a Snapshot, item: &CallHierarchyItem) -> Option<Found<'a>> {
    let data = item.data.as_ref();
    let at = Position {
        line: data
            .and_then(|data| data["line"].as_u64())
            .map_or(item.selection_range.start.line, |line| line as u32),
        column: data
            .and_then(|data| data["character"].as_u64())
            .map_or(item.selection_range.start.character, |column| column as u32),
    };
    call_hierarchy::item_at(snapshot, &item.uri, at)
}

fn span_range((start, end): (Position, Position)) -> Range {
    Range {
        start: lsp_position(start),
        end: lsp_position(end),
    }
}

fn folding_range(fold: Fold) -> FoldingRange {
    FoldingRange {
        start_line: fold.first_line,
        end_line: fold.last_line,
        kind: fold.kind.map(|kind| match kind {
            FoldKind::Comment => FoldingRangeKind::Comment,
            FoldKind::Imports => FoldingRangeKind::Imports,
            FoldKind::Region => FoldingRangeKind::Region,
        }),
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

#[allow(deprecated)]
fn java_symbol(symbol: crate::feature::outline::Symbol, tags: bool) -> DocumentSymbol {
    use crate::feature::outline::Kind as Outlined;
    let kind = match symbol.kind {
        Outlined::Package => SymbolKind::PACKAGE,
        Outlined::Class => SymbolKind::CLASS,
        Outlined::Interface => SymbolKind::INTERFACE,
        Outlined::Enum => SymbolKind::ENUM,
        Outlined::EnumMember => SymbolKind::ENUM_MEMBER,
        Outlined::Field => SymbolKind::FIELD,
        Outlined::Constant => SymbolKind::CONSTANT,
        Outlined::Method => SymbolKind::METHOD,
        Outlined::Constructor => SymbolKind::CONSTRUCTOR,
    };
    let children: Vec<DocumentSymbol> = symbol
        .children
        .into_iter()
        .map(|child| java_symbol(child, tags))
        .collect();
    DocumentSymbol {
        name: symbol.name,
        detail: symbol.detail,
        kind,
        tags: (symbol.deprecated && tags).then(|| vec![SymbolTag::DEPRECATED]),
        deprecated: (symbol.deprecated && !tags).then_some(true),
        range: Range {
            start: lsp_position(symbol.start),
            end: lsp_position(symbol.end),
        },
        selection_range: Range {
            start: lsp_position(symbol.selection.0),
            end: lsp_position(symbol.selection.1),
        },
        children: (!children.is_empty()).then_some(children),
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

fn references_now(indexer: &Indexer, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let at = params.text_document_position;
        let snapshot = indexer.snapshot();
        let Some(found) = snapshot.declaration_at(&at.text_document.uri, cursor(at.position))
        else {
            return Ok(None);
        };
        let java = found.file.language == Language::Java;
        // jdtls searches the libraries' sources too: the JDK calls a method
        // of the JDK.
        let everywhere = |of: &Found<'_>| {
            let mut uses = snapshot.uses_of(of);
            if java {
                uses.extend(snapshot.library_uses_of(of));
            }
            uses
        };
        let mut uses = snapshot.uses_of(&found);
        let also = features(found.file.language).also_referenced(&snapshot, &found);
        let related = if java && found.node.kind == "method_declaration" {
            snapshot.dispatching_to(&found)
        } else {
            Vec::new()
        };
        // A call of a method it overrides or is overridden by may run it:
        // jdtls lists those calls too — but not `super.m()`, which runs the
        // one it names.
        for related in &related {
            let declared = (&related.file.uri, related.selection());
            uses.extend(snapshot.uses_of(related).into_iter().filter(|(uri, at)| {
                (*uri, *at) != declared && !is_super_call(&snapshot, uri, *at)
            }));
        }
        if java {
            let mut family: Vec<&Found<'_>> = vec![&found];
            family.extend(related.iter());
            for (uri, at, index) in snapshot.library_uses_of_any(&family) {
                let declared = (&family[index].file.uri, family[index].selection());
                if index == 0 || ((uri, at) != declared && !is_super_call(&snapshot, uri, at)) {
                    uses.push((uri, at));
                }
            }
        }
        for also in also {
            // In Java their uses, not where they are declared.
            let declared = (&also.file.uri, also.selection());
            uses.extend(
                everywhere(&also)
                    .into_iter()
                    .filter(|(uri, at)| !java || (*uri, *at) != declared),
            );
        }
        if found.file.language == Language::Java {
            // `this(in)` and `super(in)` call a constructor without naming it.
            if is_constructor(&found) {
                let calls = call_hierarchy::call_sites(&snapshot, &found);
                uses.extend(calls.into_iter().map(|(file, start, _)| (&file.uri, start)));
            }
            uses = as_jdtls_reports(&snapshot, &found, uses);
            uses.extend(snapshot.javadoc_uses_of(&found));
        }
        uses.sort_by(|left, right| (left.0.as_str(), left.1).cmp(&(right.0.as_str(), right.1)));
        uses.dedup();
        let name = found.name.text.as_str();
        let places = uses
            .into_iter()
            .map(|(uri, at)| {
                snapshot.extract_if_searched(uri);
                let end = match snapshot.file(uri) {
                    Some(file) => {
                        let whole = java && file.language == Language::Java;
                        highlight::reference_end(file, at, name, whole)
                    }
                    None => Position {
                        line: at.line,
                        column: at.column + name.encode_utf16().count() as u32,
                    },
                };
                Location {
                    uri: uri.clone(),
                    range: Range { start: lsp_position(at), end: lsp_position(end) },
                }
            })
            .collect();
        Ok(Some(places))
    }

fn goto_implementation_now(indexer: &Indexer, params: request::GotoImplementationParams) -> Result<Option<request::GotoImplementationResponse>> {
        let at = params.text_document_position_params;
        let snapshot = indexer.snapshot();
        let places: Vec<Location> = snapshot
            .implementations_at(&at.text_document.uri, cursor(at.position))
            .into_iter()
            .map(|found| Location {
                uri: found.file.uri.clone(),
                range: name_range(found.name),
            })
            .collect();
        Ok(Some(request::GotoImplementationResponse::Array(places)))
    }

fn incoming_calls_now(indexer: &Indexer, params: CallHierarchyIncomingCallsParams) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let snapshot = indexer.snapshot();
        let Some(of) = item_of(&snapshot, &params.item) else {
            return Ok(None);
        };
        let calls = call_hierarchy::incoming(&snapshot, &of)
            .into_iter()
            .map(|calls| CallHierarchyIncomingCall {
                // As jdtls does: the caller is shown at its first call.
                from: {
                    snapshot.extract_if_searched(&calls.member.file.uri);
                    call_item(&snapshot, &calls.member, calls.at.first().copied())
                },
                from_ranges: calls.at.iter().map(|&span| span_range(span)).collect(),
            })
            .collect();
        Ok(Some(calls))
    }

fn symbol_now(indexer: &Indexer, params: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        let mut symbols = indexer.types_matching(&params.query);
        // jdtls lists the libraries' types too.
        #[allow(deprecated)]
        symbols.extend(indexer.libraries.types_matching(&params.query).into_iter().map(|declared| {
            use crate::indexer::words::TypeKind;
            let start = tower_lsp::lsp_types::Position {
                line: declared.line,
                character: declared.column,
            };
            SymbolInformation {
                name: declared.name,
                kind: match declared.kind {
                    TypeKind::Interface | TypeKind::Annotation => SymbolKind::INTERFACE,
                    TypeKind::Enum => SymbolKind::ENUM,
                    TypeKind::Class | TypeKind::Record => SymbolKind::CLASS,
                },
                tags: None,
                deprecated: None,
                location: Location {
                    uri: declared.uri,
                    range: Range { start, end: start },
                },
                container_name: Some(declared.container),
            }
        }));
        Ok(Some(symbols))
    }

fn outgoing_calls_now(indexer: &Indexer, params: CallHierarchyOutgoingCallsParams) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let snapshot = indexer.snapshot();
        let Some(of) = item_of(&snapshot, &params.item) else {
            return Ok(None);
        };
        let calls = call_hierarchy::outgoing(&snapshot, &of)
            .into_iter()
            .map(|calls| CallHierarchyOutgoingCall {
                to: call_item(&snapshot, &calls.member, None),
                from_ranges: calls.at.iter().map(|&span| span_range(span)).collect(),
            })
            .collect();
        Ok(Some(calls))
    }

/// Runs `work` on a thread of its own and waits for it without holding up
/// the request loop. One such search runs at a time, as the libraries'
/// files it reads stay in memory; one the client stopped waiting for
/// before its turn is not started.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<Option<T>> + Send + 'static) -> Result<Option<T>> {
    static ONE_AT_A_TIME: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    /// Marks the request given up when its future is dropped.
    struct GivenUp(Arc<AtomicBool>);
    impl Drop for GivenUp {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let given_up = Arc::new(AtomicBool::new(false));
    let _guard = GivenUp(Arc::clone(&given_up));
    let Ok(permit) = ONE_AT_A_TIME.acquire().await else {
        return Ok(None);
    };
    let answer = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if given_up.load(Ordering::Relaxed) {
            return Ok(None);
        }
        work()
    })
    .await;
    answer.expect("a request's work panicked")
}
