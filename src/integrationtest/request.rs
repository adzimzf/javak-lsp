//! The request kinds a case can ask, and how each answer is written down.
//!
//! An answer is rendered as the facts a reader would check by eye — a
//! position and the source line there, a name — one per line, in an order
//! that does not depend on the server. JSON shapes, URIs of the temporary
//! workspace and details one server words differently from another are
//! left out, so the same expectation holds whichever server is asked.

use serde_json::{json, Value};

use super::fixture::Fixture;
use super::server::{OnError, Session};

/// What an answer with nothing in it is rendered as.
pub(super) const NOTHING: &str = "(nothing)";

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Definition,
    TypeDefinition,
    Implementation,
    References,
    DocumentHighlight,
    Completion,
    Hover,
    SignatureHelp,
    PrepareRename,
    /// The new name is `renamed`, or `Renamed` where the old one starts
    /// with a capital.
    Rename,
    DocumentSymbol,
    FoldingRange,
    /// The query is the identifier under the cursor.
    WorkspaceSymbol,
    /// Every diagnostic of the cursor's file, pulled; the cursor itself is
    /// not used.
    Diagnostics,
    Declaration,
    /// Asked for the selection `$0`…`$1`, or the empty range at `$0`, with
    /// no diagnostics in its context.
    CodeAction,
    /// The whole file, or the selection `$0`…`$1` when the case has one.
    Formatting,
    /// The whole file.
    InlayHint,
    CodeLens,
    /// Prepared at the cursor, then the calls into and out of the first
    /// item.
    CallHierarchy,
    /// Prepared at the cursor, then the supertypes and subtypes of the
    /// first item.
    TypeHierarchy,
    SemanticTokens,
    SelectionRange,
}

impl Kind {
    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::TypeDefinition => "textDocument/typeDefinition",
            Self::Implementation => "textDocument/implementation",
            Self::References => "textDocument/references",
            Self::DocumentHighlight => "textDocument/documentHighlight",
            Self::Completion => "textDocument/completion",
            Self::Hover => "textDocument/hover",
            Self::SignatureHelp => "textDocument/signatureHelp",
            Self::PrepareRename => "textDocument/prepareRename",
            Self::Rename => "textDocument/rename",
            Self::DocumentSymbol => "textDocument/documentSymbol",
            Self::FoldingRange => "textDocument/foldingRange",
            Self::WorkspaceSymbol => "workspace/symbol",
            Self::Diagnostics => "textDocument/diagnostic",
            Self::Declaration => "textDocument/declaration",
            Self::CodeAction => "textDocument/codeAction",
            Self::Formatting => "textDocument/formatting",
            Self::InlayHint => "textDocument/inlayHint",
            Self::CodeLens => "textDocument/codeLens",
            Self::CallHierarchy => "textDocument/prepareCallHierarchy",
            Self::TypeHierarchy => "textDocument/prepareTypeHierarchy",
            Self::SemanticTokens => "textDocument/semanticTokens/full",
            Self::SelectionRange => "textDocument/selectionRange",
        }
    }

    fn params(self, session: &Session, fixture: &Fixture) -> Value {
        let cursor = &fixture.cursor;
        let document = json!({"uri": session.uri(&cursor.path)});
        let position = json!({"line": cursor.line, "character": cursor.character});
        let end = fixture.selection_end.as_ref().unwrap_or(cursor);
        let selection = json!({
            "start": position,
            "end": {"line": end.line, "character": end.character},
        });
        let options = json!({"tabSize": 4, "insertSpaces": false});
        match self {
            Self::DocumentSymbol
            | Self::FoldingRange
            | Self::Diagnostics
            | Self::CodeLens
            | Self::SemanticTokens => {
                json!({"textDocument": document})
            }
            Self::CodeAction => json!({
                "textDocument": document,
                "range": selection,
                "context": {"diagnostics": []},
            }),
            Self::Formatting if fixture.selection_end.is_some() => json!({
                "textDocument": document,
                "range": selection,
                "options": options,
            }),
            Self::Formatting => json!({"textDocument": document, "options": options}),
            Self::InlayHint => json!({
                "textDocument": document,
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": fixture.line_count(&cursor.path), "character": 0},
                },
            }),
            Self::SelectionRange => json!({"textDocument": document, "positions": [position]}),
            Self::WorkspaceSymbol => json!({"query": fixture.word_at_cursor()}),
            Self::References => json!({
                "textDocument": document,
                "position": position,
                "context": {"includeDeclaration": true},
            }),
            Self::Rename => json!({
                "textDocument": document,
                "position": position,
                "newName": new_name(&fixture.word_at_cursor()),
            }),
            _ => json!({"textDocument": document, "position": position}),
        }
    }

    /// A server says "this cannot be renamed" with an error as often as
    /// with `null`; for the two rename requests both read as nothing.
    fn on_error(self) -> OnError {
        match self {
            Self::Definition
            | Self::TypeDefinition
            | Self::Implementation
            | Self::References
            | Self::DocumentHighlight
            | Self::Completion
            | Self::Hover
            | Self::SignatureHelp
            | Self::DocumentSymbol
            | Self::FoldingRange
            | Self::WorkspaceSymbol
            | Self::Diagnostics => OnError::Panic,
            // Renaming says "cannot" with an error; the rest are requests a
            // server may not serve at all, which reads as nothing.
            _ => OnError::AnswerNothing,
        }
    }

    /// The method that range formatting is asked with instead.
    fn method_for(self, fixture: &Fixture) -> &'static str {
        match self {
            Self::Formatting if fixture.selection_end.is_some() => "textDocument/rangeFormatting",
            _ => self.method(),
        }
    }

    /// Asks the request at the fixture's cursor and renders the answer.
    /// `expected` is the answer the case writes down: a name it lists is
    /// one it asks about, whoever else spells it.
    pub(super) async fn ask(
        self,
        session: &mut Session,
        fixture: &Fixture,
        expected: &str,
        settles: Settles,
    ) -> String {
        let params = self.params(session, fixture);
        let settled = |answer: &Value| match settles {
            Settles::Immediately => true,
            Settles::WhenAnswered => has_content(answer),
        };
        let answer = if matches!(self, Self::Diagnostics) && session.is_child() {
            // Another server may only push them: what it last published.
            session
                .pushed_diagnostics(&fixture.cursor.path, |answer| {
                    matches!(settles, Settles::Immediately) || has_content(answer)
                })
                .await
        } else {
            session
                .request_until(self.method_for(fixture), params, self.on_error(), settled)
                .await
        };
        let answer = match self {
            Self::CallHierarchy => {
                hierarchy(
                    session,
                    &answer,
                    "callHierarchy",
                    ["incomingCalls", "outgoingCalls"],
                )
                .await
            }
            Self::TypeHierarchy => {
                hierarchy(
                    session,
                    &answer,
                    "typeHierarchy",
                    ["supertypes", "subtypes"],
                )
                .await
            }
            _ => answer,
        };
        let view = View {
            session,
            fixture,
            expected,
        };
        let mut lines = match self {
            Self::Definition | Self::TypeDefinition | Self::Implementation | Self::References => {
                view.locations(&answer)
            }
            Self::DocumentHighlight if is_java(&fixture.cursor.path) => {
                view.highlights(&fixture.cursor.path, &answer)
            }
            Self::DocumentHighlight => view.ranges(&fixture.cursor.path, &answer),
            Self::Completion => view.completion_names(&answer),
            Self::Hover => hover_signature(&answer, &fixture.word_at_cursor()),
            Self::SignatureHelp => signature(&answer),
            Self::PrepareRename => view.rename_target(&answer),
            Self::Rename => view.edited_places(&answer),
            Self::DocumentSymbol if is_java(&fixture.cursor.path) => java_outline(&answer),
            Self::DocumentSymbol => document_symbols(&answer),
            Self::FoldingRange => folded_lines(&answer, is_java(&fixture.cursor.path)),
            Self::WorkspaceSymbol => view.workspace_types(&answer),
            Self::Diagnostics => view.reported(&fixture.cursor.path, &answer),
            Self::Declaration => view.locations(&answer),
            Self::CodeAction => action_titles(&answer),
            Self::Formatting => view.formatted(&answer),
            Self::InlayHint => inlay_hints(&answer),
            Self::CodeLens => view.lenses(&answer),
            Self::CallHierarchy | Self::TypeHierarchy => view.hierarchy(&answer),
            Self::SemanticTokens => view.semantic_tokens(&answer),
            Self::SelectionRange => selection_ranges(&answer),
        };
        if lines.is_empty() {
            lines.push(NOTHING.to_owned());
        }
        lines.join("\n") + "\n"
    }
}

/// Whether an empty answer is the case's answer or the server still
/// reading what was just opened. A case that expects nothing says so, and
/// is asked once instead of for the whole patience.
#[derive(Clone, Copy)]
pub(super) enum Settles {
    Immediately,
    WhenAnswered,
}

/// `Renamed` for `Circle`, `renamed` for `area`: a rename keeps the case
/// convention of what it renames.
fn new_name(old_name: &str) -> &'static str {
    let is_type_name = old_name.chars().next().is_some_and(char::is_uppercase);
    if is_type_name {
        "Renamed"
    } else {
        "renamed"
    }
}

/// `null`, `[]` and an empty completion list are what a server says while
/// it is still reading the workspace.
fn has_content(answer: &Value) -> bool {
    match answer {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => fields
            .get("items")
            .and_then(Value::as_array)
            .is_none_or(|items| !items.is_empty()),
        _ => true,
    }
}

/// One answer, or a list of them, as a list.
fn as_list(answer: &Value) -> Vec<&Value> {
    match answer {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().collect(),
        single => vec![single],
    }
}

/// What a rendering needs to turn the server's URIs and positions back into
/// the fixture's paths and source lines.
struct View<'a> {
    session: &'a Session,
    fixture: &'a Fixture,
    expected: &'a str,
}

impl View<'_> {
    /// `/path:line:column  source line`, one-based, in source order.
    fn locations(&self, answer: &Value) -> Vec<String> {
        let places = as_list(answer).into_iter().filter_map(|location| {
            let uri = location
                .get("uri")
                .or_else(|| location.get("targetUri"))?
                .as_str()?;
            let range = location
                .get("targetSelectionRange")
                .or_else(|| location.get("range"))?;
            Some(Place::new(self.session.display(uri), range))
        });
        self.in_source_order(places.collect())
    }

    /// `/path:line:column  message` of each diagnostic of the file at
    /// `path`, in source order. The severity is left out: which check is a
    /// warning and which a hint differs by server, what it reports where
    /// does not.
    fn reported(&self, path: &str, answer: &Value) -> Vec<String> {
        let items = answer.get("items").unwrap_or(answer);
        let mut reported: Vec<(Place, String)> = as_list(items)
            .into_iter()
            .filter_map(|diagnostic| {
                let place = Place::new(format!("/{path}"), diagnostic.get("range")?);
                Some((place, diagnostic["message"].as_str()?.to_owned()))
            })
            .collect();
        reported.sort();
        reported.dedup();
        reported
            .iter()
            .map(|(place, message)| {
                format!(
                    "{}:{}:{}  {message}",
                    place.path,
                    place.line + 1,
                    place.column + 1
                )
            })
            .collect()
    }

    /// The ranges of `answer`, all in the file at `path`.
    fn ranges(&self, path: &str, answer: &Value) -> Vec<String> {
        let places = as_list(answer)
            .into_iter()
            .filter_map(|item| Some(Place::new(format!("/{path}"), item.get("range")?)));
        self.in_source_order(places.collect())
    }

    /// A Java file's highlights, each with where it ends and whether it is
    /// read or written: `path:line:column-end kind  source line`, where
    /// `end` is a column, or `line:column` for a range over lines.
    fn highlights(&self, path: &str, answer: &Value) -> Vec<String> {
        let mut highlights: Vec<(Place, String)> = as_list(answer)
            .into_iter()
            .filter_map(|item| {
                let range = item.get("range")?;
                let place = Place::new(format!("/{path}"), range);
                let end = &range["end"];
                let (line, column) = (end["line"].as_u64()?, end["character"].as_u64()?);
                let end = if line as usize == place.line {
                    format!("{}", column + 1)
                } else {
                    format!("{}:{}", line + 1, column + 1)
                };
                let kind = match item["kind"].as_u64() {
                    Some(2) => " read",
                    Some(3) => " write",
                    _ => "",
                };
                Some((place, format!("-{end}{kind}")))
            })
            .collect();
        highlights.sort();
        highlights.dedup();
        highlights
            .iter()
            .map(|(place, end)| {
                let source = self
                    .fixture
                    .line(&place.path, place.line)
                    .unwrap_or_default();
                format!(
                    "{}:{}:{}{end}  {}",
                    place.path,
                    place.line + 1,
                    place.column + 1,
                    source.trim()
                )
            })
            .collect()
    }

    fn in_source_order(&self, mut places: Vec<Place>) -> Vec<String> {
        places.sort();
        places.dedup();
        places.iter().map(|place| self.show(place)).collect()
    }

    fn show(&self, place: &Place) -> String {
        if place.path.starts_with(super::server::LIBRARY) {
            return place.path.clone();
        }
        let source = self
            .fixture
            .line(&place.path, place.line)
            .unwrap_or_default();
        format!(
            "{}:{}:{}  {}",
            place.path,
            place.line + 1,
            place.column + 1,
            source.trim()
        )
    }

    /// The names offered, best first, as a client would sort them — only
    /// those the fixture itself spells or the case asks for: what a JDK
    /// adds (`hashCode`, `getClass`, …) depends on the JDK the server found,
    /// while a keyword (`package`), what every array has (`length`) or a
    /// package name (`com.demo`) is the answer a case can ask for without
    /// writing it in its sources.
    fn completion_names(&self, answer: &Value) -> Vec<String> {
        let items = answer.get("items").unwrap_or(answer);
        let mut offered: Vec<(String, String)> = as_list(items)
            .into_iter()
            .filter_map(|item| {
                let name = qualified_name_prefix(item["label"].as_str()?);
                let sort_key = item["sortText"].as_str().unwrap_or(name);
                Some((sort_key.to_owned(), name.to_owned()))
            })
            .filter(|(_, name)| self.fixture.spells(name) || self.asks_for(name))
            .collect();
        offered.sort();
        let mut names: Vec<String> = offered.into_iter().map(|(_, name)| name).collect();
        names.dedup();
        names
    }

    /// Whether the case's expected answer lists `name` on a line of its own.
    fn asks_for(&self, name: &str) -> bool {
        self.expected.lines().any(|line| line.trim() == name)
    }

    /// Where the identifier a rename would change starts.
    fn rename_target(&self, answer: &Value) -> Vec<String> {
        let range = answer.get("range").unwrap_or(answer);
        if range.get("start").is_none() {
            return Vec::new();
        }
        let place = Place::new(format!("/{}", self.fixture.cursor.path), range);
        vec![self.show(&place)]
    }

    /// Where each text edit of a rename starts, as [`Self::locations`] shows
    /// a place: the line is the one before the rename. Files the rename
    /// would move are not shown.
    fn edited_places(&self, answer: &Value) -> Vec<String> {
        let mut places = Vec::new();
        if let Some(changes) = answer["changes"].as_object() {
            for (uri, edits) in changes {
                places.extend(self.edit_places(uri, edits));
            }
        }
        for change in as_list(&answer["documentChanges"]) {
            if let Some(uri) = change["textDocument"]["uri"].as_str() {
                places.extend(self.edit_places(uri, &change["edits"]));
            }
        }
        self.in_source_order(places)
    }

    /// One place per identifier an edit renames. An edit may span several
    /// names with the text between them kept (jdtls answers a Java rename
    /// with one such edit per file), so the names it changes are found by
    /// comparing the text it replaces with the text it writes.
    fn edit_places(&self, uri: &str, edits: &Value) -> Vec<Place> {
        let path = self.session.display(uri);
        let mut places = Vec::new();
        for edit in as_list(edits) {
            let Some(range) = edit.get("range") else {
                continue;
            };
            let start = Place::new(path.clone(), range);
            let end = Place::new(path.clone(), &json!({"start": range["end"]}));
            let replaced = self.fixture_text(&path, &start, &end);
            let written = edit["newText"].as_str().unwrap_or_default();
            match replaced.and_then(|replaced| renamed_words(&start, &replaced, written)) {
                Some(renamed) if !renamed.is_empty() => places.extend(renamed),
                _ => places.push(start),
            }
        }
        places
    }

    /// The fixture's text of `path` from `start` to `end`, lines joined by `\n`.
    fn fixture_text(&self, path: &str, start: &Place, end: &Place) -> Option<String> {
        let mut text = String::new();
        for line_index in start.line..=end.line {
            let line: Vec<char> = self.fixture.line(path, line_index)?.chars().collect();
            let from = if line_index == start.line {
                start.column
            } else {
                0
            };
            let to = if line_index == end.line {
                end.column
            } else {
                line.len()
            };
            if line_index != start.line {
                text.push('\n');
            }
            text.extend(line.get(from..to)?);
        }
        Some(text)
    }

    /// `name  /path:line` of the types found in the fixture's files, sorted.
    /// Whether fields and methods are searchable too differs by server.
    fn workspace_types(&self, answer: &Value) -> Vec<String> {
        let mut lines: Vec<String> = as_list(answer)
            .into_iter()
            .filter(|symbol| matches!(symbol["kind"].as_u64(), Some(5 | 10 | 11)))
            .filter_map(|symbol| {
                let path = self.session.display(symbol["location"]["uri"].as_str()?);
                let line = symbol["location"]["range"]["start"]["line"].as_u64()?;
                self.fixture.line(&path, line as usize)?;
                let name = identifier_prefix(symbol["name"].as_str()?);
                Some(format!("{name}  {path}:{}", line + 1))
            })
            .collect();
        lines.sort();
        lines.dedup();
        lines
    }
}

/// A zero-based position in a file, ordered as the source reads.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    path: String,
    line: usize,
    column: usize,
}

impl Place {
    fn new(path: String, range: &Value) -> Self {
        let start = &range["start"];
        Self {
            path,
            line: start["line"].as_u64().unwrap_or(0) as usize,
            column: start["character"].as_u64().unwrap_or(0) as usize,
        }
    }
}

/// Where the identifiers that differ between `replaced` (the text an edit
/// starting at `start` replaces) and `written` start. `None` when the two
/// are not the same text with some identifiers swapped.
fn renamed_words(start: &Place, replaced: &str, written: &str) -> Option<Vec<Place>> {
    let replaced = words(replaced);
    let written = words(written);
    if replaced.len() != written.len() {
        return None;
    }
    let mut places = Vec::new();
    for ((token, line, column), (new_token, _, _)) in replaced.iter().zip(&written) {
        if token == new_token {
            continue;
        }
        let is_identifier = token.chars().all(is_word_character);
        if !is_identifier {
            return None;
        }
        let column = if *line == 0 {
            start.column + column
        } else {
            *column
        };
        places.push(Place {
            path: start.path.clone(),
            line: start.line + line,
            column,
        });
    }
    Some(places)
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// `text` cut into runs of word characters and single other characters,
/// each with the line and column (in characters) it starts at.
fn words(text: &str) -> Vec<(String, usize, usize)> {
    let mut tokens: Vec<(String, usize, usize)> = Vec::new();
    let (mut line, mut column) = (0, 0);
    let mut in_word = false;
    for character in text.chars() {
        let is_word = is_word_character(character);
        match tokens.last_mut() {
            Some((token, _, _)) if is_word && in_word => token.push(character),
            _ => tokens.push((character.to_string(), line, column)),
        }
        in_word = is_word;
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    tokens
}

/// `scaled` of `scaled(double, int) : Circle`.
/// The dotted name a completion label starts with: `com.demo` of a package,
/// `put` of `put(K key, V value)`.
fn qualified_name_prefix(label: &str) -> &str {
    let end = label
        .find(|character: char| !character.is_alphanumeric() && !matches!(character, '_' | '.'))
        .unwrap_or(label.len());
    let name = label[..end].trim_end_matches('.');
    // A nested type is `IFoo` to one server and `Foo.IFoo` to another; a
    // package (`java.util`) is lower case and kept whole.
    let nested = name.contains('.')
        && name
            .split('.')
            .all(|segment| segment.starts_with(|character: char| character.is_uppercase()));
    if nested {
        return name.rsplit('.').next().unwrap_or(name);
    }
    name
}

fn identifier_prefix(label: &str) -> &str {
    let end = label
        .find(|character: char| !character.is_alphanumeric() && character != '_')
        .unwrap_or(label.len());
    &label[..end]
}

/// The first line of code a hover shows, with `name` unqualified: one
/// server writes `void print(String)`, another `void Printer.print(String)`.
/// A constructor is named after its class — `Printer.Printer(String)` — and
/// is shown as `Printer(String)`.
fn hover_signature(answer: &Value, name: &str) -> Vec<String> {
    let text = as_list(&answer["contents"])
        .into_iter()
        .filter_map(|part| part.as_str().or_else(|| part["value"].as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let signature = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"));
    signature
        .map(|line| vec![unqualified(line, name)])
        .unwrap_or_default()
}

/// `line` without the dotted qualifier in front of its first `name`.
fn unqualified(line: &str, name: &str) -> String {
    let Some(mut at) = line.find(name) else {
        return line.to_owned();
    };
    let constructor_of_name = format!("{name}.{name}");
    if !name.is_empty() && line[at..].starts_with(&constructor_of_name) {
        at += name.len() + 1;
    }
    let qualifier_start = line[..at]
        .rfind(|character: char| !character.is_alphanumeric() && !matches!(character, '_' | '.'))
        .map_or(0, |separator| separator + 1);
    format!("{}{}", &line[..qualifier_start], &line[at..])
}

/// `name(parameter, [active parameter])` of the active signature.
fn signature(answer: &Value) -> Vec<String> {
    let active_signature = answer["activeSignature"].as_u64().unwrap_or(0) as usize;
    let Some(signature) = answer["signatures"].get(active_signature) else {
        return Vec::new();
    };
    let label = signature["label"].as_str().unwrap_or_default();
    let active_parameter = signature["activeParameter"]
        .as_u64()
        .or_else(|| answer["activeParameter"].as_u64());
    let parameters: Vec<String> = as_list(&signature["parameters"])
        .into_iter()
        .enumerate()
        .map(|(index, parameter)| {
            let text = parameter["label"].as_str().unwrap_or("?");
            if active_parameter == Some(index as u64) {
                format!("[{text}]")
            } else {
                text.to_owned()
            }
        })
        .collect();
    vec![format!(
        "{}({})",
        identifier_prefix(label),
        parameters.join(", ")
    )]
}

const PACKAGE: u64 = 4;

fn is_java(path: &str) -> bool {
    path.ends_with(".java")
}

/// A Java file's outline as jdtls gives it: a tree, each symbol as
/// `kind name detail  range at selection`, nested two spaces under its
/// parent, and `deprecated` where it is tagged so.
fn java_outline(answer: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    outline_lines(answer, 0, &mut lines);
    lines
}

fn outline_lines(symbols: &Value, depth: usize, lines: &mut Vec<String>) {
    let mut symbols = as_list(symbols);
    symbols.sort_by_key(|symbol| start_of(&symbol["range"]));
    for symbol in symbols {
        let deprecated = symbol["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_u64() == Some(1)))
            || symbol["deprecated"].as_bool() == Some(true);
        lines.push(format!(
            "{}{} {}{}  {} at {}{}",
            "  ".repeat(depth),
            symbol_kind(symbol["kind"].as_u64()),
            symbol["name"].as_str().unwrap_or("?"),
            symbol["detail"].as_str().unwrap_or_default(),
            span(&symbol["range"]),
            span(&symbol["selectionRange"]),
            if deprecated { "  deprecated" } else { "" },
        ));
        outline_lines(&symbol["children"], depth + 1, lines);
    }
}

fn start_of(range: &Value) -> (u64, u64) {
    let start = &range["start"];
    (
        start["line"].as_u64().unwrap_or(0),
        start["character"].as_u64().unwrap_or(0),
    )
}

/// `line:column-end`, one-based, `end` written as `line:column` when the
/// range goes over lines.
fn span(range: &Value) -> String {
    let (line, column) = start_of(range);
    let end = &range["end"];
    let (end_line, end_column) = (
        end["line"].as_u64().unwrap_or(0),
        end["character"].as_u64().unwrap_or(0),
    );
    if end_line == line {
        format!("{}:{}-{}", line + 1, column + 1, end_column + 1)
    } else {
        format!(
            "{}:{}-{}:{}",
            line + 1,
            column + 1,
            end_line + 1,
            end_column + 1
        )
    }
}

/// The outline in source order: `kind name`. Nesting is not shown, nor the
/// package — one server answers a tree under the package and another a flat
/// list of the same declarations.
fn document_symbols(answer: &Value) -> Vec<String> {
    let mut symbols = Vec::new();
    flatten_symbols(answer, &mut symbols);
    symbols.sort_by_key(|symbol| {
        let start = symbol
            .get("selectionRange")
            .unwrap_or(&symbol["location"]["range"])["start"]
            .clone();
        (start["line"].as_u64(), start["character"].as_u64())
    });
    symbols
        .into_iter()
        .map(|symbol| {
            let name = identifier_prefix(symbol["name"].as_str().unwrap_or("?"));
            format!("{} {name}", symbol_kind(symbol["kind"].as_u64()))
        })
        .collect()
}

fn flatten_symbols<'a>(symbols: &'a Value, flat: &mut Vec<&'a Value>) {
    for symbol in as_list(symbols) {
        if symbol["kind"].as_u64() != Some(PACKAGE) {
            flat.push(symbol);
        }
        flatten_symbols(&symbol["children"], flat);
    }
}

fn symbol_kind(kind: Option<u64>) -> &'static str {
    match kind {
        Some(4) => "package",
        Some(5) => "class",
        Some(6) => "method",
        Some(8) => "field",
        Some(9) => "constructor",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(14) => "constant",
        Some(22) => "enum-member",
        _ => "symbol",
    }
}

/// `first..last` line of each folded region, one-based, sorted. For Java
/// a region's kind follows, when it has one: `3..5 comment`.
fn folded_lines(answer: &Value, with_kind: bool) -> Vec<String> {
    let mut regions: Vec<(u64, u64, &str)> = as_list(answer)
        .into_iter()
        .filter_map(|range| {
            let kind = range["kind"]
                .as_str()
                .filter(|_| with_kind)
                .unwrap_or_default();
            Some((
                range["startLine"].as_u64()?,
                range["endLine"].as_u64()?,
                kind,
            ))
        })
        .collect();
    regions.sort_unstable();
    regions
        .into_iter()
        .map(|(start, end, kind)| {
            format!("{}..{} {kind}", start + 1, end + 1)
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The prepared items of a call or type hierarchy, and for the first one
/// the answer of each of `directions`, as
/// `{"items": [...], "<direction>": [...]}`.
async fn hierarchy(
    session: &mut Session,
    prepared: &Value,
    prefix: &str,
    directions: [&str; 2],
) -> Value {
    let mut answer = json!({"items": prepared.clone()});
    if let Some(item) = as_list(prepared).first().copied().cloned() {
        for direction in directions {
            let found = session
                .request_until(
                    &format!("{prefix}/{direction}"),
                    json!({"item": item}),
                    OnError::AnswerNothing,
                    |_| true,
                )
                .await;
            answer[direction] = found;
        }
    }
    answer
}

/// The title of each action, sorted: which comes first is up to the
/// client.
fn action_titles(answer: &Value) -> Vec<String> {
    let mut titles: Vec<String> = as_list(answer)
        .into_iter()
        .filter_map(|action| Some(action["title"].as_str()?.to_owned()))
        .collect();
    titles.sort();
    titles.dedup();
    titles
}

/// `line:column  label` of each hint, one-based, in source order.
fn inlay_hints(answer: &Value) -> Vec<String> {
    let mut hints: Vec<(u64, u64, String)> = as_list(answer)
        .into_iter()
        .filter_map(|hint| {
            let label = match &hint["label"] {
                Value::String(text) => text.clone(),
                parts => as_list(parts)
                    .into_iter()
                    .filter_map(|part| part["value"].as_str())
                    .collect(),
            };
            let position = &hint["position"];
            Some((
                position["line"].as_u64()?,
                position["character"].as_u64()?,
                label,
            ))
        })
        .collect();
    hints.sort();
    hints
        .into_iter()
        .map(|(line, column, label)| format!("{}:{}  {}", line + 1, column + 1, label.trim()))
        .collect()
}

/// The ranges around the cursor, innermost first, as `first:column..last:column`.
fn selection_ranges(answer: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let mut range = as_list(answer).first().copied();
    while let Some(selection) = range {
        let (start, end) = (&selection["range"]["start"], &selection["range"]["end"]);
        lines.push(format!(
            "{}:{}..{}:{}",
            start["line"].as_u64().unwrap_or(0) + 1,
            start["character"].as_u64().unwrap_or(0) + 1,
            end["line"].as_u64().unwrap_or(0) + 1,
            end["character"].as_u64().unwrap_or(0) + 1,
        ));
        range = selection.get("parent").filter(|parent| !parent.is_null());
    }
    lines
}

impl View<'_> {
    /// The cursor's file once the edits are applied, line by line.
    fn formatted(&self, answer: &Value) -> Vec<String> {
        let edits = as_list(answer);
        if edits.is_empty() {
            return Vec::new();
        }
        let path = &self.fixture.cursor.path;
        let text = self.fixture.text(path).unwrap_or_default();
        apply_edits(text, &edits)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `/path:line:column  source line` of each lens, with the title of its
    /// command when the server already resolved it.
    fn lenses(&self, answer: &Value) -> Vec<String> {
        let path = format!("/{}", self.fixture.cursor.path);
        let mut lenses: Vec<(Place, String)> = as_list(answer)
            .into_iter()
            .filter_map(|lens| {
                let place = Place::new(path.clone(), lens.get("range")?);
                let title = lens["command"]["title"].as_str().unwrap_or_default();
                Some((place, title.to_owned()))
            })
            .collect();
        lenses.sort();
        lenses
            .iter()
            .map(|(place, title)| {
                let shown = self.show(place);
                if title.is_empty() {
                    shown
                } else {
                    format!("{shown}  [{title}]")
                }
            })
            .collect()
    }

    /// `item`, then `incomingCalls` / `outgoingCalls` (or `supertypes` /
    /// `subtypes`): `name  /path:line:column  source line`, each group in
    /// source order.
    fn hierarchy(&self, answer: &Value) -> Vec<String> {
        let mut lines = Vec::new();
        let show = |item: &Value| -> Option<String> {
            let uri = item["uri"].as_str()?;
            let range = item.get("selectionRange").or_else(|| item.get("range"))?;
            let place = Place::new(self.session.display(uri), range);
            Some(format!("{}  {}", item["name"].as_str()?, self.show(&place)))
        };
        for (group, key, inner) in [
            ("item", "items", None),
            ("incoming", "incomingCalls", Some("from")),
            ("outgoing", "outgoingCalls", Some("to")),
            ("supertype", "supertypes", None),
            ("subtype", "subtypes", None),
        ] {
            let mut shown: Vec<String> = as_list(&answer[key])
                .into_iter()
                .filter_map(|entry| show(inner.map_or(entry, |field| &entry[field])))
                .map(|line| format!("{group} {line}"))
                .collect();
            shown.sort();
            lines.extend(shown);
        }
        lines
    }

    /// `line:column  text  type [modifiers]` of each token, decoded with the
    /// legend the server gave in `initialize`.
    fn semantic_tokens(&self, answer: &Value) -> Vec<String> {
        let legend = self.session.semantic_legend();
        let types = as_list(&legend["tokenTypes"]);
        let modifiers = as_list(&legend["tokenModifiers"]);
        let data: Vec<u64> = as_list(&answer["data"])
            .into_iter()
            .filter_map(Value::as_u64)
            .collect();
        let path = format!("/{}", self.fixture.cursor.path);
        let (mut line, mut column) = (0, 0);
        let mut lines = Vec::new();
        for token in data.chunks_exact(5) {
            if token[0] > 0 {
                line += token[0];
                column = token[1];
            } else {
                column += token[1];
            }
            let source: Vec<u16> = self
                .fixture
                .line(&path, line as usize)
                .unwrap_or_default()
                .encode_utf16()
                .collect();
            let start = (column as usize).min(source.len());
            let end = (start + token[2] as usize).min(source.len());
            let text = String::from_utf16_lossy(&source[start..end]);
            let kind = types
                .get(token[3] as usize)
                .and_then(|kind| kind.as_str())
                .unwrap_or("?");
            let set: Vec<&str> = modifiers
                .iter()
                .enumerate()
                .filter(|(bit, _)| token[4] & (1 << bit) != 0)
                .filter_map(|(_, modifier)| modifier.as_str())
                .collect();
            let mut shown = format!("{}:{}  {text}  {kind}", line + 1, column + 1);
            if !set.is_empty() {
                shown.push_str(&format!(" [{}]", set.join(", ")));
            }
            lines.push(shown);
        }
        lines
    }
}

/// `text` with the LSP `edits` applied; positions count UTF-16 units.
fn apply_edits(text: &str, edits: &[&Value]) -> String {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let offset = |position: &Value| -> usize {
        let line = position["line"].as_u64().unwrap_or(0) as usize;
        let Some(&start) = line_starts.get(line) else {
            return text.len();
        };
        let rest = &text[start..];
        let mut units = position["character"].as_u64().unwrap_or(0) as usize;
        let mut end = 0;
        for character in rest.chars() {
            if units == 0 || character == '\n' {
                break;
            }
            units = units.saturating_sub(character.len_utf16());
            end += character.len_utf8();
        }
        start + end
    };
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            (
                offset(&edit["range"]["start"]),
                offset(&edit["range"]["end"]),
                edit["newText"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    spans.sort_by_key(|(start, end, _)| std::cmp::Reverse((*start, *end)));
    let mut result = text.to_owned();
    for (start, end, new_text) in spans {
        result.replace_range(start..end.max(start), new_text);
    }
    result
}
